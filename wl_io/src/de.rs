use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use std::task::ready;
use serde::de::{self, value, Deserializer as _, Error, Visitor};

use crate::{AsyncBufReadWithFd, AsyncReadWithFd};

/// When passed this string as name, deserialize_newtype_struct will try
/// to deserialize a file descriptor.
pub const WAYLAND_FD_NEWTYPE_NAME: &str = "\0__WaylandFd__";

/// Holds a value that is deserialized from borrowed data from `reader`.
/// This prevents the reader from being used again while this accessor is alive.
/// Also this accessor will advance the reader to the next message when dropped.
#[derive(Debug)]
pub struct Deserializer<'a, R> {
    // XXX: can't be &mut here, because borrowing from &'b &'a mut shortens the lifetime to 'b
    //      has to be &, but also need to be able to pop file descriptors.
    //      this requires RefCell inside the BufReaderWithFd??
    //      also can't own the BufReaderWithFd, otherwise we have trouble in SeqAccess
    reader:            &'a R,
    bytes_read:        usize,
    enum_deserialized: bool,
}

impl<'a, R> Deserializer<'a, R>
where
    R: AsyncBufReadWithFd,
{
    pub fn into_inner(self) -> &'a R {
        self.reader
    }

    pub fn poll_next_message(
        mut reader: Pin<&'a mut R>,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<(u32, usize, Self)>> {
        // Wait until we have the message header ready at least.
        let (object_id, len) = {
            ready!(reader.as_mut().poll_fill_buf_until(cx, 8))?;
            let object_id = reader
                .buffer()
                .get(..4)
                .expect("Bug in poll_fill_buf_until implementation");
            // Safety: get is guaranteed to return a slice of 4 bytes.
            let object_id = unsafe { u32::from_ne_bytes(*(object_id.as_ptr() as *const [u8; 4])) };
            let header = reader
                .buffer()
                .get(4..8)
                .expect("Bug in poll_fill_buf_until implementation");
            let header = unsafe { u32::from_ne_bytes(*(header.as_ptr() as *const [u8; 4])) };
            (object_id, (header >> 16) as usize)
        };

        ready!(reader.as_mut().poll_fill_buf_until(cx, len))?;
        Poll::Ready(Ok((object_id, len, Self {
            reader:            reader.into_ref().get_ref(),
            bytes_read:        0,
            enum_deserialized: false,
        })))
    }

    pub fn next_message(
        reader: Pin<&'a mut R>,
    ) -> impl Future<Output = std::io::Result<(u32, usize, Deserializer<'a, R>)>> + 'a {
        pub struct NextMessage<'a, R>(Option<Pin<&'a mut R>>);
        impl<'a, R> std::future::Future for NextMessage<'a, R>
        where
            R: AsyncBufReadWithFd,
        {
            type Output = std::io::Result<(u32, usize, Deserializer<'a, R>)>;

            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = self.get_mut();
                let mut reader = this.0.take().expect("NextMessage polled after completion");
                match Deserializer::poll_next_message(reader.as_mut(), cx) {
                    Poll::Pending => {
                        this.0 = Some(reader);
                        Poll::Pending
                    },
                    Poll::Ready(Ok(_)) => match Deserializer::poll_next_message(reader, cx) {
                        Poll::Pending => {
                            panic!("poll_next_message returned Ready, but then Pending again")
                        },
                        ready => ready,
                    },
                    Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                }
            }
        }
        NextMessage(Some(reader))
    }

    pub fn deserialize<'b, T>(&'b mut self) -> Result<T, serde::de::value::Error>
    where
        T: serde::de::Deserialize<'a>,
        R: Sized,
    {
        T::deserialize(self)
    }

    fn pop_bytes(&mut self, len: usize) -> Result<&'a [u8], serde::de::value::Error> {
        let offset = self.bytes_read;
        let buf = self.reader.buffer();
        if buf.len() < len {
            return Err(serde::de::value::Error::custom(
                "Not enough bytes in buffer",
            ))
        }
        let buf = &buf[offset..offset + len];
        self.bytes_read += len;
        Ok(buf)
    }

    fn pop_i32(&mut self) -> Result<i32, value::Error> {
        let slice = self.pop_bytes(4)?;
        // Safety: slice is guaranteed to be 4 bytes long
        let ret = i32::from_ne_bytes(unsafe { *(slice.as_ptr() as *const [u8; 4]) });
        Ok(ret)
    }

    fn pop_u32(&mut self) -> Result<u32, value::Error> {
        let slice = self.pop_bytes(4)?;
        // Safety: slice is guaranteed to be 4 bytes long
        let ret = u32::from_ne_bytes(unsafe { *(slice.as_ptr() as *const [u8; 4]) });
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
struct HeaderDerserializer(u32);

impl<'de> de::Deserializer<'de> for HeaderDerserializer {
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

impl<'de, 'a: 'de, R> de::Deserializer<'de> for &'_ mut Deserializer<'a, R>
where
    R: AsyncBufReadWithFd,
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
        impl<'de, 'a: 'de, R> serde::de::EnumAccess<'de> for &'_ mut Deserializer<'a, R>
        where
            R: AsyncBufReadWithFd,
        {
            type Error = serde::de::value::Error;
            type Variant = Self;

            fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self::Variant), Self::Error>
            where
                V: serde::de::DeserializeSeed<'de>,
            {
                let _object_id = self.pop_u32()?;
                let variant = self.pop_u32()?;
                let variant = seed.deserialize(HeaderDerserializer(variant))?;
                Ok((variant, self))
            }
        }
        if self.enum_deserialized {
            return Err(Self::Error::custom("Message header already deserialized"))
        }
        self.enum_deserialized = true;
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
            let fd = Pin::new(&mut self.reader).next_fd();
            if let Some(fd) = fd {
                use std::os::unix::io::IntoRawFd;
                visitor.visit_i32(fd.into_raw_fd())
            } else {
                Err(Self::Error::custom("No fd available"))
            }
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
        struct Access<'a, 'b, R: AsyncReadWithFd> {
            deserializer: &'b mut Deserializer<'a, R>,
            len:          usize,
        }

        impl<'de, 'a: 'de, R> serde::de::SeqAccess<'de> for Access<'a, '_, R>
        where
            R: AsyncBufReadWithFd,
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
        let buf = self.pop_bytes(read_len as usize)?;
        visitor.visit_borrowed_bytes(&buf[..len as usize]) // don't expose the
                                                           // padding bytes
    }
}
impl<'de, 'a: 'de, R> serde::de::VariantAccess<'de> for &'_ mut Deserializer<'a, R>
where
    R: AsyncBufReadWithFd,
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
