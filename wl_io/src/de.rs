use std::os::fd::RawFd;

/// When passed this string as name, deserialize_newtype_struct will try
/// to deserialize a file descriptor.
pub const WAYLAND_FD_NEWTYPE_NAME: &str = "\0__WaylandFd__";

/// Holds a value that is deserialized from borrowed data from `reader`.
/// This prevents the reader from being used again while this accessor is alive.
/// Also this accessor will advance the reader to the next message when dropped.
#[derive(Debug)]
pub struct Deserializer<'a> {
    bytes:      &'a [u8],
    bytes_read: usize,
    fds:        &'a [RawFd],
    fds_read:   usize,
}

/// An analogue of `&'b mut Deserializer<'a>`, but is covariant w.r.t
/// `'a`.
pub struct DeserializerRefMut<'a, 'b> {
    bytes:      &'a [u8],
    fds:        &'a [RawFd],
    bytes_read: &'b mut usize,
    fds_read:   &'b mut usize,
}

impl<'a, 'b> crate::traits::de::Deserializer<'a> for DeserializerRefMut<'a, 'b> {
    #[inline]
    fn pop_fd(&mut self) -> RawFd {
        let offset = *self.fds_read;
        assert!(self.fds.len() > offset, "Not enough fds in buffer");
        // Safety: we checked that the slice is long enough
        let ret = unsafe { *self.fds.get_unchecked(offset) };
        *self.fds_read += 1;
        ret
    }

    #[inline]
    fn pop_bytes(&mut self, len: usize) -> &'a [u8] {
        let offset = *self.bytes_read;
        assert!(
            self.bytes.len() >= len + offset,
            "Not enough bytes in buffer, read {}, has {}, asking for {}",
            offset,
            self.bytes.len(),
            len
        );
        // Safety: we checked that the slice is long enough
        let buf = unsafe { self.bytes.get_unchecked(offset..offset + len) };
        *self.bytes_read += (len + 3) & !3;
        buf
    }

    #[inline]
    fn pop_i32(&mut self) -> i32 {
        let slice = self.pop_bytes(4);
        // Safety: slice is guaranteed to be 4 bytes long
        let ret = i32::from_ne_bytes(unsafe { *(slice.as_ptr() as *const [u8; 4]) });
        ret
    }

    #[inline]
    fn pop_u32(&mut self) -> u32 {
        let slice = self.pop_bytes(4);
        // Safety: slice is guaranteed to be 4 bytes long
        let ret = u32::from_ne_bytes(unsafe { *(slice.as_ptr() as *const [u8; 4]) });
        ret
    }
}

impl<'a> Deserializer<'a> {
    #[inline]
    pub fn new(bytes: &'a [u8], fds: &'a [RawFd]) -> Self {
        Self {
            bytes,
            bytes_read: 0,
            fds,
            fds_read: 0,
        }
    }

    pub fn borrow_mut<'b>(&'b mut self) -> DeserializerRefMut<'a, 'b> {
        DeserializerRefMut {
            bytes:      self.bytes,
            fds:        self.fds,
            bytes_read: &mut self.bytes_read,
            fds_read:   &mut self.fds_read,
        }
    }

    /// Return the number of bytes and file descriptors read.
    #[inline]
    pub fn consumed(&self) -> (usize, usize) {
        (self.bytes_read, self.fds_read)
    }

    #[inline]
    pub fn deserialize<'b, T>(&'b mut self) -> Result<T, crate::traits::de::Error>
    where
        T: crate::traits::de::Deserialize<'a>,
    {
        T::deserialize(self)
    }
}

impl<'a> crate::traits::de::Deserializer<'a> for Deserializer<'a> {
    #[inline]
    fn pop_bytes(&mut self, len: usize) -> &'a [u8] {
        self.borrow_mut().pop_bytes(len)
    }

    #[inline]
    fn pop_fd(&mut self) -> RawFd {
        self.borrow_mut().pop_fd()
    }

    #[inline]
    fn pop_i32(&mut self) -> i32 {
        self.borrow_mut().pop_i32()
    }

    #[inline]
    fn pop_u32(&mut self) -> u32 {
        self.borrow_mut().pop_u32()
    }
}

#[cfg(test)]
mod test {
    use std::os::fd::IntoRawFd;

    #[test]
    fn test_serialize_deserialize_wl_display() {
        use std::pin::Pin;

        use futures_lite::AsyncWrite;
        use futures_test::task::noop_context;

        use crate::{traits::ser::Serialize, utils::WritePool};
        let mut buf = WritePool::new();
        let msg = wl_protocol::wayland::wl_display::v1::requests::Sync {
            callback: wl_types::NewId(100),
        };
        let _ = Pin::new(&mut buf).poll_write(&mut noop_context(), &[0, 0, 0, 0]);
        msg.serialize(Pin::new(&mut buf));
        let (buf, fd) = buf.into_inner();
        let fd: Vec<_> = fd.into_iter().map(|fd| fd.into_raw_fd()).collect();
        let mut de = super::Deserializer::new(&buf, &fd);
        let msg: wl_protocol::wayland::wl_display::v1::Request =
            de.deserialize().expect("Failed to deserialize");
        assert_eq!(
            msg,
            wl_protocol::wayland::wl_display::v1::Request::Sync(
                wl_protocol::wayland::wl_display::v1::requests::Sync {
                    callback: wl_types::NewId(100),
                }
            )
        );
        assert_eq!(de.consumed(), (12, 0));
    }
}
