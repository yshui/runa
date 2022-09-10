#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Deserializer(#[from] serde::de::value::Error),
    #[error("Unknown opcode {0}")]
    UnknownOpcode(u32),
}

pub mod wayland_types {
    use std::ffi::{CStr, CString};
    use std::os::unix::io::OwnedFd;
    use std::os::unix::prelude::{AsRawFd, FromRawFd, RawFd};

    use fixed::types::extra::U8;
    use serde::{de, Deserialize};
    #[derive(Deserialize, Debug, Clone, Copy)]
    pub struct NewId(pub u32);
    #[derive(Debug, Clone, Copy)]
    pub struct Fixed(pub fixed::FixedI32<U8>);
    #[derive(Debug)]
    pub enum Fd {
        Raw(RawFd),
        Owned(OwnedFd),
    }
    #[derive(Deserialize, Debug, Clone, Copy)]
    pub struct Object(pub u32);
    #[derive(Debug, Clone)]
    pub struct Str<'a>(pub &'a CStr);
    #[derive(Debug, Clone)]
    pub struct String(pub CString);

    impl AsRawFd for Fd {
        fn as_raw_fd(&self) -> RawFd {
            match self {
                Fd::Raw(fd) => *fd,
                Fd::Owned(fd) => fd.as_raw_fd(),
            }
        }
    }

    impl Fd {
        pub unsafe fn assume_owned(&mut self) -> &mut OwnedFd {
            match self {
                Fd::Raw(fd) => {
                    *self = Fd::Owned(OwnedFd::from_raw_fd(*fd));
                    match self {
                        Fd::Owned(fd) => fd,
                        // Safety: we just assigned OwnedFd to self
                        Fd::Raw(_) => unsafe { std::hint::unreachable_unchecked() },
                    }
                }
                Fd::Owned(fd) => fd,
            }
        }
        pub fn unwrap_owned(self) -> OwnedFd {
            match self {
                Fd::Raw(_) => panic!("file descriptor was not owned"),
                Fd::Owned(fd) => fd,
            }
        }
        pub fn unwrap_owned_mut(&mut self) -> &mut OwnedFd {
            match self {
                Fd::Raw(_) => panic!("file descriptor was not owned"),
                Fd::Owned(fd) => fd,
            }
        }
    }

    impl<'de> de::Deserialize<'de> for Fd {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            struct Visitor;
            impl<'de> de::Visitor<'de> for Visitor {
                type Value = Fd;
                fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                    write!(formatter, "file descriptor")
                }

                fn visit_i32<E>(self, v: i32) -> Result<Self::Value, E>
                where
                    E: de::Error,
                {
                    Ok(Fd::Raw(v))
                }
            }
            let visitor = Visitor;
            deserializer.deserialize_newtype_struct(::wl_io::de::WAYLAND_FD_NEWTYPE_NAME, visitor)
        }
    }

    impl<'a, 'de: 'a> de::Deserialize<'de> for Str<'a> {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            use serde::de::Error;
            let s: &[u8] = Deserialize::deserialize(deserializer)?;
            Ok(Str(CStr::from_bytes_with_nul(s).map_err(D::Error::custom)?))
        }
    }
    impl<'de> de::Deserialize<'de> for Fixed {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            let s: i32 = Deserialize::deserialize(deserializer)?;
            Ok(Self(fixed::FixedI32::from_bits(s)))
        }
    }
    impl<'de> de::Deserialize<'de> for String {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            let s: Str = Deserialize::deserialize(deserializer)?;
            Ok(Self(s.0.into()))
        }
    }
}

pub mod io {
    pub use futures_lite::AsyncBufRead;

    pub use wl_io::*;
}

pub use futures_lite::ready;
pub use serde;

pub mod future {
    use std::{pin::Pin, task::Poll};
    pub struct PollMapFn<'a, F, O, T> {
        inner: Option<Pin<&'a mut T>>,
        f: F,
        _marker: ::std::marker::PhantomData<O>,
    }
    impl<'a, F, O, T> ::std::future::Future for PollMapFn<'a, F, O, T>
    where
        F: Fn(Pin<&'a mut T>, &mut ::std::task::Context<'_>) -> Poll<O> + Unpin,
        O: Unpin,
    {
        type Output = O;
        fn poll(
            mut self: ::std::pin::Pin<&mut Self>,
            cx: &mut ::std::task::Context<'_>,
        ) -> ::std::task::Poll<Self::Output> {
            use std::ptr::NonNull;
            let inner = self
                .inner
                .take()
                .expect("PollMapFn polled after completion");
            unsafe {
                // Safety:
                // 1. We don't move the reader using the &mut we get from Pin::get_mut_unchecked.
                // 2. We know Fn(Pin<&'de mut D>, cx) -> T is implemented for F, so we know the
                //    lifetime going from 'de to T is sound.
                // 3. In case of Ready, we won't touch raw_reader ever again here, and inner is left
                //    empty.
                // 4. In case of Pending, we know the inner is definitely no longer borrowed, so
                //    we can get our Pin<&mut inner> back safely.
                // 5. self.f must be called with Pin created from raw_inner to satisfy stacked
                //    borrow rules.
                let mut raw_inner = NonNull::from(inner.get_unchecked_mut());
                match (self.f)(Pin::new_unchecked(raw_inner.as_mut()), cx) {
                    Poll::Ready(v) => Poll::Ready(v),
                    Poll::Pending => {
                        self.inner = Some(Pin::new_unchecked(raw_inner.as_mut()));
                        Poll::Pending
                    }
                }
            }
        }
    }

    pub fn poll_map_fn<'a, F, O, T>(inner: Pin<&'a mut T>, f: F) -> PollMapFn<'a, F, O, T>
    where
        F: Fn(Pin<&'a mut T>, &mut ::std::task::Context<'_>) -> Poll<O> + Unpin,
    {
        PollMapFn {
            inner: Some(inner),
            f,
            _marker: ::std::marker::PhantomData,
        }
    }
}
