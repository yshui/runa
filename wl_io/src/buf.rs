use crate::{AsyncBufReadExt, AsyncReadWithFds, AsyncWriteWithFds};
use futures_lite::{ready, AsyncBufRead, AsyncRead, AsyncWrite};
use pin_project_lite::pin_project;
use std::{
    collections::VecDeque, os::unix::prelude::RawFd, pin::Pin, task::Poll,
};

pin_project! {
/// A buffered reader for reading data with file descriptors.
///
/// #Note
///
/// Because of the special treatment of file descriptors, i.e. they are closed if we don't call
/// `recvmsg` with a big enough buffer, so every time we read, we have to read all of them, whehter
/// there are spare buffer left or not. This means the file descriptors buffer will grow
/// indefinitely if they are not read from BufWithFd.
///
/// Also, users are encouraged to use up all the available data before calling
/// poll_fill_buf/poll_fill_buf_until again, otherwise there is potential for causing a lot of
/// allocations and memcpys.
pub struct BufReaderWithFd<T> {
    #[pin]
    inner: T,
    buf: Vec<u8>,
    cap_data: usize,
    filled_data: usize,
    pos_data: usize,

    fd_buf: VecDeque<RawFd>,
}
}

impl<T> BufReaderWithFd<T> {
    #[inline]
    pub fn new(inner: T) -> Self {
        Self::with_capacity(inner, 4 * 1024, 32)
    }
    #[inline]
    pub fn shrink(self: Pin<&mut Self>) {
        // We have something to do if either:
        // 1. pos_data > 0 - we can move data to the front
        // 2. buf.len() > filled_data, and buf.len() > cap_data - we can shrink the buffer down to
        //    filled_data or cap_data
        if self.pos_data > 0 || self.buf.len() > std::cmp::max(self.filled_data, self.cap_data) {
            let this = self.project();
            let data_len = *this.filled_data - *this.pos_data;
            // Safety: pos_data and filled_data are valid indices. u8 is Copy and !Drop
            unsafe {
                std::ptr::copy(
                    this.buf[*this.pos_data..].as_ptr(),
                    this.buf.as_mut_ptr(),
                    data_len,
                )
            };
            this.buf.truncate(std::cmp::max(data_len, *this.cap_data));
            this.buf.shrink_to_fit();
            *this.pos_data = 0;
            *this.filled_data = data_len;
        }
    }
    #[inline]
    pub fn with_capacity(inner: T, cap_data: usize, cap_fd: usize) -> Self {
        // TODO: consider using box::new_uninit_slice when #63291 is stablized
        // Actually, we can't use MaybeUninit here, AsyncRead::poll_read has no guarantee that it
        // will definitely initialize the number of bytes it claims to have read. That's why tokio
        // uses a ReadBuf type track how many bytes have been initialized.
        Self {
            inner,
            buf: vec![0; cap_data],
            filled_data: 0,
            pos_data: 0,
            cap_data,

            fd_buf: VecDeque::with_capacity(cap_fd),
        }
    }

    #[inline]
    fn consume_with_fds(self: Pin<&mut Self>, amt_data: usize, amt_fd: usize) {
        let this = self.project();
        *this.pos_data += amt_data;

        // Has to drop the Drain iterator to prevent leaking
        drop(this.fd_buf.drain(..amt_fd));
    }

    #[inline]
    fn buffer(&self) -> &[u8] {
        let range = self.pos_data..self.filled_data;
        // Safety: invariant: filled_data <= buf.len()
        unsafe { self.buf.get_unchecked(range) }
    }
}

impl<T: AsyncReadWithFds> AsyncRead for BufReaderWithFd<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(ready!(self.poll_read_with_fds(cx, buf, &mut [])).map(|(n, _)| n))
    }
}

impl<T: AsyncReadWithFds> AsyncBufReadExt for BufReaderWithFd<T> {
    fn poll_fill_buf_until(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        len: usize,
    ) -> Poll<std::io::Result<&[u8]>> {
        if self.pos_data + len > self.buf.len() || self.filled_data == self.pos_data {
            // Try to shrink before we grow it. Or if the buf is empty.
            self.as_mut().shrink();
        }
        while self.filled_data - self.pos_data < len {
            let this = self.as_mut().project();
            if this.filled_data == this.pos_data {
                *this.filled_data = 0;
                *this.pos_data = 0;
            }
            if *this.pos_data + len > this.buf.len() {
                this.buf.resize(len + *this.pos_data, 0);
            }

            let mut tmp_fds = [0; crate::SCM_MAX_FD];
            // Safety: loop invariant: filled_data < len + pos_data
            // post condition from the if above: buf.len() >= len + pos_data
            // combined: filled_data < buf.len()
            let buf = unsafe { &mut this.buf.get_unchecked_mut(*this.filled_data..) };
            let (bytes, nfds) = ready!(this.inner.poll_read_with_fds(cx, buf, &mut tmp_fds))?;
            *this.filled_data += bytes;
            this.fd_buf.extend(tmp_fds[..nfds].iter());
        }

        Poll::Ready(Ok(self.into_ref().get_ref().buffer()))
    }
}

impl<T: AsyncReadWithFds> AsyncBufRead for BufReaderWithFd<T> {
    fn poll_fill_buf(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<&[u8]>> {
        // Fill at least 1 byte
        self.poll_fill_buf_until(cx, 1)
    }
    fn consume(self: Pin<&mut Self>, amt: usize) {
        self.consume_with_fds(amt, 0);
    }
}

impl<T: AsyncReadWithFds> AsyncReadWithFds for BufReaderWithFd<T> {
    fn poll_read_with_fds(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        mut buf: &mut [u8],
        mut fds: &mut [RawFd],
    ) -> Poll<std::io::Result<(usize, usize)>> {
        let our_buf = ready!(self.as_mut().poll_fill_buf(cx))?;
        let read_len = std::cmp::min(our_buf.len(), buf.len());
        buf[..read_len].copy_from_slice(&our_buf[..read_len]);
        buf = &mut buf[read_len..];

        let nfds = std::cmp::min(self.fd_buf.len(), fds.len());
        for (i, fd) in fds.iter_mut().take(nfds).enumerate() {
            *fd = self.fd_buf[i];
        }
        fds = &mut fds[nfds..];

        self.as_mut().consume_with_fds(read_len, nfds);

        let mut read = read_len;
        let mut fds_read = nfds;
        if !buf.is_empty() {
            // If we still have buffer left, we try to read directly into the buffer to
            // opportunistically avoid copying.
            //
            // If `poll_read_with_fds` returns `Poll::Pending`, next time this function is called
            // we will fill our buffer instead re-entering this if branch.
            let this = self.project();
            let mut tmp_fds = [0; crate::SCM_MAX_FD];
            let mut tmp_fds = &mut tmp_fds[..];
            match this.inner.poll_read_with_fds(cx, buf, tmp_fds)? {
                Poll::Ready((bytes, nfds)) => {
                    tmp_fds = &mut tmp_fds[..nfds];
                    read += bytes;
                    let nfds_to_copy = std::cmp::min(nfds, fds.len());
                    if nfds_to_copy > 0 {
                        fds[..nfds_to_copy].copy_from_slice(&tmp_fds[..nfds_to_copy]);
                        tmp_fds = &mut tmp_fds[nfds_to_copy..];
                        fds_read += nfds_to_copy;
                    }
                    this.fd_buf.extend(tmp_fds.iter());
                }
                Poll::Pending => {} // This is fine - we already read data.
            }
        }

        Poll::Ready(Ok((read, fds_read)))
    }
}

pin_project! {
pub struct BufWriterWithFd<T> {
    #[pin]
    inner: T,
    buf: Vec<u8>,
    fd_buf: Vec<RawFd>,
    written: usize,
}
}

impl<T> BufWriterWithFd<T> {
    pub fn with_capacity(inner: T, cap: usize, cap_fd: usize) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(cap),
            fd_buf: Vec::with_capacity(cap_fd),
            written: 0,
        }
    }
    pub fn new(inner: T) -> Self {
        Self::with_capacity(inner, 4 * 1024, 32)
    }
}

impl<T: AsyncWriteWithFds> BufWriterWithFd<T> {
    fn poll_flush_buf(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let mut this = self.as_mut().project();
        while *this.written < this.buf.len() {
            match ready!(this.inner.as_mut().poll_write_with_fds(
                cx,
                &this.buf[*this.written..],
                this.fd_buf
            )) {
                Ok(0) => {
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "failed to write whole buffer",
                    )))
                }
                Ok(written) => {
                    this.fd_buf.clear();
                    *this.written += written;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
        this.buf.clear();
        *this.written = 0;
        Poll::Ready(Ok(()))
    }
}

impl<T: AsyncWriteWithFds> AsyncWrite for BufWriterWithFd<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.poll_write_with_fds(cx, buf, &[])
    }
    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        ready!(self.as_mut().poll_flush_buf(cx))?;
        self.project().inner.poll_flush(cx)
    }
    fn poll_close(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        ready!(self.as_mut().poll_flush(cx))?;
        self.project().inner.poll_close(cx)
    }
}

impl<T: AsyncWriteWithFds> AsyncWriteWithFds for BufWriterWithFd<T> {
    fn poll_write_with_fds(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
        fds: &[RawFd],
    ) -> Poll<std::io::Result<usize>> {
        if self.buf.len() + buf.len() > self.buf.capacity()
            || self.fd_buf.len() + fds.len() > self.fd_buf.capacity()
        {
            ready!(self.as_mut().poll_flush_buf(cx))?;
        }

        if buf.len() >= self.buf.capacity() || fds.len() >= self.fd_buf.capacity() {
            self.as_mut().poll_write_with_fds(cx, buf, fds)
        } else {
            let this = self.project();
            this.buf.extend_from_slice(buf);
            this.fd_buf.extend_from_slice(fds);
            Poll::Ready(Ok(buf.len()))
        }
    }
}
