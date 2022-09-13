use futures_lite::{ready, AsyncBufRead};
use serde::de::{value, Deserializer, Error, Visitor};
use std::{
    os::unix::io::RawFd,
    pin::Pin,
    ptr::NonNull,
    task::{Context, Poll},
};
use crate::AsyncBufReadWithFds;

/// When passed this string as name, deserialize_newtype_struct will try
/// to deserialize a file descriptor.
pub const WAYLAND_FD_NEWTYPE_NAME: &str = "__WaylandFd__";

/// Holds a value that is deserialized from borrowed data from `reader`.
/// This prevents the reader from being used again while this accessor is alive.
/// Also this accessor will advance the reader to the next message when dropped.
pub struct WaylandBufAccess<'a, R: AsyncBufReadWithFds + 'a> {
    read: &'a [u8],
    fds: &'a [RawFd],
    reader: NonNull<R>,
    fds_used: usize,
    read_len: usize,
}

impl<'a, R> WaylandBufAccess<'a, R>
where
    R: AsyncBufReadWithFds + 'a,
{
    /// Safety: result must be borrowing from `reader`, `reader` must be valid. (i.e. reader
    /// must outlive result). _And_ `reader` must have already been pinned
    pub unsafe fn new(reader: NonNull<R>, read: &'a [u8], fds: &'a [RawFd]) -> Self {
        Self {
            read,
            reader,
            fds,
            fds_used: 0,
            read_len: read.len(),
        }
    }

    pub fn into_inner(mut self) -> Pin<&'a mut R> {
        unsafe { Pin::new_unchecked(self.reader.as_mut()) }
    }
}

impl<'a, R> Drop for WaylandBufAccess<'a, R>
where
    R: AsyncBufReadWithFds + 'a,
{
    fn drop(&mut self) {
        // Safety: based on contrat of `new`, `reader` must outlive `result`, which we borrow.
        // And `reader` must be pinned already.
        unsafe {
            ::std::pin::Pin::new_unchecked(self.reader.as_mut()).consume_with_fds(self.read_len, self.fds_used);
        }
    }
}

impl<'a, R> WaylandBufAccess<'a, R>
where
    R: AsyncBufReadWithFds + 'a,
{
    /// Poll the reader until it has `len` bytes in the buffer. Then wrap
    /// the buffer and the reader in a `BufAccessor`. When the `BufAccessor`
    /// is dropped, the reader will be advanced by buf.len() bytes.
    pub fn poll_fill_buf_until(
        reader: Pin<&'a mut R>,
        cx: &mut Context<'_>,
        len: usize,
    ) -> Poll<std::io::Result<Self>> {
        // Safety:
        // 1. reader was pinned and we didn't move it.
        // 2. poll_fill_buf_until must originate from the raw reader pointer to not
        //    violate stacked borrow rules.
        // 3. Self has a 'a lifetime so it borrows reader
        unsafe {
            let mut reader = NonNull::from(reader.get_unchecked_mut());
            let (read, fds) =
                ready!(Pin::new_unchecked(reader.as_mut()).poll_fill_buf_until(cx, len))?;
            Poll::Ready(Ok(Self::new(reader, read, fds)))
        }
    }

    pub fn poll_next_message(
        mut reader: Pin<&'a mut R>,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<(u32, u32, Self)>> {
        // Wait until we have the message header ready at least.
        let (object_id, opcode, len) = {
            let (buf, _) = ready!(reader.as_mut().poll_fill_buf_until(cx, 8))?;
            let object_id = buf
                .get(..4)
                .expect("Bug in poll_fill_buf_until implementation");
            // Safety: get is guaranteed to return a slice of 4 bytes.
            let object_id = unsafe { u32::from_ne_bytes(*(object_id.as_ptr() as *const [u8; 4])) };
            let header = buf
                .get(4..8)
                .expect("Bug in poll_fill_buf_until implementation");
            let header = unsafe { u32::from_ne_bytes(*(header.as_ptr() as *const [u8; 4])) };
            (object_id, header & 0xffff, (header >> 16) as usize)
        };
        // Make sure we have the whole message in the buffer first, because we are going to drop
        // the object id here, if we are not ready, returning Pending from this functino would
        // cause us to lose the object id.
        let b = ready!(reader.as_mut().poll_fill_buf_until(cx, len))?;
        eprintln!("bbb {:x?}", b);
        // Drop object_id
        reader.as_mut().consume_with_fds(4, 0);
        Poll::Ready(match Self::poll_fill_buf_until(reader, cx, len - 4) {
            Poll::Ready(v) => v.map(|a|  {
                eprintln!("{:x?}", a.read);
                (object_id, opcode, a)
            }),
            Poll::Pending => unsafe { std::hint::unreachable_unchecked() },
        })
    }
}

impl<'a, R> WaylandBufAccess<'a, R>
where
    R: AsyncBufReadWithFds + 'a,
{
    fn pop_i32(&mut self) -> Result<i32, value::Error> {
        let slice = self
            .read
            .get(..4)
            .ok_or_else(|| value::Error::custom("Unexpected end of buffer"))?;
        // Safety: slice is guaranteed to be 4 bytes long
        let ret = i32::from_ne_bytes(unsafe { *(slice.as_ptr() as *const [u8; 4]) });
        self.read = &self.read[4..];
        Ok(ret)
    }
    fn pop_u32(&mut self) -> Result<u32, value::Error> {
        let slice = self
            .read
            .get(..4)
            .ok_or_else(|| value::Error::custom("Unexpected end of buffer"))?;
        // Safety: slice is guaranteed to be 4 bytes long
        let ret = u32::from_ne_bytes(unsafe { *(slice.as_ptr() as *const [u8; 4]) });
        self.read = &self.read[4..];
        Ok(ret)
    }
}

macro_rules! not_implemented_deserialize {
    ($($t:ident),*) => {
        $(
            fn $t<V>(self, _: V) -> Result<V::Value, Self::Error>
            where V: Visitor<'de>
            {
                Err(Self::Error::custom("not implemented"))
            }
        )*
    };
    ($($t:ident),*,) => {
        not_implemented_deserialize!($($t),*);
    }
}

/// Supports deserializing an opcode from the header, into a enum variant
struct WaylandHeaderDerserializer(u32);

impl<'de> Deserializer<'de> for WaylandHeaderDerserializer {
    type Error = serde::de::value::Error;
    not_implemented_deserialize!(
        deserialize_any,
        deserialize_bool,
        deserialize_i8,
        deserialize_i16,
        deserialize_i32,
        deserialize_i64,
        deserialize_u8,
        deserialize_u16,
        deserialize_u32,
        deserialize_u64,
        deserialize_f32,
        deserialize_f64,
        deserialize_char,
        deserialize_str,
        deserialize_string,
        deserialize_byte_buf,
        deserialize_option,
        deserialize_unit,
        deserialize_seq,
        deserialize_map,
        deserialize_ignored_any,
    );
    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_u64((self.0 & 0xffff) as u64)
    }
    fn deserialize_enum<V>(
        self,
        _name: &str,
        _variants: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        Err(Self::Error::custom("not implemented"))
    }
    fn deserialize_unit_struct<V>(
        self,
        _name: &'static str,
        _visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        Err(Self::Error::custom("not implemented"))
    }
    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        Err(Self::Error::custom("not implemented"))
    }

    fn deserialize_tuple<V>(self, _len: usize, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(Self::Error::custom("not implemented"))
    }
    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        _visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        Err(Self::Error::custom("not implemented"))
    }
    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        _visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        Err(Self::Error::custom("not implemented"))
    }
    fn deserialize_bytes<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        Err(Self::Error::custom("not implemented"))
    }
}

impl<'de, 'b, 'a: 'de + 'b, R> Deserializer<'de> for &'b mut WaylandBufAccess<'a, R>
where
    R: AsyncBufReadWithFds + 'a,
{
    type Error = serde::de::value::Error;
    not_implemented_deserialize!(
        deserialize_any,
        deserialize_bool,
        deserialize_i8,
        deserialize_i16,
        deserialize_i64,
        deserialize_u8,
        deserialize_u16,
        deserialize_u64,
        deserialize_f32,
        deserialize_f64,
        deserialize_char,
        deserialize_str,
        deserialize_string,
        deserialize_byte_buf,
        deserialize_option,
        deserialize_unit,
        deserialize_seq,
        deserialize_map,
        deserialize_identifier,
        deserialize_ignored_any,
    );
    fn deserialize_enum<V>(
        self,
        _name: &str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        impl<'de, 'b, 'a: 'de + 'b, R> serde::de::EnumAccess<'de> for &'b mut WaylandBufAccess<'a, R>
        where
            R: AsyncBufReadWithFds + 'a,
        {
            type Error = serde::de::value::Error;
            type Variant = Self;
            fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self::Variant), Self::Error>
            where
                V: serde::de::DeserializeSeed<'de>,
            {
                let variant = self.pop_u32()?;
                eprintln!("variant: {:x?}", variant);
                let variant = seed.deserialize(WaylandHeaderDerserializer(variant))?;
                Ok((variant, self))
            }
        }
        visitor.visit_enum(self)
    }
    fn deserialize_unit_struct<V>(
        self,
        _name: &'static str,
        _visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        Err(Self::Error::custom("not implemented"))
    }
    fn deserialize_newtype_struct<V>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        if name == WAYLAND_FD_NEWTYPE_NAME {
            let fd = *self
                .fds
                .get(0)
                .ok_or_else(|| Self::Error::custom("Not enough file descriptors"))?;
            self.fds = &self.fds[1..];
            self.fds_used += 1;
            visitor.visit_i32(fd as i32)
        } else {
            visitor.visit_newtype_struct(self)
        }
    }
    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_tuple(len, visitor)
    }
    fn deserialize_struct<V>(
        self,
        _name: &str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_tuple(fields.len(), visitor)
    }

    fn deserialize_tuple<V>(self, len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        struct Access<'a, 'b, R: AsyncBufReadWithFds + 'a> {
            deserializer: &'b mut WaylandBufAccess<'a, R>,
            len: usize,
        }

        impl<'de, 'b, 'a: 'de + 'b, R> serde::de::SeqAccess<'de> for Access<'a, 'b, R>
        where
            R: AsyncBufReadWithFds + 'a,
        {
            type Error = value::Error;

            fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
            where
                T: serde::de::DeserializeSeed<'de>,
            {
                if self.len > 0 {
                    self.len -= 1;
                    let value =
                        serde::de::DeserializeSeed::deserialize(seed, &mut *self.deserializer)?;
                    Ok(Some(value))
                } else {
                    Ok(None)
                }
            }

            fn size_hint(&self) -> Option<usize> {
                Some(self.len)
            }
        }

        visitor.visit_seq(Access {
            deserializer: self,
            len,
        })
    }
    fn deserialize_u32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_u32(self.pop_u32()?)
    }
    fn deserialize_i32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_i32(self.pop_i32()?)
    }
    fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let len = self.pop_u32()?;
        let read_len = (len + 3) & !3;
        if self.read.len() < read_len as usize {
            return Err(Self::Error::custom("Unexpected end of buffer"));
        }
        let (head, tail) = self.read.split_at(read_len as usize); // could use split_at_unchecked
        self.read = tail;
        visitor.visit_borrowed_bytes(&head[..len as usize]) // don't expose the padding bytes
    }
}
impl<'de, 'b, 'a: 'de + 'b, R> serde::de::VariantAccess<'de> for &'b mut WaylandBufAccess<'a, R>
where
    R: AsyncBufReadWithFds + 'a,
{
    type Error = serde::de::value::Error;
    fn unit_variant(self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, Self::Error>
    where
        T: serde::de::DeserializeSeed<'de>,
    {
        seed.deserialize(self)
    }
    fn tuple_variant<V>(self, len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.deserialize_tuple(len, visitor)
    }
    fn struct_variant<V>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.deserialize_tuple(fields.len(), visitor)
    }
}
