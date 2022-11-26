#![feature(type_alias_impl_trait)]
use std::pin::Pin;

use futures_lite::Future;
use hashbrown::{hash_map, HashMap};
pub mod utils;

#[doc(hidden)]
pub mod __private {
    pub use ::wl_io_traits::buf::AsyncBufReadWithFd;
}

/// Event serial management.
///
/// This trait allocates serial numbers, while keeping track of allocated
/// numbers and their associated data.
///
/// Some expiration scheme might be employed by the implementation to free up
/// old serial numbers.
pub trait Serial {
    type Data;
    type Iter<'a>: Iterator<Item = (u32, &'a Self::Data)> + 'a
    where
        Self: 'a;
    /// Get the next serial number in sequence. A piece of data can be attached
    /// to each serial, storing, for example, what this event is about.
    fn next_serial(&mut self, data: Self::Data) -> u32;
    /// Get the data associated with the given serial.
    fn get(&self, serial: u32) -> Option<&Self::Data>;
    fn iter(&self) -> Self::Iter<'_>;
    /// Remove the serial number from the list of allocated serials.
    fn expire(&mut self, serial: u32) -> bool;
}

pub struct IdAlloc<D> {
    next: u32,
    data: HashMap<u32, D>,
}

impl<D> Default for IdAlloc<D> {
    fn default() -> Self {
        Self {
            // 0 is reserved for the null object
            next: 1,
            data: HashMap::new(),
        }
    }
}

impl<D> std::fmt::Debug for IdAlloc<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct DebugMap<'a, K, V>(&'a HashMap<K, V>);
        impl<'a, K: std::fmt::Debug, V> std::fmt::Debug for DebugMap<'a, K, V> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_set().entries(self.0.keys()).finish()
            }
        }
        f.debug_struct("IdAlloc")
            .field("next", &self.next)
            .field("data", &DebugMap(&self.data))
            .finish()
    }
}

impl<D> Serial for IdAlloc<D> {
    type Data = D;

    type Iter<'a> = impl Iterator<Item = (u32, &'a Self::Data)> + 'a where Self: 'a;

    fn iter(&self) -> Self::Iter<'_> {
        self.data.iter().map(|(k, v)| (*k, v))
    }

    fn next_serial(&mut self, data: Self::Data) -> u32 {
        loop {
            // We could wrap around, so check for used IDs.
            // If the occupation rate is high, this could be slow. But IdAlloc is used for
            // things like allocating client/object IDs, so we expect at most a
            // few thousand IDs used at a time, out of 4 billion available.
            let id = self.next;
            self.next += 1;
            match self.data.entry(id) {
                hash_map::Entry::Vacant(e) => {
                    e.insert(data);
                    break id
                },
                hash_map::Entry::Occupied(_) => (),
            }
        }
    }

    fn get(&self, serial: u32) -> Option<&Self::Data> {
        self.data.get(&serial)
    }

    fn expire(&mut self, serial: u32) -> bool {
        self.data.remove(&serial).is_some()
    }
}

/// The entry point of a wayland application, either a client or a server.
pub trait MessageDispatch {
    type Error;
    type Fut<'a>: Future<Output = Result<(), Self::Error>> + 'a;
    fn dispatch<'a, R>(&self, reader: Pin<&mut R>) -> Self::Fut<'a>
    where
        R: wl_io_traits::buf::AsyncBufReadWithFd + 'a;
}

/// The entry point of an interface implementation, called when message of a
/// certain interface is received
pub trait InterfaceMessageDispatch<Ctx> {
    type Error;
    // TODO: the R parameter might be unnecessary, see:
    //       https://github.com/rust-lang/rust/issues/42940
    type Fut<'a, D>: Future<Output = Result<(), Self::Error>> + 'a
    where
        Self: 'a,
        Ctx: 'a,
        D: wl_io_traits::de::Deserializer<'a> + 'a;
    fn dispatch<'a, D: wl_io_traits::de::Deserializer<'a> + 'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        reader: D,
    ) -> Self::Fut<'a, D>;
}

pub use std::convert::Infallible;

pub use wl_macros::interface_message_dispatch;
pub use wl_macros::InterfaceMessageDispatch;
