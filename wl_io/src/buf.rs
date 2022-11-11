use std::{
    io::Result,
    os::{
        fd::RawFd,
        unix::io::{BorrowedFd, OwnedFd},
    },
    pin::Pin,
    task::{ready, Context, Poll},
};

use pin_project_lite::pin_project;

use crate::traits::{
    buf::{AsyncBufReadWithFd, AsyncBufWriteWithFd},
    AsyncReadWithFd, AsyncWriteWithFd,
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
#[derive(Debug)]
pub struct BufReaderWithFd<T> {
    #[pin]
    inner: T,
    buf: Vec<u8>,
    cap_data: usize,
    filled_data: usize,
    pos_data: usize,

    fd_buf: Vec<RawFd>,
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
        // 2. buf.len() > filled_data, and buf.len() > cap_data - we can shrink the
        // buffer down to    filled_data or cap_data
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
        // Actually, we can't use MaybeUninit here, AsyncRead::poll_read has no
        // guarantee that it will definitely initialize the number of bytes it
        // claims to have read. That's why tokio uses a ReadBuf type track how
        // many bytes have been initialized.
        Self {
            inner,
            buf: vec![0; cap_data],
            filled_data: 0,
            pos_data: 0,
            cap_data,

            fd_buf: Vec::with_capacity(cap_fd),
        }
    }

    #[inline]
    fn buffer(&self) -> &[u8] {
        let range = self.pos_data..self.filled_data;
        // Safety: invariant: filled_data <= buf.len()
        unsafe { self.buf.get_unchecked(range) }
    }
}

unsafe impl<T: AsyncReadWithFd> AsyncBufReadWithFd for BufReaderWithFd<T> {
    fn poll_fill_buf_until(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        len: usize,
    ) -> Poll<std::io::Result<()>> {
        if self.pos_data + len > self.buf.len() || self.filled_data == self.pos_data {
            // Try to shrink buffer before we grow it. Or adjust buffer pointers when the
            // buf is empty.
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

            // Safety: loop invariant: filled_data < len + pos_data
            // post condition from the if above: buf.len() >= len + pos_data
            // combined: filled_data < buf.len()
            let buf = unsafe { &mut this.buf.get_unchecked_mut(*this.filled_data..) };
            let mut fd_buf = [0; crate::SCM_MAX_FD as usize];
            let (bytes, nfds) = ready!(this.inner.poll_read_with_fds(cx, buf, &mut fd_buf[..]))?;
            if bytes == 0 && nfds == 0 {
                // We hit EOF while the buffer is not filled
                tracing::debug!(
                    "EOF while the buffer is not filled, filled {}",
                    this.filled_data
                );
                return Poll::Ready(Err(std::io::ErrorKind::UnexpectedEof.into()))
            }
            this.fd_buf.extend_from_slice(&fd_buf[..nfds]);
            *this.filled_data += bytes;
        }

        Poll::Ready(Ok(()))
    }

    #[inline]
    fn fds(&self) -> &[RawFd] {
        &self.fd_buf[..]
    }

    #[inline]
    fn buffer(&self) -> &[u8] {
        self.buffer()
    }

    fn consume(self: Pin<&mut Self>, amt: usize, amt_fd: usize) {
        let this = self.project();
        *this.pos_data = std::cmp::min(*this.pos_data + amt, *this.filled_data);
        this.fd_buf.drain(..amt_fd);
    }
}

unsafe impl<T: AsyncReadWithFd> AsyncReadWithFd for BufReaderWithFd<T> {
    fn poll_read_with_fds(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        mut buf: &mut [u8],
        fds: &mut [RawFd],
    ) -> Poll<std::io::Result<(usize, usize)>> {
        ready!(self.as_mut().poll_fill_buf_until(cx, 1))?;
        let our_buf = self.as_ref().get_ref().buffer();
        let read_len = std::cmp::min(our_buf.len(), buf.len());
        buf[..read_len].copy_from_slice(&our_buf[..read_len]);
        buf = &mut buf[read_len..];

        let this = self.as_mut().project();
        let fd_len = std::cmp::min(this.fd_buf.len(), fds.len());
        fds.copy_from_slice(&this.fd_buf[..fd_len]);

        self.as_mut().consume(read_len, fd_len);

        let mut read = read_len;
        let mut fd_read = fd_len;
        if !buf.is_empty() {
            // If we still have buffer left, we try to read directly into the buffer to
            // opportunistically avoid copying.
            //
            // If `poll_read_with_fds` returns `Poll::Pending`, next time this function is
            // called we will fill our buffer instead re-entering this if
            // branch.
            let this = self.project();
            match this.inner.poll_read_with_fds(cx, buf, &mut fds[fd_len..])? {
                Poll::Ready((bytes, nfds)) => {
                    read += bytes;
                    fd_read += nfds;
                },
                Poll::Pending => {}, // This is fine - we already read data.
            }
        }

        Poll::Ready(Ok((read, fd_read)))
    }
}

pin_project! {
pub struct BufWriterWithFd<T> {
    #[pin]
    inner: T,
    buf: Vec<u8>,
    fd_buf: Vec<OwnedFd>,
    written: usize,
}
}

impl<T: std::fmt::Debug> std::fmt::Debug for BufWriterWithFd<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufWriterWithFd")
            .field("inner", &self.inner)
            .field("buf", &"…")
            .field("fd_buf", &"…")
            .field("written", &self.written)
            .finish()
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
    fn poll_flush_buf(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let cap = self.buf.capacity();
        let cap_fd = self.fd_buf.capacity();
        self.poll_reserve(cx, cap, cap_fd)
    }
}

impl<T: AsyncWriteWithFd> AsyncBufWriteWithFd for BufWriterWithFd<T> {
    /// Flush until there is at least `len` bytes free at the end of the buffer
    /// capacity.
    fn poll_reserve(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        demand: usize,
        demand_fd: usize,
    ) -> Poll<std::io::Result<()>> {
        let mut this = self.as_mut().project();
        //debug!(
        //    "poll_reserve: demand={}, demand_fd={}, written={}, buf.len={},
        // remaining_cap={}",    demand,
        //    demand_fd,
        //    this.written,
        //    this.buf.len(),
        //    this.buf.capacity() - this.buf.len()
        //);
        // Do we have extra capacity left at the end?
        let remaining_cap = this.buf.capacity() - this.buf.len();
        let remaining_cap_fd = this.fd_buf.capacity() - this.fd_buf.len();
        if remaining_cap >= demand && remaining_cap_fd >= demand_fd {
            return Poll::Ready(Ok(()))
        }
        let mut aim = demand - (this.buf.capacity() - this.buf.len());
        if aim <= *this.written && remaining_cap_fd < demand_fd {
            // If we need free file descriptor capacity, we need to write some data even if
            // we already met the data buffer capacity demand.
            aim = *this.written + 1;
        }
        while *this.written < this.buf.len() && *this.written < aim {
            let fd_len = this.fd_buf.len();
            match ready!(this.inner.as_mut().poll_write_with_fds(
                cx,
                &this.buf[*this.written..],
                // Safety: OwnedFd and BorrowedFd are both transparent wrappers around RawFd
                unsafe {
                    std::slice::from_raw_parts(
                        this.fd_buf.as_slice().as_ptr().cast::<BorrowedFd>(),
                        fd_len,
                    )
                },
            )) {
                Ok(0) =>
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "failed to write whole buffer",
                    ))),
                Ok(written) => {
                    //debug!(
                    //    "wrote {} bytes, offset {}, len {}",
                    //    written,
                    //    *this.written,
                    //    this.buf.len()
                    //);
                    this.fd_buf.clear();
                    *this.written += written;
                },
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {},
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
        let remaining_cap_fd = this.fd_buf.capacity() - this.fd_buf.len();
        if remaining_cap_fd < demand_fd {
            // This only happens if we have no data to write but somehow still have file
            // descriptors.
            this.fd_buf.reserve(demand_fd);
        }
        assert!(*this.written <= this.buf.len());
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
            *this.written = 0;
        }
        Poll::Ready(Ok(()))
    }

    fn write(self: Pin<&mut Self>, buf: &[u8]) {
        let this = self.project();
        let remaining_cap = this.buf.capacity() - this.buf.len();
        assert!(remaining_cap >= buf.len());
        this.buf.extend_from_slice(buf);
    }

    fn push_fds(self: Pin<&mut Self>, fds: &mut impl Iterator<Item = OwnedFd>) {
        let this = self.project();
        let last_cap = this.fd_buf.capacity();
        this.fd_buf.extend(fds);
        assert!(this.fd_buf.len() <= last_cap);
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.poll_flush_buf(cx)
    }
}

impl<T: AsyncWriteWithFd> AsyncWriteWithFd for BufWriterWithFd<T> {
    fn poll_write_with_fds(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
        fds: &[BorrowedFd],
    ) -> Poll<std::io::Result<usize>> {
        // Flush if we don't have capacity for the data and file descriptors.
        // We can take any amount of data, but we have to take all file descriptors
        if (self.buf.len() >= self.buf.capacity() && buf.len() != 0) ||
            self.fd_buf.len() + fds.len() >= self.fd_buf.capacity()
        {
            ready!(self.as_mut().poll_flush_buf(cx))?;
        }
        // Take however much we can take and put them into the buffer.
        let this = self.project();
        let to_write = std::cmp::min(this.buf.capacity() - this.buf.len(), buf.len());
        this.buf.extend_from_slice(&buf[..to_write]);
        for fd in fds {
            // We are required to take all file descriptors
            this.fd_buf.push(fd.try_clone_to_owned()?);
        }
        Poll::Ready(Ok(buf.len()))
    }
}

#[cfg(test)]
mod test {
    use std::{
        os::fd::{AsRawFd, IntoRawFd},
        pin::Pin,
    };

    use anyhow::Result;
    use arbitrary::Arbitrary;
    use smol::Task;
    use tracing::debug;

    use crate::{
        traits::buf::{AsyncBufReadWithFd, AsyncBufWriteWithFd, AsyncBufWriteWithFdExt},
        BufReaderWithFd, BufWriterWithFd, ReadWithFd, WriteWithFd,
    };
    async fn buf_roundtrip_seeded(raw: &[u8], executor: &smol::LocalExecutor<'_>) {
        let mut source = arbitrary::Unstructured::new(raw);
        let (rx, tx) = std::os::unix::net::UnixStream::pair().unwrap();
        let (_, tx) = crate::split_unixstream(tx).unwrap();
        let (rx, _) = crate::split_unixstream(rx).unwrap();
        let mut tx = BufWriterWithFd::new(tx);
        let mut rx = BufReaderWithFd::new(rx);
        let task: Task<Result<_>> = executor.spawn(async move {
            debug!("start");
            use futures_lite::AsyncBufRead;

            use crate::traits::buf::AsyncBufReadWithFdExt;
            let mut bytes = Vec::new();
            let mut fds = Vec::new();
            loop {
                let buf = if let Err(e) = rx.fill_buf_until(4).await {
                    if e.kind() == std::io::ErrorKind::UnexpectedEof {
                        break
                    } else {
                        return Err(e.into())
                    }
                } else {
                    rx.buffer()
                };
                assert!(buf.len() >= 4);
                let len: [u8; 4] = buf[..4].try_into().unwrap();
                let len = u32::from_le_bytes(len) as usize;
                debug!("len: {:?}", len);
                rx.fill_buf_until(len).await?;
                bytes.extend_from_slice(&rx.buffer()[4..len]);
                fds.extend_from_slice(rx.fds());
                debug!("fds: {:?}", rx.fds());
                let nfds = rx.fds().len();
                Pin::new(&mut rx).consume(len, nfds);
            }
            Ok((bytes, fds))
        });
        let mut sent_bytes = Vec::new();
        let mut sent_fds = Vec::new();
        use crate::traits::buf::AsyncBufWriteWithFdExt;
        while let Ok(packet) = <&[u8]>::arbitrary(&mut source) {
            if packet.is_empty() {
                break
            }
            let has_fd = bool::arbitrary(&mut source).unwrap();
            let fds = if has_fd {
                let fd: std::os::unix::io::OwnedFd =
                    std::fs::File::open("/dev/null").unwrap().into();
                sent_fds.push(fd.as_raw_fd());
                Some(fd)
            } else {
                None
            };
            let len = (packet.len() as u32 + 4).to_ne_bytes();
            tx.reserve(packet.len() + 4, if fds.is_some() { 1 } else { 0 })
                .await
                .unwrap();
            Pin::new(&mut tx).write(&len);
            Pin::new(&mut tx).write(packet);
            debug!("send len: {:?}", packet.len() + 4);
            sent_bytes.extend_from_slice(packet);
            Pin::new(&mut tx).push_fds(&mut fds.into_iter());
        }
        tx.flush().await.unwrap();
        drop(tx);
        let (bytes, fds) = task.await.unwrap();
        assert_eq!(bytes, sent_bytes);
        // The actual file descriptor number is not preserved, so we just check the
        // number of file descriptors matches.
        assert_eq!(fds.len(), sent_fds.len());
    }
    #[test]
    fn buf_roundtrip() {
        use rand::{Rng, SeedableRng};
        tracing_subscriber::fmt::init();
        let mut rng = rand::rngs::SmallRng::seed_from_u64(0x1238_aefb_d129_3a12);
        let mut raw: Vec<u8> = Vec::with_capacity(1024 * 1024);
        let executor = smol::LocalExecutor::new();
        raw.resize(1024 * 1024, 0);
        rng.fill(raw.as_mut_slice());
        futures_executor::block_on(executor.run(buf_roundtrip_seeded(&raw, &executor)));
    }
}
