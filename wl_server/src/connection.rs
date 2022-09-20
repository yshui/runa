//! This mod is for traits that are usually implemented by per client connection
//! context objects, and their related traits.
//!
//! Here are also some default implementations of these traits.

use std::{any::Any, cell::RefCell, collections::HashMap};

pub trait InterfaceMeta {
    /// Return the interface name of this object.
    fn interface(&self) -> &'static str;
    /// Case self to &dyn Any
    fn as_any(&self) -> &dyn Any;
    /// Return the global ID this object is a proxy of. None means this object
    /// is not a global.
    fn global(&self) -> Option<u32>;
    /// Mark this object as dead. The object is kept in place until the client
    /// drops it, and it should parse the messages sent by the client and
    /// handle them as no-ops.
    ///
    /// It's an error to call this on non-globals.
    fn global_remove(&self);
}

use std::rc::Rc;
/// Per client mapping from object ID to objects.
pub struct Store {
    map: RefCell<HashMap<u32, Rc<dyn InterfaceMeta>>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct DebugMap<'a>(&'a HashMap<u32, Rc<dyn InterfaceMeta>>);
        let map = self.map.borrow();
        let debug_map = DebugMap(&map);
        impl std::fmt::Debug for DebugMap<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_map()
                    .entries(self.0.iter().map(|(k, v)| (k, v.interface())))
                    .finish()
            }
        }
        f.debug_struct("Store").field("map", &debug_map).finish()
    }
}

pub trait Entry<'a> {
    fn is_vacant(&self) -> bool;
    fn or_insert_boxed(self, v: Box<dyn InterfaceMeta>) -> &'a mut Rc<dyn InterfaceMeta>;
}

impl<'a, K> Entry<'a> for std::collections::hash_map::Entry<'a, K, Rc<dyn InterfaceMeta>>
where
    K: std::cmp::Eq + std::hash::Hash,
{
    fn is_vacant(&self) -> bool {
        match self {
            std::collections::hash_map::Entry::Vacant(_) => true,
            std::collections::hash_map::Entry::Occupied(_) => false,
        }
    }

    fn or_insert_boxed(self, v: Box<dyn InterfaceMeta>) -> &'a mut Rc<dyn InterfaceMeta> {
        std::collections::hash_map::Entry::or_insert(self, v.into())
    }
}

/// A bundle of objects.
///
/// Usually this is the set of objects a client has bound to.
pub trait Objects {
    type Entry<'a>: Entry<'a>;
    /// Insert object into the store with the given ID. Returns Ok(()) if
    /// successful, Err(T) if the ID is already in use.
    fn insert<T: InterfaceMeta + 'static>(&self, id: u32, object: T) -> Result<(), T>;
    fn remove(&self, id: u32) -> Option<Rc<dyn InterfaceMeta>>;
    fn get(&self, id: u32) -> Option<Rc<dyn InterfaceMeta>>;
    /// Call the `global_remove` method on all objects whose `global` equals the
    /// given global ID.
    fn global_remove(&self, global: u32);
    fn with_entry<T>(&self, id: u32, f: impl FnOnce(Self::Entry<'_>) -> T) -> T;
}

impl Store {
    pub fn new() -> Self {
        Self {
            map: RefCell::new(HashMap::new()),
        }
    }
}

impl Objects for Store {
    type Entry<'a> = std::collections::hash_map::Entry<'a, u32, Rc<dyn InterfaceMeta>>;

    fn insert<T: InterfaceMeta + 'static>(&self, object_id: u32, object: T) -> Result<(), T> {
        match self.map.borrow_mut().entry(object_id) {
            std::collections::hash_map::Entry::Occupied(_) => Err(object),
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(Rc::new(object));
                Ok(())
            },
        }
    }

    fn remove(&self, object_id: u32) -> Option<Rc<dyn InterfaceMeta>> {
        self.map.borrow_mut().remove(&object_id)
    }

    fn get(&self, object_id: u32) -> Option<Rc<dyn InterfaceMeta>> {
        self.map.borrow().get(&object_id).map(Clone::clone)
    }

    fn global_remove(&self, global: u32) {
        for (_, obj) in self.map.borrow_mut().iter() {
            if obj.global() == Some(global) {
                obj.global_remove();
            }
        }
    }

    fn with_entry<T>(&self, id: u32, f: impl FnOnce(Self::Entry<'_>) -> T) -> T {
        f(self.map.borrow_mut().entry(id))
    }
}

/// A client connection
pub trait Connection {
    type Context;
    /// Return the server context singleton.
    fn server_context(&self) -> &Self::Context;
}

/// Event serial management.
///
/// This trait allocates serial numbers for events, while keeping track of them.
/// Some expiration scheme could to employed to free up old serial numbers.
pub trait Serial {
    type Data: Clone;
    /// Get the next serial number in sequence. A piece of data can be attached
    /// to each serial, storing, for example, what this event is about.
    fn next_serial(&self, data: Self::Data) -> u32;
    /// Get the data associated with the given serial.
    fn get_data(&self, serial: u32) -> Option<Self::Data>;
}

pub struct EventSerial<D> {
    serials:     RefCell<HashMap<u32, (D, std::time::Instant)>>,
    last_serial: RefCell<u32>,
    expire:      std::time::Duration,
}

impl<D> std::fmt::Debug for EventSerial<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct DebugMap<'a, D>(&'a HashMap<u32, D>);
        let map = self.serials.borrow();
        let debug_map = DebugMap(&map);
        impl<D> std::fmt::Debug for DebugMap<'_, D> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_map()
                    .entries(self.0.iter().map(|(k, _)| (k, "...")))
                    .finish()
            }
        }
        f.debug_struct("EventSerial")
            .field("serials", &debug_map)
            .field("last_serial", &self.last_serial.borrow())
            .finish()
    }
}

impl<D> EventSerial<D> {
    pub fn new(expire: std::time::Duration) -> Self {
        Self {
            serials: RefCell::new(HashMap::new()),
            last_serial: RefCell::new(0),
            expire,
        }
    }
}

impl<D: Clone> Serial for EventSerial<D> {
    type Data = D;

    fn next_serial(&self, data: Self::Data) -> u32 {
        let mut last_serial = self.last_serial.borrow_mut();
        *last_serial += 1;
        let serial = *last_serial;

        let mut serials = self.serials.borrow_mut();
        let now = std::time::Instant::now();
        serials.retain(|_, (_, t)| *t + self.expire > now);
        match serials.insert(serial, (data, std::time::Instant::now())) {
            Some(_) => {
                panic!("serial {} already in use", serial);
            },
            None => (),
        }
        serial
    }

    fn get_data(&self, serial: u32) -> Option<Self::Data> {
        let mut serials = self.serials.borrow_mut();
        let now = std::time::Instant::now();
        serials.retain(|_, (_, t)| *t + self.expire > now);
        serials.get(&serial).map(|(d, _)| d.clone())
    }
}
