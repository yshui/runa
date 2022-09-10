use serde::de::{value, Deserializer, Error, Visitor};

/// When passed this string as name, deserialize_newtype_struct will try
/// to deserialize a file descriptor.
pub const WAYLAND_FD_NEWTYPE_NAME: &str = "__WaylandFd__";

pub struct WaylandDeserializer<'a> {
    read: &'a [u8],
}

impl<'a> WaylandDeserializer<'a> {
    pub fn new(read: &'a [u8]) -> Self {
        Self { read }
    }
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

impl<'de, 'b, 'a: 'de + 'b> Deserializer<'de> for &'b mut WaylandDeserializer<'a> {
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
    fn deserialize_newtype_struct<V>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        if name == WAYLAND_FD_NEWTYPE_NAME {
            Err(Self::Error::custom("not implemented"))
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
        struct Access<'a, 'b> {
            deserializer: &'b mut WaylandDeserializer<'a>,
            len: usize,
        }

        impl<'de, 'b, 'a: 'de + 'b> serde::de::SeqAccess<'de> for Access<'a, 'b> {
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
    fn deserialize_bytes<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
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
