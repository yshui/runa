use std::os::unix::{
    io::OwnedFd,
    prelude::{AsRawFd, FromRawFd, RawFd},
};

use fixed::{traits::ToFixed, types::extra::U8};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewId(pub u32);

impl From<u32> for NewId {
    fn from(id: u32) -> Self {
        NewId(id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fixed(pub fixed::FixedI32<U8>);
#[derive(Debug)]
pub enum Fd {
    Raw(RawFd),
    Owned(OwnedFd),
}
impl From<RawFd> for Fd {
    fn from(fd: RawFd) -> Self {
        Fd::Raw(fd)
    }
}

impl From<OwnedFd> for Fd {
    fn from(fd: OwnedFd) -> Self {
        Fd::Owned(fd)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Object(pub u32);

impl From<u32> for Object {
    fn from(id: u32) -> Self {
        Object(id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub struct Str<'a>(pub &'a [u8]);
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct String(pub Vec<u8>);

impl From<std::string::String> for String {
    fn from(s: std::string::String) -> Self {
        String(s.into_bytes())
    }
}

impl<'a> From<&'a [u8]> for Str<'a> {
    fn from(value: &'a [u8]) -> Self {
        Str(value)
    }
}

impl<'a, const N: usize> From<&'a [u8; N]> for Str<'a> {
    fn from(value: &'a [u8; N]) -> Self {
        Str(value)
    }
}

impl String {
    pub fn as_str(&self) -> Str {
        Str(&self.0[..])
    }
}

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
    /// Convert a `Fd` into an `OwnedFd`.
    ///
    /// # Safety
    ///
    /// The `Fd` must own the contained file descriptor, i.e. this file
    /// descriptor must not be used elsewhere. This is because `OwnedFd`
    /// will close the file descriptor when it is dropped, and invalidate
    /// the file descriptor if it's incorrectly used elsewhere.
    pub unsafe fn assume_owned(&mut self) -> &mut OwnedFd {
        match self {
            Fd::Raw(fd) => {
                *self = Fd::Owned(OwnedFd::from_raw_fd(*fd));
                match self {
                    Fd::Owned(fd) => fd,
                    // Safety: we just assigned OwnedFd to self
                    Fd::Raw(_) => std::hint::unreachable_unchecked(),
                }
            },
            Fd::Owned(fd) => fd,
        }
    }

    /// Take ownership of the file descriptor.
    ///
    /// This will return `None` if the `Fd` does not own the file descriptor.
    pub fn take(&mut self) -> Option<OwnedFd> {
        match self {
            Fd::Raw(_) => None,
            Fd::Owned(fd) => {
                let mut fd2 = Fd::Raw(fd.as_raw_fd());
                std::mem::swap(self, &mut fd2);
                match fd2 {
                    Fd::Owned(fd) => Some(fd),
                    // Safety: we just swapped a Fd::Owned into fd2
                    Fd::Raw(_) => unsafe { std::hint::unreachable_unchecked() },
                }
            },
        }
    }

    /// Take ownership of the file descriptor.
    ///
    /// # Panic
    ///
    /// Panics if the `Fd` does not own the file descriptor.
    pub fn unwrap_owned(self) -> OwnedFd {
        match self {
            Fd::Raw(_) => panic!("file descriptor was not owned"),
            Fd::Owned(fd) => fd,
        }
    }

    /// Get a mutable reference to the owned file descriptor.
    ///
    /// # Panic
    ///
    /// Panics if the `Fd` does not own the file descriptor.
    pub fn unwrap_owned_mut(&mut self) -> &mut OwnedFd {
        match self {
            Fd::Raw(_) => panic!("file descriptor was not owned"),
            Fd::Owned(fd) => fd,
        }
    }
}

impl Fixed {
    pub fn from_bits(v: i32) -> Self {
        Self(fixed::FixedI32::from_bits(v))
    }

    pub fn from_num<Src: ToFixed>(src: Src) -> Self {
        Self(fixed::FixedI32::from_num(src))
    }
}
