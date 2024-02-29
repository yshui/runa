use std::{
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::io::RawFd,
    },
    pin::Pin,
    task::{ready, Poll},
};

use pin_project_lite::pin_project;
use runa_io_traits::{OwnedFds, ReadMessage};

use crate::traits::{AsyncBufReadWithFd, AsyncReadWithFd};

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
            // Safety: OwnedFd is repr(transparent) over RawFd.
            let fd_buf = unsafe {
                std::mem::transmute::<&mut Vec<RawFd>, &mut Vec<OwnedFd>>(&mut *this.fd_buf)
            };
            let nfds = fd_buf.len();
            let bytes = ready!(this.inner.poll_read_with_fds(cx, buf, fd_buf))?;
            if bytes == 0 && (fd_buf.len() == nfds) {
                // We hit EOF while the buffer is not filled
                tracing::debug!(
                    "EOF while the buffer is not filled, filled {}",
                    this.filled_data
                );
                return Poll::Ready(Err(std::io::ErrorKind::UnexpectedEof.into()))
            }
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
        assert!(amt <= *this.filled_data - *this.pos_data);
        *this.pos_data += amt;
        this.fd_buf.drain(..amt_fd);
    }
}

impl<T: AsyncReadWithFd> ReadMessage for BufReaderWithFd<T> {}

impl<T: AsyncReadWithFd> AsyncReadWithFd for BufReaderWithFd<T> {
    fn poll_read_with_fds<Fds: OwnedFds>(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        mut buf: &mut [u8],
        fds: &mut Fds,
    ) -> Poll<std::io::Result<usize>> {
        ready!(self.as_mut().poll_fill_buf_until(cx, 1))?;
        let our_buf = self.as_ref().get_ref().buffer();
        let read_len = std::cmp::min(our_buf.len(), buf.len());
        buf[..read_len].copy_from_slice(&our_buf[..read_len]);
        buf = &mut buf[read_len..];

        let this = self.as_mut().project();
        fds.extend(
            this.fd_buf
                .drain(..)
                .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) }),
        );

        self.as_mut().consume(read_len, 0);

        let mut read = read_len;
        if !buf.is_empty() {
            // If we still have buffer left, we try to read directly into the buffer to
            // opportunistically avoid copying.
            //
            // If `poll_read_with_fds` returns `Poll::Pending`, next time this function is
            // called we will fill our buffer instead re-entering this if
            // branch.
            let this = self.project();
            match this.inner.poll_read_with_fds(cx, buf, fds)? {
                Poll::Ready(bytes) => {
                    read += bytes;
                },
                Poll::Pending => {}, // This is fine - we already read data.
            }
        }

        Poll::Ready(Ok(read))
    }
}

#[cfg(test)]
mod test {
    use std::{os::fd::AsRawFd, pin::Pin};

    use anyhow::Result;
    use arbitrary::Arbitrary;
    use smol::{future::poll_fn, Task};
    use tracing::debug;

    use crate::{
        traits::{buf::AsyncBufReadWithFd, AsyncWriteWithFd},
        BufReaderWithFd,
    };
    async fn buf_roundtrip_seeded(raw: &[u8], executor: &smol::LocalExecutor<'_>) {
        let mut source = arbitrary::Unstructured::new(raw);
        let (rx, tx) = std::os::unix::net::UnixStream::pair().unwrap();
        let (_, mut tx) = crate::split_unixstream(tx).unwrap();
        let (rx, _) = crate::split_unixstream(rx).unwrap();
        let mut rx = BufReaderWithFd::new(rx);
        let task: Task<Result<_>> = executor.spawn(async move {
            debug!("start");

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
        while let Ok(packet) = <&[u8]>::arbitrary(&mut source) {
            if packet.is_empty() {
                break
            }
            let has_fd = bool::arbitrary(&mut source).unwrap();
            let mut fds = if has_fd {
                let fd: std::os::unix::io::OwnedFd =
                    std::fs::File::open("/dev/null").unwrap().into();
                sent_fds.push(fd.as_raw_fd());
                vec![fd]
            } else {
                Vec::new()
            };
            let len = (packet.len() as u32 + 4).to_ne_bytes();
            poll_fn(|cx| Pin::new(&mut tx).poll_write_with_fds(cx, &len[..], &mut vec![]))
                .await
                .unwrap();
            poll_fn(|cx| Pin::new(&mut tx).poll_write_with_fds(cx, packet, &mut fds))
                .await
                .unwrap();
            debug!("send len: {:?}", packet.len() + 4);
            sent_bytes.extend_from_slice(packet);
        }
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
