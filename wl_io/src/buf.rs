use crate::{AsyncReadWithFds, AsyncWriteWithFds};
use futures_lite::{ready, AsyncBufRead, AsyncRead, AsyncWrite};
use pin_project_lite::pin_project;
use std::{
    collections::VecDeque, mem::MaybeUninit, os::unix::prelude::RawFd, pin::Pin, task::Poll,
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
pub struct BufReaderWithFd<T> {
    #[pin]
    inner: T,
    buf: Box<[MaybeUninit<u8>]>,
    filled_data: usize,
    pos_data: usize,

    fd_buf: VecDeque<RawFd>,
}
}

impl<T> BufReaderWithFd<T> {
    pub fn new(inner: T) -> Self {
        Self::with_capacity(inner, 4 * 1024, 32)
    }
    pub fn with_capacity(inner: T, cap_data: usize, cap_fd: usize) -> Self {
        // TODO: consider using box::new_uninit_slice when #63291 is stablized
        Self {
            inner,
            buf: vec![MaybeUninit::uninit(); cap_data].into_boxed_slice(),
            filled_data: 0,
            pos_data: 0,

            fd_buf: VecDeque::with_capacity(cap_fd),
        }
    }

    fn consume_with_fds(self: Pin<&mut Self>, amt_data: usize, amt_fd: usize) {
        let this = self.project();
        *this.pos_data += amt_data;

        // Has to drop the Drain iterator to prevent leaking
        drop(this.fd_buf.drain(..amt_fd));
    }
    fn buffer(self: Pin<&mut Self>) -> &[u8] {
        unsafe {
            let buf = self.buf.get_unchecked(self.pos_data..self.filled_data);
            &*(buf as *const [MaybeUninit<u8>] as *const [u8])
        }
    }
}

impl<T: AsyncReadWithFds> AsyncRead for BufReaderWithFd<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        mut buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(ready!(self.poll_read_with_fds(cx, &mut buf, &mut [])).map(|(n, _)| n))
    }
}

impl<T: AsyncReadWithFds> AsyncBufRead for BufReaderWithFd<T> {
    fn poll_fill_buf(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<&[u8]>> {
        use std::ops::DerefMut;
        if self.pos_data >= self.filled_data {
            let this = self.as_mut().project();
            *this.filled_data = 0;
            *this.pos_data = 0;

            loop {
                let this = self.as_mut().project();
                let mut tmp_fds = [0; crate::SCM_MAX_FD];
                let (bytes, nfds) = unsafe {
                    let buf = &mut *(this.buf.deref_mut() as *mut [MaybeUninit<u8>] as *mut [u8]);
                    ready!(this.inner.poll_read_with_fds(cx, buf, &mut tmp_fds))?
                };
                *this.filled_data = bytes;
                *this.pos_data = 0;
                this.fd_buf.extend(tmp_fds[..nfds].iter());
                if *this.filled_data > 0 {
                    break;
                }
            }
        }

        Poll::Ready(Ok(self.buffer()))
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
        for i in 0..nfds {
            fds[i] = self.fd_buf[i];
        }
        fds = &mut fds[nfds..];

        self.as_mut().consume_with_fds(read_len, nfds);

        let mut read = read_len;
        let mut fds_read = nfds;
        if buf.len() > 0 {
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
                &this.fd_buf
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
