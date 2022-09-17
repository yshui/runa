#![feature(generic_associated_types)]
use futures_lite::Future;
pub use wl_io::de::WaylandBufAccess;

#[doc(hidden)]
pub mod __private {
    pub use ::wl_io::AsyncBufReadWithFdExt;
}

/// The entry point of a wayland application, either a client or a server.
pub trait MessageDispatch {
    type Error;
    type Fut<'a>: Future<Output = Result<(), Self::Error>> + 'a;
    fn dispatch<'a, R>(
        &self,
        object_id: u32,
        buf: &mut WaylandBufAccess<'a, R>,
    ) -> Self::Fut<'a>
    where
        R: wl_io::AsyncBufReadWithFd;
}

/// The entry point of an interface implementation, called when message of a certain interface is
/// received
pub trait InterfaceMessageDispatch<Ctx> {
    type Error;
    // TODO: the R parameter might be unnecessary, see:
    //       https://github.com/rust-lang/rust/issues/42940
    type Fut<'a, R>: Future<Output = Result<(), Self::Error>> + 'a
    where
        Self: 'a,
        Ctx: 'a,
        R: 'a + wl_io::AsyncBufReadWithFdExt;
    fn dispatch<'a, 'b, R>(
        &'a self,
        ctx: &'a Ctx,
        buf: &'_ mut WaylandBufAccess<'b, R>,
    ) -> Self::Fut<'a, R>
    where
        R: wl_io::AsyncBufReadWithFdExt,
        'b: 'a;
}

pub use wl_macros::{interface_message_dispatch, message_broker};
pub mod types {
    use std::ffi::{CStr, CString};
    use std::os::unix::io::OwnedFd;
    use std::os::unix::prelude::{AsRawFd, FromRawFd, RawFd};

    use fixed::types::extra::U8;
    use serde::{de, Deserialize};
    #[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
    pub struct NewId(pub u32);
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Fixed(pub fixed::FixedI32<U8>);
    #[derive(Debug)]
    pub enum Fd {
        Raw(RawFd),
        Owned(OwnedFd),
    }
    #[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Object(pub u32);
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Str<'a>(pub &'a CStr);
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct String(pub CString);

    impl ::std::fmt::Display for NewId {
        fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
            write!(f, "NewId({})", self.0)
        }
    }

    impl AsRawFd for Fd {
        fn as_raw_fd(&self) -> RawFd {
            match self {
                Fd::Raw(fd) => *fd,
                Fd::Owned(fd) => fd.as_raw_fd(),
            }
        }
    }

    impl ::std::cmp::PartialEq for Fd {
        fn eq(&self, other: &Self) -> bool {
            self.as_raw_fd() == other.as_raw_fd()
        }
    }

    impl ::std::cmp::Eq for Fd {}

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
