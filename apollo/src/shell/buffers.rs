use std::cell::Cell;

use wl_common::utils::geometry::{Extent, Logical};

/// The base buffer trait.
///
/// If you want to add a new type of buffer to use with this crate, you need to
/// provide a `&dyn Buffer` from [`ObjectMeta::provide`] function your buffer
/// object implements.
pub trait Buffer: 'static {
    // TODO: take rectangles
    fn damage(&self);
    fn clear_damage(&self);
    fn get_damage(&self) -> bool;
    // TODO: really logical?
    fn dimension(&self) -> Extent<u32, Logical>;
    /// Return the object id for the buffer object.
    /// Used for sending release event to the client.
    fn object_id(&self) -> u32;
}

pub trait HasBuffer {
    type Buffer: Buffer;
}

/// Buffer base
///
/// Various buffer implementations can choose to provide this struct as the
/// implementation of the wl_buffer interface.
///
/// All buffer implementations in this crate uses this.
#[derive(Debug)]
pub struct BufferBase {
    damaged: Cell<bool>,
    object_id: u32,
}

impl BufferBase {
    pub fn new(object_id: u32) -> Self {
        Self {
            damaged: Cell::new(false),
            object_id,
        }
    }
}

impl Buffer for BufferBase {
    #[inline]
    fn damage(&self) {
        self.damaged.set(true);
    }

    #[inline]
    fn clear_damage(&self) {
        self.damaged.set(false);
    }

    #[inline]
    fn get_damage(&self) -> bool {
        self.damaged.get()
    }

    #[inline]
    fn dimension(&self) -> Extent<u32, Logical> {
        unimplemented!()
    }
    #[inline]
    fn object_id(&self) -> u32 {
        self.object_id
    }
}

/// An enum of all buffer types defined in apollo.
#[derive(Debug)]
pub enum Buffers {
    Shm(crate::objects::shm::Buffer),
}

#[derive(Debug)]
pub struct RendererBuffer<Data> {
    pub buffer: Buffers,
    pub data:   Data,
}

impl From<crate::objects::shm::Buffer> for Buffers {
    fn from(buffer: crate::objects::shm::Buffer) -> Self {
        Self::Shm(buffer)
    }
}

impl Buffer for Buffers {
    #[inline]
    fn damage(&self) {
        match self {
            Self::Shm(buffer) => buffer.damage(),
        }
    }

    #[inline]
    fn clear_damage(&self) {
        match self {
            Self::Shm(buffer) => buffer.clear_damage(),
        }
    }

    #[inline]
    fn get_damage(&self) -> bool {
        match self {
            Self::Shm(buffer) => buffer.get_damage(),
        }
    }

    #[inline]
    fn dimension(&self) -> Extent<u32, Logical> {
        match self {
            Self::Shm(buffer) => buffer.dimension(),
        }
    }
    #[inline]
    fn object_id(&self) -> u32 {
        match self {
            Self::Shm(buffer) => buffer.object_id(),
        }
    }
}

impl<T, Data: Default> From<T> for RendererBuffer<Data>
where
    Buffers: From<T>,
{
    fn from(value: T) -> Self {
        Self {
            buffer: value.into(),
            data:   Default::default(),
        }
    }
}
impl<Data: 'static> Buffer for RendererBuffer<Data> {
    #[inline]
    fn damage(&self) {
        self.buffer.damage()
    }

    #[inline]
    fn clear_damage(&self) {
        self.buffer.clear_damage()
    }

    #[inline]
    fn get_damage(&self) -> bool {
        self.buffer.get_damage()
    }

    #[inline]
    fn dimension(&self) -> Extent<u32, Logical> {
        self.buffer.dimension()
    }
    #[inline]
    fn object_id(&self) -> u32 {
        self.buffer.object_id()
    }
}
