use std::{
    cell::{Cell, RefCell},
    pin::Pin,
};

use futures_lite::Future;
use hashbrown::{hash_map, HashMap};
pub use wl_io::de::Deserializer;
pub mod utils;

#[doc(hidden)]
pub mod __private {
    pub use ::wl_io::AsyncBufReadWithFd;
}

/// Event serial management.
///
/// This trait allocates serial numbers, while keeping track of allocated
/// numbers and their associated data.
///
/// Some expiration scheme might be employed by the implementation to free up
/// old serial numbers.
pub trait Serial {
    type Data: Clone;
    /// Get the next serial number in sequence. A piece of data can be attached
    /// to each serial, storing, for example, what this event is about.
    fn next_serial(&self, data: Self::Data) -> u32;
    /// Get the data associated with the given serial.
    fn get(&self, serial: u32) -> Option<Self::Data>;
    /// Non-cloning version of `get`.
    fn with<F, R>(&self, serial: u32, f: F) -> Option<R>
    where
        F: FnOnce(&Self::Data) -> R;
    fn for_each<F>(&self, f: F)
    where
        F: FnMut(u32, &Self::Data);
    /// Remove the serial number from the list of allocated serials.
    fn expire(&self, serial: u32) -> bool;
    fn find<F>(&self, mut f: F) -> Option<Self::Data>
    where
        F: FnMut(&Self::Data) -> bool,
    {
        self.find_map(|d| if f(d) { Some(d.clone()) } else { None })
    }
    fn find_map<F, R>(&self, f: F) -> Option<R>
    where
        F: FnMut(&Self::Data) -> Option<R>;
}

// TODO: impl Debug
pub struct IdAlloc<D> {
    next: Cell<u32>,
    data: RefCell<HashMap<u32, D>>,
}

impl<D> Default for IdAlloc<D> {
    fn default() -> Self {
        Self {
            // 0 is reserved for the null object
            next: Cell::new(1),
            data: RefCell::new(HashMap::new()),
        }
    }
}

impl<D: Clone> Serial for IdAlloc<D> {
    type Data = D;

    fn next_serial(&self, data: Self::Data) -> u32 {
        loop {
            // We could wrap around, so check for used IDs.
            // If the occupation rate is high, this could be slow. But IdAlloc is used for
            // things like allocating client/object IDs, so we expect at most a
            // few thousand IDs used at a time, out of 4 billion available.
            let id = self.next.get();
            self.next.set(id + 1);
            match self.data.borrow_mut().entry(id) {
                hash_map::Entry::Vacant(e) => {
                    e.insert(data);
                    break id
                },
                hash_map::Entry::Occupied(_) => (),
            }
        }
    }

    fn get(&self, serial: u32) -> Option<Self::Data> {
        self.data.borrow().get(&serial).cloned()
    }

    fn with<F, R>(&self, serial: u32, f: F) -> Option<R>
    where
        F: FnOnce(&Self::Data) -> R,
    {
        self.data.borrow().get(&serial).map(f)
    }

    fn for_each<F>(&self, mut f: F)
    where
        F: FnMut(u32, &Self::Data),
    {
        self.data.borrow().iter().for_each(|(a, b)| f(*a, b))
    }

    fn expire(&self, serial: u32) -> bool {
        self.data.borrow_mut().remove(&serial).is_some()
    }

    fn find_map<F, R>(&self, mut f: F) -> Option<R>
    where
        F: FnMut(&Self::Data) -> Option<R>,
    {
        self.data.borrow().values().find_map(|d| f(d))
    }
}

/// The entry point of a wayland application, either a client or a server.
pub trait MessageDispatch {
    type Error;
    type Fut<'a>: Future<Output = Result<(), Self::Error>> + 'a;
    fn dispatch<'a, R>(&self, reader: Pin<&mut R>) -> Self::Fut<'a>
    where
        R: wl_io::AsyncBufReadWithFd + 'a;
}

/// The entry point of an interface implementation, called when message of a
/// certain interface is received
pub trait InterfaceMessageDispatch<Ctx> {
    type Error;
    // TODO: the R parameter might be unnecessary, see:
    //       https://github.com/rust-lang/rust/issues/42940
    type Fut<'a, R>: Future<Output = Result<(), Self::Error>> + 'a
    where
        Self: 'a,
        Ctx: 'a,
        R: 'a + wl_io::AsyncBufReadWithFd;
    fn dispatch<'a, R>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        reader: &mut Deserializer<'a, R>,
    ) -> Self::Fut<'a, R>
    where
        R: wl_io::AsyncBufReadWithFd;
}

pub use wl_macros::{interface_message_dispatch, message_broker};

pub use std::convert::Infallible;
