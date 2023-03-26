#![feature(type_alias_impl_trait)]
use std::{
    future::Future,
    io::Result,
    os::{fd::OwnedFd, unix::io::RawFd},
    pin::Pin,
    task::{ready, Context, Poll},
};

/// A bunch of owned file descriptors.
pub trait OwnedFds: Extend<OwnedFd> {
    /// Returns the number of file descriptors.
    fn len(&self) -> usize;
    /// Returns the maximum number of file descriptors that can be stored.
    /// Trying to store more than this number of file descriptors will cause
    /// them to be dropped.
    ///
    /// Returns `None` if there is no limit.
    fn capacity(&self) -> Option<usize>;
    /// Returns true if there are no file descriptors.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Take all the file descriptors out of this object.
    fn take<T: Extend<OwnedFd>>(&mut self, fds: &mut T);
}

impl OwnedFds for Vec<OwnedFd> {
    #[inline]
    fn len(&self) -> usize {
        Vec::len(self)
    }

    #[inline]
    fn capacity(&self) -> Option<usize> {
        None
    }

    #[inline]
    fn take<T: Extend<OwnedFd>>(&mut self, fds: &mut T) {
        fds.extend(self.drain(..))
    }
}

/// A extension trait of `AsyncWrite` that supports sending file descriptors
/// along with data.
pub trait AsyncWriteWithFd {
    /// Writes the given buffer and file descriptors to the stream.
    ///
    /// # Note
    ///
    /// To send file descriptors, usually at least one byte of data must be
    /// sent. Unless, for example, the implementation choose to buffer the
    /// file descriptors until flush is called. Check the documentation of
    /// the specific implementation to see if this is the case.
    ///
    /// # Returns
    ///
    /// Returns the number of bytes written on success. The file descriptors
    /// will all be sent as long as they don't exceed the maximum number of
    /// file descriptors that can be sent in a message, in which case an
    /// error is returned.
    fn poll_write_with_fds<Fds: OwnedFds>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
        fds: &mut Fds,
    ) -> Poll<Result<usize>>;
}

impl<T: AsyncWriteWithFd + Unpin> AsyncWriteWithFd for &mut T {
    #[inline]
    fn poll_write_with_fds<Fds: OwnedFds>(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
        fds: &mut Fds,
    ) -> Poll<Result<usize>> {
        Pin::new(&mut **self).poll_write_with_fds(cx, buf, fds)
    }
}

pub struct Send<'a, W: WriteMessage + ?Sized + 'a, M: ser::Serialize + Unpin + std::fmt::Debug + 'a>
{
    writer:    &'a mut W,
    object_id: u32,
    msg:       Option<M>,
}
pub struct Flush<'a, W: WriteMessage + ?Sized + 'a> {
    writer: &'a mut W,
}

impl<
        'a,
        W: WriteMessage + Unpin + ?Sized + 'a,
        M: ser::Serialize + Unpin + std::fmt::Debug + 'a,
    > Future for Send<'a, W, M>
{
    type Output = std::io::Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut sink = Pin::new(&mut *this.writer);
        ready!(sink.as_mut().poll_ready(cx))?;
        sink.start_send(this.object_id, this.msg.take().unwrap());
        Poll::Ready(Ok(()))
    }
}

impl<'a, W: WriteMessage + Unpin + ?Sized + 'a> Future for Flush<'a, W> {
    type Output = std::io::Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        Pin::new(&mut *this.writer).poll_flush(cx)
    }
}

/// A trait for objects that can accept messages to be sent.
///
/// This is similar to `Sink`, but instead of accepting only one type of
/// Items, it accepts any type that implements
/// [`Serialize`](crate::ser::Serialize).
pub trait WriteMessage {
    /// Reserve space for a message
    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>>;

    /// Queue a message to be sent.
    ///
    /// # Panics
    ///
    /// if there is not enough space in the queue, this function panics.
    /// Before calling this, you should call `poll_reserve` to
    /// ensure there is enough space.
    fn start_send<M: ser::Serialize + std::fmt::Debug>(
        self: Pin<&mut Self>,
        object_id: u32,
        msg: M,
    );

    /// Flush connection
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>>;
    #[must_use]
    fn send<'a, 'b, 'c, M: ser::Serialize + Unpin + std::fmt::Debug + 'b>(
        &'a mut self,
        object_id: u32,
        msg: M,
    ) -> Send<'c, Self, M>
    where
        Self: Unpin,
        'a: 'c,
        'b: 'c,
    {
        Send {
            writer: self,
            object_id,
            msg: Some(msg),
        }
    }
    #[must_use]
    fn flush(&mut self) -> Flush<'_, Self>
    where
        Self: Unpin,
    {
        Flush { writer: self }
    }
}

/// A extension trait of `AsyncRead` that supports receiving file descriptors
/// along with data.
pub trait AsyncReadWithFd {
    /// Reads data and file descriptors from the stream. This is generic over
    /// how you store the file descriptors. Use something like tinyvec if
    /// you want to avoid heap allocations.
    ///
    /// This cumbersome interface mainly originates from the fact kernel would
    /// drop file descriptors if you don't give it a buffer big enough.
    /// Otherwise it would be easy to have read_data and read_fd be separate
    /// functions.
    ///
    /// # Arguments
    ///
    /// * `fds`     : Storage for the file descriptors.
    /// * `fd_limit`: Maximum number of file descriptors to receive. If more are
    ///   received, they could be closed or stored in a buffer, depends on the
    ///   implementation. None means no limit.
    ///
    /// # Note
    ///
    /// If the `fds` buffer is too small to hold all the file descriptors, the
    /// extra file descriptors MAY BE CLOSED (see [`OwnedFds`]). Some
    /// implementation might hold a buffer of file descriptors to prevent
    /// this from happening. You should check the documentation of the
    /// implementor.
    ///
    /// # Returns
    ///
    /// The number of bytes read.
    fn poll_read_with_fds<Fds: OwnedFds>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
        fds: &mut Fds,
    ) -> Poll<Result<usize>>;
}

/// Forward impl of `AsyncReadWithFd` for `&mut T` where `T: AsyncReadWithFd`.
impl<T: AsyncReadWithFd + Unpin> AsyncReadWithFd for &mut T {
    fn poll_read_with_fds<Fds: OwnedFds>(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
        fds: &mut Fds,
    ) -> Poll<Result<usize>> {
        Pin::new(&mut **self).poll_read_with_fds(cx, buf, fds)
    }
}

pub mod ser {
    use std::os::fd::OwnedFd;

    use bytes::BytesMut;

    /// A serialization trait, implemented by wayland message types.
    ///
    /// We can't use serde, because it doesn't support passing file descriptors.
    /// Most of the serialization code is expected to be generated by
    /// scanner.
    ///
    /// For now instead of a Serializer trait, we only serialize to
    /// bytes and fds, but this might change in the future.
    #[allow(clippy::len_without_is_empty)]
    pub trait Serialize {
        /// Serialize into the buffered writer. This function returns no errors,
        /// failures in seializing are generally program errors, and triggers
        /// panicking.
        ///
        /// # Panic
        ///
        /// If there is not enough space in the buffer, this function should
        /// panic - the user should have called `poll_reserve` before
        /// serializing, so this indicates programming error. If `self`
        /// contains file descriptors that aren't OwnedFd, this function
        /// panics too.
        fn serialize<Fds: Extend<OwnedFd>>(self, buf: &mut BytesMut, fds: &mut Fds);
        /// How many bytes will this message serialize to. Including the 8 byte
        /// header.
        fn len(&self) -> u16;
        /// How many file descriptors will this message serialize to.
        fn nfds(&self) -> u8;
    }
}

pub mod de {
    use std::{convert::Infallible, os::unix::io::RawFd};

    pub enum Error {
        InvalidIntEnum(i32, &'static str),
        InvalidUintEnum(u32, &'static str),
        UnknownOpcode(u32, &'static str),
        TrailingData(u32, u32),
        MissingNul(&'static str),
    }

    impl std::fmt::Debug for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Error::InvalidIntEnum(v, name) =>
                    write!(f, "int {v} is not a valid value for {name}"),
                Error::InvalidUintEnum(v, name) =>
                    write!(f, "uint {v} is not a valid value for {name}"),
                Error::UnknownOpcode(v, name) => write!(f, "opcode {v} is not valid for {name}"),
                Error::TrailingData(expected, got) => write!(
                    f,
                    "message trailing bytes, expected {expected} bytes, got {got} bytes"
                ),
                Error::MissingNul(name) =>
                    write!(f, "string value for {name} is missing the NUL terminator"),
            }
        }
    }

    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            std::fmt::Debug::fmt(self, f)
        }
    }

    impl std::error::Error for Error {}

    pub trait Deserialize<'a>: Sized {
        /// Deserialize from the given buffer. Returns deserialized message, and
        /// number of bytes and file descriptors consumed, respectively.
        fn deserialize(data: &'a [u8], fds: &'a [RawFd]) -> Result<Self, Error>;
    }
    impl<'a> Deserialize<'a> for Infallible {
        fn deserialize(_: &'a [u8], _: &'a [RawFd]) -> Result<Self, Error> {
            Err(Error::UnknownOpcode(0, "unexpected message for object"))
        }
    }
    impl<'a> Deserialize<'a> for (&'a [u8], &'a [RawFd]) {
        fn deserialize(data: &'a [u8], fds: &'a [RawFd]) -> Result<Self, Error> {
            Ok((data, fds))
        }
    }
}

pub mod buf {
    use std::{future::Future, io::Result, task::ready};

    use super::*;
    /// Buffered I/O object for a stream of bytes with file descriptors.
    ///
    /// # Safety
    ///
    /// See [`crate::AsyncReadWithFd`]. Also, implementation cannot hold copies,
    /// or use any of the file descriptors after they are consumed by the
    /// caller.
    pub unsafe trait AsyncBufReadWithFd: AsyncReadWithFd {
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
        /// compromise, we have to pop file descriptors using a shared
        /// reference. Implementations would have to use a RefCell, a
        /// Mutex, or something similar.
        fn fds(&self) -> &[RawFd];
        fn buffer(&self) -> &[u8];
        fn consume(self: Pin<&mut Self>, amt: usize, amt_fd: usize);

        fn fill_buf_until(&mut self, len: usize) -> FillBufUtil<'_, Self>
        where
            Self: Unpin,
        {
            FillBufUtil(Some(self), len)
        }

        fn poll_next_message<'a>(
            mut self: Pin<&'a mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Result<(u32, usize, &'a [u8], &'a [RawFd])>> {
            // Wait until we have the message header ready at least.
            let (object_id, len) = {
                ready!(self.as_mut().poll_fill_buf_until(cx, 8))?;
                let object_id = self
                    .buffer()
                    .get(..4)
                    .expect("Bug in poll_fill_buf_until implementation");
                // Safety: get is guaranteed to return a slice of 4 bytes.
                let object_id =
                    unsafe { u32::from_ne_bytes(*(object_id.as_ptr() as *const [u8; 4])) };
                let header = self
                    .buffer()
                    .get(4..8)
                    .expect("Bug in poll_fill_buf_until implementation");
                let header = unsafe { u32::from_ne_bytes(*(header.as_ptr() as *const [u8; 4])) };
                (object_id, (header >> 16) as usize)
            };

            ready!(self.as_mut().poll_fill_buf_until(cx, len))?;
            let this = self.into_ref().get_ref();
            Poll::Ready(Ok((object_id, len, &this.buffer()[..len], this.fds())))
        }

        fn next_message<'a>(self: Pin<&'a mut Self>) -> NextMessageFut<'a, Self>
        where
            Self: Sized,
        {
            pub struct NextMessage<'a, R>(Option<Pin<&'a mut R>>);
            impl<'a, R> Future for NextMessage<'a, R>
            where
                R: AsyncBufReadWithFd,
            {
                type Output = Result<(u32, usize, &'a [u8], &'a [RawFd])>;

                fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                    let this = self.get_mut();
                    let mut reader = this.0.take().expect("NextMessage polled after completion");
                    match reader.as_mut().poll_next_message(cx) {
                        Poll::Pending => {
                            this.0 = Some(reader);
                            Poll::Pending
                        },
                        Poll::Ready(Ok(_)) => match reader.poll_next_message(cx) {
                            Poll::Pending => {
                                panic!("poll_next_message returned Ready, but then Pending again")
                            },
                            ready => ready,
                        },
                        Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                    }
                }
            }
            NextMessage(Some(self))
        }
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

    pub type NextMessageFut<'a, T: AsyncBufReadWithFd + 'a> =
        impl Future<Output = Result<(u32, usize, &'a [u8], &'a [RawFd])>> + 'a;
}
