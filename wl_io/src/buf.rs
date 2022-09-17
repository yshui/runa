use crate::{AsyncReadWithFd, AsyncWriteWithFd};
use futures_lite::{ready, AsyncBufRead, AsyncRead, AsyncWrite};
use pin_project_lite::pin_project;
use std::io::Result;
use std::{
    collections::VecDeque,
    mem::MaybeUninit,
    os::unix::prelude::RawFd,
    pin::Pin,
    ptr::NonNull,
    task::{Context, Poll},
};

pub trait AsyncBufReadWithFd: AsyncReadWithFd {
    /// Reads enough data to return a buffer at least the given size.
    fn poll_fill_buf_until<'a>(
        self: Pin<&'a mut Self>,
        cx: &mut Context<'_>,
        len: usize,
    ) -> Poll<Result<(&'a [u8], &'a [RawFd])>>;
    fn consume_with_fds(self: Pin<&mut Self>, amt_data: usize, amt_fd: usize);
}

pub struct FillBufUtil<'a, R: Unpin + ?Sized>(Option<&'a mut R>, usize);

impl<'a, R: AsyncBufReadWithFd + Unpin> ::std::future::Future for FillBufUtil<'a, R> {
    type Output = Result<(&'a [u8], &'a [RawFd])>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        let len = this.1;
        let inner = this.0.take().expect("FillBufUtil polled after completion");
        match Pin::new(&mut *inner).poll_fill_buf_until(cx, len) {
            Poll::Pending => {
                this.0 = Some(inner);
                Poll::Pending
            }
            Poll::Ready(Ok(_)) => match Pin::new(inner).poll_fill_buf_until(cx, len) {
                Poll::Ready(Ok(v)) => Poll::Ready(Ok(v)),
                other => panic!(
                    "poll_fill_buf_until returned {:?} after returning Ready",
                    other
                ),
            },
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
        }
    }
}

pub trait AsyncBufReadWithFdExt: AsyncBufReadWithFd {
    fn fill_buf_until<'a>(self: &'a mut Self, len: usize) -> FillBufUtil<'a, Self>
    where
        Self: Unpin,
    {
        FillBufUtil(Some(self), len)
    }
}

impl<T: AsyncBufReadWithFd> AsyncBufReadWithFdExt for T {}

/// A wrapper around a byte buffer that is incrementally filled and initialized.
///
/// Taken from tokio, see tokio::io::ReadBuf
pub struct WriteBuf<'a> {
    buf: &'a mut [MaybeUninit<u8>],
    filled: usize,
    fds: &'a mut [MaybeUninit<RawFd>],
    filled_fd: usize,
}

fn slice_to_uninit_mut<T>(slice: &mut [T]) -> &mut [MaybeUninit<T>] {
    unsafe { &mut *(slice as *mut [T] as *mut [MaybeUninit<T>]) }
}

impl<'a> WriteBuf<'a> {
    pub fn from_spare(buf: &'a mut Vec<u8>, fds: &'a mut Vec<RawFd>) -> Self {
        Self {
            buf: buf.spare_capacity_mut(),
            fds: fds.spare_capacity_mut(),
            filled: 0,
            filled_fd: 0,
        }
    }
    pub fn new(buf: &'a mut [u8], fds: &'a mut [RawFd]) -> Self {
        Self {
            buf: slice_to_uninit_mut(buf),
            fds: slice_to_uninit_mut(fds),
            filled: 0,
            filled_fd: 0,
        }
    }
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.filled
    }
    pub fn remaining_fds(&self) -> usize {
        self.fds.len() - self.filled_fd
    }
    pub fn filled(&self) -> usize {
        self.filled
    }
    pub fn filled_fd(&self) -> usize {
        self.filled_fd
    }
    pub fn put_slice(&mut self, buf: &[u8]) {
        assert!(
            self.remaining() >= buf.len(),
            "WriteBuf::put_slice: not enough space"
        );
        let amt = buf.len();
        let end = self.filled + amt;
        // Safety: length checked above, and we never read from the ptr.
        unsafe {
            self.buf[self.filled..end]
                .as_mut_ptr()
                .cast::<u8>()
                .copy_from_nonoverlapping(buf.as_ptr(), amt);
        }
        self.filled = end;
    }
    pub fn put_fds(&mut self, fds: &[RawFd]) {
        assert!(
            self.remaining_fds() >= fds.len(),
            "WriteBuf::put_fds: not enough space"
        );
        let amt = fds.len();
        let end = self.filled_fd + amt;
        // Safety: length checked above, and we never read from the ptr.
        unsafe {
            self.fds[self.filled_fd..end]
                .as_mut_ptr()
                .cast::<RawFd>()
                .copy_from_nonoverlapping(fds.as_ptr(), amt);
        }
        self.filled_fd = end;
    }
}

pub trait AsyncBufWriteWithFd: AsyncWriteWithFd {
    /// Waits until there are at least `len` bytes available in the buffer. Then call a callback to
    /// write data into the reserved buffer. Implementation should update the internal buffer
    /// length according to how many bytes are written into WriteBuf. write_buf.remaining() must be
    /// greater or equal to `len` when the callback is called.
    /// Implementation shold first try to flush the buffer, until enough free space is available.
    /// If buffer is not big enough after a complete flush, it should allocate more space.
    fn poll_write_to_reserve<'a>(
        self: Pin<&'a mut Self>,
        cx: &mut Context<'_>,
        demand: usize,
        demand_fd: usize,
        f: impl FnOnce(&mut WriteBuf<'_>) -> std::io::Result<()>,
    ) -> Poll<Result<()>>;
}

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
#[derive(Debug)]
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
    fn buffer(&self) -> &[u8] {
        let range = self.pos_data..self.filled_data;
        // Safety: invariant: filled_data <= buf.len()
        unsafe { self.buf.get_unchecked(range) }
    }
}

impl<T: AsyncReadWithFd> AsyncRead for BufReaderWithFd<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(ready!(self.poll_read_with_fds(cx, buf, &mut [])).map(|(n, _)| n))
    }
}

impl<T: AsyncReadWithFd> AsyncBufReadWithFd for BufReaderWithFd<T> {
    fn poll_fill_buf_until(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        len: usize,
    ) -> Poll<std::io::Result<(&[u8], &[RawFd])>> {
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

        self.as_mut().project().fd_buf.make_contiguous();
        let mut this = self.into_ref().get_ref();
        Poll::Ready(Ok((this.buffer(), this.fd_buf.as_slices().0)))
    }

    #[inline]
    fn consume_with_fds(self: Pin<&mut Self>, amt_data: usize, amt_fd: usize) {
        let this = self.project();
        *this.pos_data += amt_data;

        // Has to drop the Drain iterator to prevent leaking
        drop(this.fd_buf.drain(..amt_fd));
    }
}

impl<T: AsyncReadWithFd> AsyncBufRead for BufReaderWithFd<T> {
    fn poll_fill_buf(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<&[u8]>> {
        // Fill at least 1 byte
        Poll::Ready(Ok(ready!(self.poll_fill_buf_until(cx, 0))?.0))
    }
    fn consume(self: Pin<&mut Self>, amt: usize) {
        self.consume_with_fds(amt, 0);
    }
}

impl<T: AsyncReadWithFd> AsyncReadWithFd for BufReaderWithFd<T> {
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

impl<T: AsyncWriteWithFd> BufWriterWithFd<T> {
    /// Flush until there is at least `len` bytes free at the end of the buffer capacity.
    fn poll_flush_buf_until(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        demand: usize,
        demand_fd: usize,
    ) -> Poll<std::io::Result<()>> {
        let mut this = self.as_mut().project();
        // Do we have extra capacity left at the end?
        let remaining_cap = this.buf.capacity() - this.buf.len();
        let remaining_cap_fd = this.fd_buf.capacity() - this.fd_buf.len();
        if remaining_cap >= demand && remaining_cap_fd >= demand_fd {
            return Poll::Ready(Ok(()));
        }
        let mut aim = demand - (this.buf.capacity() - this.buf.len());
        if aim <= *this.written && remaining_cap_fd < demand_fd {
            // If we need free file descriptor capacity, we need to write some data even if
            // we already met the data buffer capacity demand.
            aim = *this.written + 1;
        }
        while *this.written < this.buf.len() && *this.written < aim {
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
        let remaining_cap_fd = this.fd_buf.capacity() - this.fd_buf.len();
        if remaining_cap_fd < demand_fd {
            // This only happens if we have no data to write but somehow still have file descriptors.
            this.fd_buf.reserve(demand_fd);
        }
        if *this.written == this.buf.len() {
            // All written, no need to copy
            *this.written = 0;
            this.buf.clear();
            if demand > this.buf.len() {
                this.buf.reserve(demand - this.buf.len());
            }
        } else {
            // Move data to the front
            let len = this.buf.len() - *this.written;
            this.buf.copy_within(*this.written.., 0);
            this.buf.truncate(len);
        }
        Poll::Ready(Ok(()))
    }
    fn poll_flush_buf(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let cap = self.buf.capacity();
        let cap_fd = self.fd_buf.capacity();
        self.poll_flush_buf_until(cx, cap, cap_fd)
    }
}

impl<T: AsyncWriteWithFd> AsyncWrite for BufWriterWithFd<T> {
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

impl<T: AsyncWriteWithFd> AsyncBufWriteWithFd for BufWriterWithFd<T> {
    fn poll_write_to_reserve<'a>(
            mut self: Pin<&'a mut Self>,
            cx: &mut Context<'_>,
            demand: usize,
            demand_fd: usize,
            f: impl FnOnce(&mut WriteBuf<'_>) -> std::io::Result<()>,
        ) -> Poll<Result<()>> {
        ready!(self.as_mut().poll_flush_buf_until(cx, demand, demand_fd))?;
        let this = self.project();
        let mut write_buf = WriteBuf::from_spare(this.buf, this.fd_buf);
        f(&mut write_buf)?;
        let filled = write_buf.filled();
        let filled_fd = write_buf.filled_fd();
        unsafe {
            this.buf.set_len(this.buf.len() + filled);
            this.fd_buf.set_len(this.fd_buf.len() + filled_fd);
        }
        Poll::Ready(Ok(()))
    }
}

impl<T: AsyncWriteWithFd> AsyncWriteWithFd for BufWriterWithFd<T> {
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
