use std::{cell::Cell, fmt::Debug, rc::Rc};

use derivative::Derivative;
use runa_core::events::{single_state, EventSource};

use crate::utils::geometry::{coords, Extent};

/// The base buffer trait.
pub trait BufferLike: EventSource<BufferEvent> + Debug + 'static {
    // TODO: take rectangles
    fn damage(&self);
    fn clear_damage(&self);
    fn get_damage(&self) -> bool;
    fn dimension(&self) -> Extent<u32, coords::Buffer>;
    /// Return the object id for the buffer object.
    /// Used for sending release event to the client.
    fn object_id(&self) -> u32;

    /// Send the Released event for this buffer. After this is called, the
    /// buffer should prevent any further access of the client resources.
    /// Repeated calls to this function without intervening calls to
    /// [`BufferLike::acquire`] are allowed, but should have no effect.
    ///
    /// When a buffer is just created, it is in the released state.
    fn release(&self);

    /// Acquire the buffer so it can be read.
    fn acquire(&self);
}

#[derive(Debug)]
pub(crate) struct AttachedBuffer<B: BufferLike> {
    pub(crate) inner: Rc<B>,
    attach_count:     Rc<Cell<u64>>,
}

impl<B: BufferLike> Clone for AttachedBuffer<B> {
    fn clone(&self) -> Self {
        let count = self.attach_count.get();
        self.attach_count.set(count + 1);
        Self {
            inner:        self.inner.clone(),
            attach_count: self.attach_count.clone(),
        }
    }
}

impl<B: BufferLike> PartialEq for AttachedBuffer<B> {
    fn eq(&self, other: &Self) -> bool {
        let eq = Rc::ptr_eq(&self.inner, &other.inner);
        assert_eq!(eq, Rc::ptr_eq(&self.attach_count, &other.attach_count));
        eq
    }
}

impl<B: BufferLike> Eq for AttachedBuffer<B> {}

impl<B: BufferLike> Drop for AttachedBuffer<B> {
    fn drop(&mut self) {
        let count = self.attach_count.get();
        self.attach_count.set(count - 1);
        if count == 1 {
            self.inner.release();
        }
    }
}

/// A wrapper of buffer to automatically keep track of how many surface
/// states a buffer is attached to, and automatically release the buffer when
/// the count reaches 0.
#[derive(Derivative, Debug)]
#[derivative(Clone(bound = ""))]
pub(crate) struct AttachableBuffer<B> {
    pub(crate) inner: Rc<B>,
    attach_count:     Rc<Cell<u64>>,
}

impl<B: BufferLike> From<B> for AttachableBuffer<B> {
    fn from(inner: B) -> Self {
        Self {
            inner:        Rc::new(inner),
            attach_count: Rc::new(Cell::new(0)),
        }
    }
}

impl<B: BufferLike> AttachableBuffer<B> {
    pub fn new(inner: B) -> Self {
        Self::from(inner)
    }

    pub fn attach(&self) -> AttachedBuffer<B> {
        let count = self.attach_count.get();
        self.attach_count.set(count + 1);
        AttachedBuffer {
            inner:        self.inner.clone(),
            attach_count: self.attach_count.clone(),
        }
    }
}

pub trait HasBuffer {
    type Buffer: BufferLike;
}

/// Buffer base
///
/// Various buffer implementations can choose to provide this struct as the
/// implementation of the wl_buffer interface.
///
/// All buffer implementations in this crate uses this.
#[derive(Debug)]
pub struct Buffer<Base> {
    damaged:      Cell<bool>,
    object_id:    u32,
    base:         Base,
    dimension:    Extent<u32, coords::Buffer>,
    is_released:  Cell<bool>,
    event_source: single_state::Sender<BufferEvent>,
}

impl<Base> Buffer<Base> {
    pub fn new(dimension: Extent<u32, coords::Buffer>, object_id: u32, inner: Base) -> Self {
        Self {
            damaged: Cell::new(false),
            object_id,
            base: inner,
            dimension,
            is_released: Cell::new(true),
            event_source: Default::default(),
        }
    }

    /// Get a reference to the base type
    pub fn base(&self) -> &Base {
        assert!(!self.is_released.get());
        &self.base
    }

    /// Get a mutable reference to the base type
    pub fn base_mut(&mut self) -> &mut Base {
        assert!(!self.is_released.get());
        &mut self.base
    }

    /// Map the base type of the buffer. Note that already created event sources
    /// attached to the origin buffer, are still be attached to the
    /// resultant buffer.
    pub fn map_base<U, F: FnOnce(Base) -> U>(self, f: F) -> Buffer<U> {
        let Self {
            damaged,
            object_id,
            base,
            dimension,
            is_released,
            event_source,
        } = self;
        Buffer {
            damaged,
            object_id,
            base: f(base),
            dimension,
            is_released,
            event_source,
        }
    }
}

impl<Base> EventSource<BufferEvent> for Buffer<Base> {
    type Source = single_state::Receiver<BufferEvent>;

    fn subscribe(&self) -> Self::Source {
        self.event_source.new_receiver()
    }
}

impl<Base: Debug + 'static> BufferLike for Buffer<Base> {
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
    fn dimension(&self) -> Extent<u32, coords::Buffer> {
        self.dimension
    }

    #[inline]
    fn object_id(&self) -> u32 {
        self.object_id
    }

    #[inline]
    fn release(&self) {
        if !self.is_released.get() {
            tracing::trace!("Releasing buffer {}", self.object_id);
            self.is_released.set(true);
            self.event_source.send(BufferEvent::Released);
        }
    }

    #[inline]
    fn acquire(&self) {
        tracing::trace!("Acquiring buffer {}", self.object_id);
        self.is_released.set(false);
    }
}

/// An empty private trait just to make enum_dispatch generate the `From` impls.
#[enum_dispatch::enum_dispatch]
trait BufferBaseFrom {}

/// An enum of all buffer base types defined in this crate.
#[derive(Debug)]
#[enum_dispatch::enum_dispatch(BufferBaseFrom)]
pub enum BufferBase {
    Shm(crate::objects::shm::BufferBase),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BufferEvent {
    Released,
}

/// A buffer with additional user data.
#[derive(Debug)]
pub struct UserBuffer<B, Data> {
    buffer:   B,
    pub data: Data,
}

impl<B, Data> UserBuffer<B, Data> {
    pub fn buffer(&self) -> &B {
        &self.buffer
    }

    pub fn map_buffer<U, F: FnOnce(B) -> U>(self, f: F) -> UserBuffer<U, Data> {
        UserBuffer {
            buffer: f(self.buffer),
            data:   self.data,
        }
    }
}

impl<B, Data> UserBuffer<B, Data> {
    pub fn new(buffer: B, data: Data) -> Self {
        Self { buffer, data }
    }

    pub fn with_default_data(buffer: B) -> Self
    where
        Data: Default,
    {
        Self::new(buffer, Default::default())
    }
}

impl<U, T, Data: Default> From<Buffer<T>> for UserBuffer<Buffer<U>, Data>
where
    U: From<T>,
{
    fn from(value: Buffer<T>) -> Self {
        Self::with_default_data(value.map_base(U::from))
    }
}

impl<U: BufferLike, Data> EventSource<BufferEvent> for UserBuffer<U, Data> {
    type Source = <U as EventSource<BufferEvent>>::Source;

    fn subscribe(&self) -> Self::Source {
        self.buffer.subscribe()
    }
}

impl<B: BufferLike, Data: Debug + 'static> BufferLike for UserBuffer<B, Data> {
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
    fn dimension(&self) -> Extent<u32, coords::Buffer> {
        self.buffer.dimension()
    }

    #[inline]
    fn object_id(&self) -> u32 {
        self.buffer.object_id()
    }

    #[inline]
    fn release(&self) {
        self.buffer.release()
    }

    #[inline]
    fn acquire(&self) {
        self.buffer.acquire()
    }
}
