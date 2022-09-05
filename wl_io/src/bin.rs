use std::{
    ffi::OsStr,
    os::unix::prelude::RawFd,
    pin::Pin,
    task::{Context, Poll},
};

use futures_lite::ready;
use pin_project_lite::pin_project;

use crate::{AsyncReadWithFds, Deserializer};

pin_project! {
#[project = BinProject]
pub struct Bin<T> {
    buf: Vec<u8>,
    cap: usize,
    #[pin]
    inner: T
}
}

mod internal {
    use super::*;
    use std::{future::Future, pin::Pin};

    pub struct FillInternalBuf<'a, T> {
        pub(super) inner: BinProject<'a, T>,
        pub(super) len: usize,
        pub(super) offset: usize,
    }

    impl<'a, T: AsyncReadWithFds> Future for FillInternalBuf<'a, T> {
        type Output = std::io::Result<()>;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let Self { inner, len, offset } = &mut *self;
            while offset < len {
                let read = ready!(inner
                    .inner
                    .as_mut()
                    .poll_read(cx, unsafe { inner.buf.get_unchecked_mut(*offset..*len) }))?;
                if read == 0 {
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "EOF",
                    )));
                }
                *offset += read;
            }
            Poll::Ready(Ok(()))
        }
    }

    pub struct DeserializeU32<'a, T> {
        pub(super) inner: FillInternalBuf<'a, T>,
    }

    impl<'a, T: AsyncReadWithFds> Future for DeserializeU32<'a, T> {
        type Output = std::io::Result<u32>;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let () = ready!(Pin::new(&mut self.inner).poll(cx))?;
            let mut buf = [0u8; 4];
            buf.copy_from_slice(&self.inner.inner.buf[..4]);
            Poll::Ready(Ok(u32::from_ne_bytes(buf)))
        }
    }

    pub struct DeserializeI32<'a, T> {
        pub(super) inner: FillInternalBuf<'a, T>,
    }

    impl<'a, T: AsyncReadWithFds> Future for DeserializeI32<'a, T> {
        type Output = std::io::Result<i32>;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let () = ready!(Pin::new(&mut self.inner).poll(cx))?;
            let mut buf = [0u8; 4];
            buf.copy_from_slice(&self.inner.inner.buf[..4]);
            Poll::Ready(Ok(i32::from_ne_bytes(buf)))
        }
    }

    pub enum DeserializeBytes<'a, T> {
        ReadLen {
            inner: DeserializeU32<'a, T>,
        },
        ReadBytes {
            len: u32,
            inner: FillInternalBuf<'a, T>,
        },
        Invalid,
    }

    impl<'a, T: AsyncReadWithFds> Future for DeserializeBytes<'a, T> {
        type Output = std::io::Result<&'a [u8]>;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            match &mut *self {
                Self::ReadLen { ref mut inner } => {
                    let len = ready!(Pin::new(&mut *inner).poll(cx))?;
                    // Swap with invalid so we can regain ownership of bin. This is
                    // essentially replace_with, but that's not available from std.
                    match std::mem::replace(&mut *self, Self::Invalid) {
                        Self::ReadLen {
                            inner:
                                DeserializeU32 {
                                    inner: FillInternalBuf { inner, .. },
                                },
                        } => {
                            *self = Self::ReadBytes {
                                len,
                                inner: inner.fill_internal_buf(((len + 3) & !3) as usize),
                            };
                            self.poll(cx)
                        }
                        _ => unreachable!(),
                    }
                }
                Self::ReadBytes { ref mut inner, .. } => {
                    let () = ready!(Pin::new(&mut *inner).poll(cx))?;
                    match std::mem::replace(&mut *self, Self::Invalid) {
                        Self::ReadBytes { inner, len } => {
                            let FillInternalBuf { inner, .. } = inner;
                            Poll::Ready(Ok(&inner.buf[..len as usize]))
                        }
                        _ => unreachable!(),
                    }
                }
                Self::Invalid => {
                    panic!("DeserializeString in invalid state, or polled after completion")
                }
            }
        }
    }
}

use internal::*;

impl<'a, T> BinProject<'a, T> {
    fn adjust_buffer(&mut self, len: usize) {
        if self.buf.capacity() < len {
            self.buf.reserve(len - self.buf.capacity());
        } else if len <= *self.cap {
            // our cap is enough to hold the string, so we try to shrink to cap
            self.buf.truncate(*self.cap);
            self.buf.shrink_to(*self.cap);
        }
    }

    fn fill_internal_buf(mut self, len: usize) -> FillInternalBuf<'a, T> {
        Self::adjust_buffer(&mut self, len);
        FillInternalBuf {
            inner: self,
            len,
            offset: 0,
        }
    }
}

/// A deserializer for deserializing data stored in wayland's wire format.
///
/// See: https://wayland.freedesktop.org/docs/html/ch04.html#sect-Protocol-Wire-Format
impl<'de, T: AsyncReadWithFds + 'de> Deserializer<'de> for Bin<T> {
    type Error = ::std::io::Error;
    type DeserializeU32 = DeserializeU32<'de, T>;
    type DeserializeI32 = DeserializeI32<'de, T>;
    type DeserializeBytes = DeserializeBytes<'de, T>;
    fn poll_deserialize_fd(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<RawFd, Self::Error>> {
        let mut fds = [0; 1];
        let this = self.project();
        let (_, nfds) = ready!(this.inner.poll_read_with_fds(cx, &mut [], &mut fds))?;
        if nfds == 0 {
            Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "No enough file descriptors",
            )))
        } else {
            Poll::Ready(Ok(fds[0]))
        }
    }
    fn deserialize_u32(self: Pin<&'de mut Self>) -> DeserializeU32<'de, T> {
        DeserializeU32 {
            inner: self.project().fill_internal_buf(4),
        }
    }
    fn deserialize_i32(self: Pin<&'de mut Self>) -> DeserializeI32<'de, T> {
        DeserializeI32 {
            inner: self.project().fill_internal_buf(4),
        }
    }

    fn deserialize_bytes(self: Pin<&'de mut Self>) -> DeserializeBytes<'de, T> {
        DeserializeBytes::ReadLen {
            inner: self.deserialize_u32(),
        }
    }
}
