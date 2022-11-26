#![feature(type_alias_impl_trait)]
use std::{
    io::Result,
    os::unix::{
        io::{AsRawFd, BorrowedFd, OwnedFd, RawFd},
        net::UnixStream as StdUnixStream,
    },
    pin::Pin,
    rc::Rc,
    task::{ready, Context, Poll},
};

pub mod buf;
pub mod utils;

pub use buf::*;

pub mod traits {
    pub use wl_io_traits::*;
}

/// Maximum number of file descriptors that can be sent in a write by the
/// wayland protocol. As defined in libwayland.
#[allow(dead_code)]
const MAX_FDS_OUT: usize = 28;

pub(crate) const SCM_MAX_FD: usize = 253;

#[derive(Debug)]
pub struct ReadWithFd {
    inner: Rc<async_io::Async<OwnedFd>>,

    /// Temporary buffer used for recvmsg.
    buf: Vec<u8>,
}

#[derive(Debug)]
pub struct WriteWithFd {
    inner: Rc<async_io::Async<OwnedFd>>,
}

#[tracing::instrument(level = "debug", ret)]
pub fn split_unixstream(stream: StdUnixStream) -> Result<(ReadWithFd, WriteWithFd)> {
    let raw_fd = Rc::new(async_io::Async::new(stream.into())?);
    Ok((
        ReadWithFd {
            inner: raw_fd.clone(),
            buf:   nix::cmsg_space!([RawFd; SCM_MAX_FD]),
        },
        WriteWithFd { inner: raw_fd },
    ))
}

impl traits::AsyncWriteWithFd for WriteWithFd {
    /// Writes the given buffer and file descriptors to a unix stream. `buf`
    /// must contain at least one byte of data. This function should not be
    /// called concurrently from different tasks. Otherwise you risk
    /// interleaving data, as well as causing tasks to wake each other up and
    /// eatting CPU.
    fn poll_write_with_fds(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
        fds: &[BorrowedFd<'_>],
    ) -> Poll<Result<usize>> {
        use nix::sys::socket::{sendmsg, ControlMessage, MsgFlags};

        ready!(self.inner.poll_writable(cx)?);
        let fd = self.inner.as_raw_fd();
        let fd_len = fds.len();

        match sendmsg::<()>(
            fd,
            &[std::io::IoSlice::new(buf)],
            &[ControlMessage::ScmRights(unsafe {
                std::slice::from_raw_parts(fds.as_ptr().cast::<RawFd>(), fd_len)
            })],
            MsgFlags::MSG_DONTWAIT | MsgFlags::MSG_NOSIGNAL,
            None,
        ) {
            Err(nix::errno::Errno::EWOULDBLOCK) => Poll::Pending,
            Err(e) => Poll::Ready(Err(e.into())),
            Ok(n) => Poll::Ready(Ok(n)),
        }
    }
}

// Copying some code from nix, to avoid allocation

unsafe fn pack_mhdr_to_receive<'outer, 'inner, I, S>(
    iov: I,
    cmsg_buffer: &mut Option<&mut Vec<u8>>,
    address: *mut S,
) -> (usize, libc::msghdr)
where
    I: AsRef<[std::io::IoSliceMut<'inner>]> + 'outer,
    S: nix::sys::socket::SockaddrLike + 'outer,
{
    let (msg_control, msg_controllen) = cmsg_buffer
        .as_mut()
        .map(|v| (v.as_mut_ptr(), v.capacity()))
        .unwrap_or((std::ptr::null_mut(), 0));

    let mhdr = {
        // Musl's msghdr has private fields, so this is the only way to
        // initialize it.
        let mut mhdr = std::mem::MaybeUninit::<libc::msghdr>::zeroed();
        let p = mhdr.as_mut_ptr();
        (*p).msg_name = (*address).as_mut_ptr() as *mut libc::c_void;
        (*p).msg_namelen = S::size();
        (*p).msg_iov = iov.as_ref().as_ptr() as *mut libc::iovec;
        (*p).msg_iovlen = iov.as_ref().len() as _;
        (*p).msg_control = msg_control as *mut libc::c_void;
        (*p).msg_controllen = msg_controllen as _;
        (*p).msg_flags = 0;
        mhdr.assume_init()
    };

    (msg_controllen, mhdr)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecvMsg<'a, S> {
    bytes:   usize,
    cmsghdr: Option<&'a libc::cmsghdr>,
    address: Option<S>,
    flags:   nix::sys::socket::MsgFlags,
    mhdr:    libc::msghdr,
}
impl<'a, S> RecvMsg<'a, S> {
    /// Iterate over the valid control messages pointed to by this
    /// msghdr.
    pub fn scm_rights(&self) -> ScmRightsIterator {
        ScmRightsIterator {
            cmsghdr: self.cmsghdr,
            mhdr:    &self.mhdr,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScmRightsIterator<'a> {
    /// Control message buffer to decode from. Must adhere to cmsg alignment.
    cmsghdr: Option<&'a libc::cmsghdr>,
    mhdr:    &'a libc::msghdr,
}

pub struct FdIter<'a> {
    cmsghdr: &'a libc::cmsghdr,
    idx:     usize,
}

impl<'a> Iterator for ScmRightsIterator<'a> {
    type Item = FdIter<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.cmsghdr {
                None => break None, // No more messages
                Some(hdr) => {
                    // Get the data.
                    // Safe if cmsghdr points to valid data returned by recvmsg(2)
                    let ret = FdIter {
                        cmsghdr: hdr,
                        idx:     0,
                    };
                    self.cmsghdr = unsafe {
                        let p = libc::CMSG_NXTHDR(self.mhdr as *const _, hdr as *const _);
                        p.as_ref()
                    };
                    if hdr.cmsg_type != libc::SCM_RIGHTS || hdr.cmsg_level != libc::SOL_SOCKET {
                        continue
                    }
                    break Some(ret)
                },
            }
        }
    }
}

impl Iterator for FdIter<'_> {
    type Item = RawFd;

    fn next(&mut self) -> Option<Self::Item> {
        let p = unsafe { libc::CMSG_DATA(self.cmsghdr as *const _) };
        let data_len =
            self.cmsghdr as *const _ as usize + self.cmsghdr.cmsg_len as usize - p as usize;
        let nfds = data_len / std::mem::size_of::<RawFd>();
        let fds = unsafe { std::slice::from_raw_parts(p as *const RawFd, nfds) };
        let ret = fds.get(self.idx).copied();
        self.idx += 1;
        ret
    }
}

unsafe fn read_mhdr<'a, 'b, S>(
    mhdr: libc::msghdr,
    r: isize,
    msg_controllen: usize,
    address: S,
    cmsg_buffer: &'a mut Option<&'b mut Vec<u8>>,
) -> RecvMsg<'b, S>
where
    S: nix::sys::socket::SockaddrLike,
{
    let cmsghdr = {
        if mhdr.msg_controllen > 0 {
            // got control message(s)
            cmsg_buffer
                .as_mut()
                .unwrap()
                .set_len(mhdr.msg_controllen as usize);
            debug_assert!(!mhdr.msg_control.is_null());
            debug_assert!(msg_controllen >= mhdr.msg_controllen as usize);
            libc::CMSG_FIRSTHDR(&mhdr as *const libc::msghdr)
        } else {
            std::ptr::null()
        }
        .as_ref()
    };

    RecvMsg {
        bytes: r as usize,
        cmsghdr,
        address: Some(address),
        flags: nix::sys::socket::MsgFlags::from_bits_truncate(mhdr.msg_flags),
        mhdr,
    }
}

pub fn recvmsg<'a, 'outer, 'inner, S>(
    fd: RawFd,
    iov: &'outer mut [std::io::IoSliceMut<'inner>],
    mut cmsg_buffer: Option<&'a mut Vec<u8>>,
    flags: nix::sys::socket::MsgFlags,
) -> std::result::Result<RecvMsg<'a, S>, nix::Error>
where
    S: nix::sys::socket::SockaddrLike + 'a,
{
    let mut address = std::mem::MaybeUninit::uninit();

    let (msg_controllen, mut mhdr) =
        unsafe { pack_mhdr_to_receive::<_, S>(iov, &mut cmsg_buffer, address.as_mut_ptr()) };

    let ret = unsafe { libc::recvmsg(fd, &mut mhdr, flags.bits()) };

    let r = nix::errno::Errno::result(ret)?;

    Ok(unsafe {
        read_mhdr(
            mhdr,
            r,
            msg_controllen,
            address.assume_init(),
            &mut cmsg_buffer,
        )
    })
}

unsafe impl traits::AsyncReadWithFd for ReadWithFd {
    /// This implementation will close extra file descriptors if fd_limit is
    /// reached.
    fn poll_read_with_fds(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
        fds: &mut [RawFd],
    ) -> Poll<Result<(usize, usize)>> {
        use nix::sys::socket::MsgFlags;
        ready!(self.inner.poll_readable(cx)?);
        let fd = self.inner.as_raw_fd();

        match recvmsg::<()>(
            fd,
            &mut [std::io::IoSliceMut::new(buf)],
            Some(&mut self.buf),
            MsgFlags::MSG_DONTWAIT | MsgFlags::MSG_NOSIGNAL | MsgFlags::MSG_CMSG_CLOEXEC,
        ) {
            Err(nix::errno::Errno::EWOULDBLOCK) => Poll::Pending,
            Err(e) => Poll::Ready(Err(e.into())),
            Ok(msg) => {
                let mut ifds = msg.scm_rights().flatten();
                let mut count = 0;
                for (dst, src) in fds.iter_mut().zip(&mut ifds) {
                    *dst = src;
                    count += 1;
                }
                for fd in ifds {
                    nix::unistd::close(fd).unwrap();
                }
                Poll::Ready(Ok((msg.bytes, count)))
            },
        }
    }
}
