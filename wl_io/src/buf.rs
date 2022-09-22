use std::{
    cell::RefCell,
    collections::VecDeque,
    io::Result,
    os::unix::io::{BorrowedFd, OwnedFd},
    pin::Pin,
    task::{Context, Poll},
};

use futures_lite::ready;
use pin_project_lite::pin_project;

use crate::{AsyncReadWithFd, AsyncWriteWithFd};

pub trait AsyncBufReadWithFd: AsyncReadWithFd {
    /// Reads enough data to return a buffer at least the given size.
    fn poll_fill_buf_until<'a>(
        self: Pin<&'a mut Self>,
        cx: &mut Context<'_>,
        len: usize,
    ) -> Poll<Result<()>>;
    /// Pop 1 file descriptor from the buffer, return None if the buffer is
    /// empty. This takes shared references, mainly because we want to have
    /// the deserialized value borrow from the BufReader, but while
    /// deserializing, we also need to pop file descriptors. As a
    /// compromise, we have to pop file descriptors using a shared reference.
    /// Implementations would have to use a RefCell, a Mutex, or something
    /// similar.
    fn next_fd(&self) -> Option<OwnedFd>;
    fn buffer(&self) -> &[u8];
    fn consume(self: Pin<&mut Self>, amt: usize);
}

pub struct FillBufUtil<'a, R: Unpin + ?Sized>(Option<&'a mut R>, usize);

impl<'a, R: AsyncBufReadWithFd + Unpin> ::std::future::Future for FillBufUtil<'a, R> {
    type Output = Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        let len = this.1;
        let inner = this.0.take().expect("FillBufUtil polled after completion");
        match Pin::new(&mut *inner).poll_fill_buf_until(cx, len) {
            Poll::Pending => {
                this.0 = Some(inner);
                Poll::Pending
            },
            ready => ready,
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

impl<T: AsyncBufReadWithFd + ?Sized> AsyncBufReadWithFdExt for T {}

pub trait AsyncBufWriteWithFd: AsyncWriteWithFd {
    /// Waits until there are at least `demand` bytes available in the buffer,
    /// and `demand_fd` available slots in the file descriptor buffer.
    /// Implementation shold first try to flush the buffer, until enough free
    /// space is available. If buffer is not big enough after a complete
    /// flush, it should allocate more space.
    fn poll_reserve(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        demand: usize,
        demand_fd: usize,
    ) -> Poll<Result<()>>;

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>>;

    /// Write data into the buffer, until the buffer is full. This function
    /// should just do memory copy and cannot fail. Return the number of
    /// bytes accepted.
    fn try_write(self: Pin<&mut Self>, buf: &[u8]) -> usize;

    /// Move file descriptor into the buffer, until the buffer is full. Return
    /// the number of file descriptors accepted.
    fn try_push_fds(self: Pin<&mut Self>, fds: &mut impl Iterator<Item = OwnedFd>) -> usize;
}

pub struct Reserve<'a, W: ?Sized>(Pin<&'a mut W>, (usize, usize));

impl<'a, W: AsyncBufWriteWithFd + Unpin + ?Sized> std::future::Future for Reserve<'a, W> {
    type Output = Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let (demand, demand_fd) = self.1;
        // Poll to ready with a dummy write function, because we can only use the
        // function once, and we can't lose it if we return Poll::Pending.
        self.0.as_mut().poll_reserve(cx, demand, demand_fd)
    }
}

type Flush<'a, T: Unpin + AsyncBufWriteWithFd + 'a> = impl std::future::Future<Output = Result<()>> + 'a;

pub trait AsyncBufWriteWithFdExt: AsyncBufWriteWithFd {
    fn reserve(&mut self, demand: usize, demand_fd: usize) -> Reserve<'_, Self>
    where
        Self: Unpin,
    {
        Reserve(Pin::new(self), (demand, demand_fd))
    }
    fn flush(&mut self) -> Flush<'_, Self>
    where
        Self: Unpin + Sized,
    {
        struct Flush<'a, W: ?Sized>(Pin<&'a mut W>);

        impl<'a, W: AsyncBufWriteWithFd + Unpin + ?Sized> std::future::Future for Flush<'a, W> {
            type Output = Result<()>;

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                self.0.as_mut().poll_flush(cx)
            }
        }

        Flush(Pin::new(self))
    }
}

impl<T: AsyncBufWriteWithFd + ?Sized> AsyncBufWriteWithFdExt for T {}

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

    fd_buf: RefCell<VecDeque<OwnedFd>>,
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

            fd_buf: RefCell::new(VecDeque::with_capacity(cap_fd)),
        }
    }

    #[inline]
    fn buffer(&self) -> &[u8] {
        let range = self.pos_data..self.filled_data;
        // Safety: invariant: filled_data <= buf.len()
        unsafe { self.buf.get_unchecked(range) }
    }
}

impl<T: AsyncReadWithFd> AsyncBufReadWithFd for BufReaderWithFd<T> {
    fn poll_fill_buf_until(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        len: usize,
    ) -> Poll<std::io::Result<()>> {
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

            // Safety: loop invariant: filled_data < len + pos_data
            // post condition from the if above: buf.len() >= len + pos_data
            // combined: filled_data < buf.len()
            let buf = unsafe { &mut this.buf.get_unchecked_mut(*this.filled_data..) };
            let last_fd_len = this.fd_buf.get_mut().len();
            let bytes =
                ready!(this
                    .inner
                    .poll_read_with_fds(cx, buf, this.fd_buf.get_mut(), None))?;
            if bytes == 0 && last_fd_len == this.fd_buf.get_mut().len() {
                // We hit EOF while the buffer is not filled
                return Poll::Ready(Err(std::io::ErrorKind::UnexpectedEof.into()));
            }
            *this.filled_data += bytes;
        }

        Poll::Ready(Ok(()))
    }

    #[inline]
    fn next_fd(&self) -> Option<OwnedFd> {
        self.fd_buf.borrow_mut().pop_front()
    }

    #[inline]
    fn buffer(&self) -> &[u8] {
        self.buffer()
    }

    fn consume(self: Pin<&mut Self>, amt: usize) {
        let this = self.project();
        *this.pos_data = std::cmp::min(*this.pos_data + amt, *this.filled_data);
    }
}

impl<T: AsyncReadWithFd> AsyncReadWithFd for BufReaderWithFd<T> {
    fn poll_read_with_fds(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        mut buf: &mut [u8],
        fds: &mut impl Extend<OwnedFd>,
        fd_limit: Option<usize>,
    ) -> Poll<std::io::Result<usize>> {
        ready!(self.as_mut().poll_fill_buf_until(cx, 1))?;
        let our_buf = self.as_ref().get_ref().buffer();
        let read_len = std::cmp::min(our_buf.len(), buf.len());
        buf[..read_len].copy_from_slice(&our_buf[..read_len]);
        buf = &mut buf[read_len..];

        let this = self.as_mut().project();
        let fd_limit = if let Some(fd_limit) = fd_limit {
            let fd_len = this.fd_buf.get_mut().len();
            fds.extend(this.fd_buf.get_mut().drain(..fd_limit));
            Some(fd_limit - std::cmp::min(fd_len, fd_limit))
        } else {
            fds.extend(this.fd_buf.get_mut().drain(..));
            None
        };

        self.as_mut().consume(read_len);

        let mut read = read_len;
        if !buf.is_empty() {
            // If we still have buffer left, we try to read directly into the buffer to
            // opportunistically avoid copying.
            //
            // If `poll_read_with_fds` returns `Poll::Pending`, next time this function is
            // called we will fill our buffer instead re-entering this if
            // branch.
            let this = self.project();
            match this.inner.poll_read_with_fds(cx, buf, fds, fd_limit)? {
                Poll::Ready(bytes) => {
                    read += bytes;
                },
                Poll::Pending => {}, // This is fine - we already read data.
            }
        }

        Poll::Ready(Ok(read))
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
            .field("buf", &"...")
            .field("fd_buf", &"...")
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

    fn try_write(self: Pin<&mut Self>, buf: &[u8]) -> usize {
        let this = self.project();
        let remaining_cap = this.buf.capacity() - this.buf.len();
        let to_write = std::cmp::min(remaining_cap, buf.len());
        this.buf.extend_from_slice(&buf[..to_write]);
        to_write
    }

    fn try_push_fds(self: Pin<&mut Self>, fds: &mut impl Iterator<Item = OwnedFd>) -> usize {
        let this = self.project();
        let last_len = this.fd_buf.len();
        let fds = fds.take(this.fd_buf.capacity() - last_len);
        this.fd_buf.extend(fds);
        this.fd_buf.len() - last_len
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
    use std::pin::Pin;

    use anyhow::Result;
    use arbitrary::Arbitrary;
    use smol::Task;
    use tracing::debug;

    use super::{
        AsyncBufReadWithFd, AsyncBufWriteWithFd, AsyncBufWriteWithFdExt, BufReaderWithFd,
        BufWriterWithFd,
    };
    use crate::{ReadWithFd, WriteWithFd};
    async fn buf_roundtrip_seeded(raw: &[u8], executor: &smol::LocalExecutor<'_>) {
        let mut source = arbitrary::Unstructured::new(raw);
        let (rx, tx) = std::os::unix::net::UnixStream::pair().unwrap();
        let (_, tx) = crate::split_unixstream(tx).unwrap();
        let (rx, _) = crate::split_unixstream(rx).unwrap();
        let mut tx = BufWriterWithFd::new(tx);
        let mut rx = BufReaderWithFd::new(rx);
        let task: Task<Result<()>> = executor.spawn(async move {
            debug!("start");
            // Read data and drop them
            use futures_lite::AsyncBufRead;

            use crate::AsyncBufReadWithFdExt;
            loop {
                rx.fill_buf_until(4).await?;
                let buf = rx.buffer();
                if buf.len() < 4 {
                    break
                }
                let len: [u8; 4] = buf[..4].try_into().unwrap();
                let len = u32::from_le_bytes(len) as usize;
                debug!("len: {:?}", len);
                rx.fill_buf_until(len).await?;
                Pin::new(&mut rx).consume(len);
                while let Some(fd) = rx.next_fd() {
                    debug!("fd: {:?}", fd);
                }
            }
            Ok(())
        });
        while let Ok(packet) = <&[u8]>::arbitrary(&mut source) {
            if packet.is_empty() {
                break
            }
            let has_fd = bool::arbitrary(&mut source).unwrap();
            let fds = if has_fd {
                let fd: std::os::unix::io::OwnedFd =
                    std::fs::File::open("/dev/null").unwrap().into();
                Some(fd)
            } else {
                None
            };
            let len = (packet.len() as u32 + 4).to_ne_bytes();
            tx.reserve(packet.len() + 4, if fds.is_some() { 1 } else { 0 })
                .await
                .unwrap();
            assert_eq!(Pin::new(&mut tx).try_write(&len), 4);
            assert_eq!(Pin::new(&mut tx).try_write(packet), packet.len());
            Pin::new(&mut tx).try_push_fds(&mut fds.into_iter());
        }
        tx.flush().await.unwrap();
        drop(tx);
        task.await.unwrap();
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
