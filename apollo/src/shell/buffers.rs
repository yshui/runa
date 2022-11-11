use std::cell::Cell;

use wl_common::utils::geometry::{Extent, Logical};
use wl_protocol::wayland::wl_buffer::v1 as wl_buffer;

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
#[derive(Default)]
pub struct BufferBase {
    damaged: Cell<bool>,
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
}

/// An enum of all buffer types defined in apollo.
pub enum Buffers {
    Shm(crate::objects::shm::Buffer),
}

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
}
