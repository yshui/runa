use std::{
    ffi::{CStr, CString},
    os::unix::{
        io::OwnedFd,
        prelude::{AsRawFd, FromRawFd, RawFd},
    },
};

use fixed::types::extra::U8;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Object(pub u32);

impl From<u32> for Object {
    fn from(id: u32) -> Self {
        Object(id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Str<'a>(pub &'a [u8]);
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct String(pub Vec<u8>);

impl From<std::string::String> for String {
    fn from(s: std::string::String) -> Self {
        String(s.into_bytes())
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

impl<'a> From<&'a [u8]> for Str<'a> {
    fn from(s: &'a [u8]) -> Self {
        Str(s)
    }
}

impl From<i32> for Fixed {
    fn from(v: i32) -> Self {
        Self(fixed::FixedI32::from_bits(v))
    }
}
