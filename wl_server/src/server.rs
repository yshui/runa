//! These are traits that are typically implemented by the server context
//! singleton, and related types and traits.

use std::{cell::RefCell, rc::Rc};

use derivative::Derivative;

use crate::{events::EventHandle, globals::Global, Serial};

pub trait Server: Sized {
    /// The per client context type.
    type ClientContext: crate::connection::Client<ServerContext = Self>;
    type Conn;
    type Error;
    type GlobalStore: Globals<Self::Global>;
    type Global: Global<Self::ClientContext>;

    fn globals(&self) -> &RefCell<Self::GlobalStore>;
    fn new_connection(&self, conn: Self::Conn) -> Result<(), Self::Error>;
}

pub trait Globals<G> {
    type Iter<'a>: Iterator<Item = (u32, &'a Rc<G>)> + 'a
    where
        G: 'a,
        Self: 'a;
    /// Add a global to the store, return its allocated ID.
    fn insert(&mut self, global: impl Into<G>) -> u32;
    /// Get the global with the given id.
    fn get(&self, id: u32) -> Option<&Rc<G>>;
    /// Remove the global with the given id.
    fn remove(&mut self, id: u32) -> bool;
    fn iter(&self) -> Self::Iter<'_>;
    fn add_update_listener(&self, listener: (EventHandle, usize));
    fn remove_update_listener(&self, listener: (EventHandle, usize)) -> bool;
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct GlobalStore<G> {
    globals:   crate::IdAlloc<Rc<G>>,
    listeners: crate::events::Listeners,
}

impl<G> FromIterator<G> for GlobalStore<G> {
    fn from_iter<T: IntoIterator<Item = G>>(iter: T) -> Self {
        let mut id_alloc = crate::IdAlloc::default();
        for global in iter.into_iter() {
            id_alloc.next_serial(Rc::new(global));
        }
        GlobalStore {
            globals:   id_alloc,
            listeners: Default::default(),
        }
    }
}

/// GlobalStore will notify listeners when globals are added or removed. The
/// notification will be sent to the slot registered as "wl_registry".
impl<G> Globals<G> for GlobalStore<G> {
    type Iter<'a> = <crate::IdAlloc<Rc<G>> as Serial>::Iter<'a> where G: 'a, Self: 'a;

    fn insert(&mut self, global: impl Into<G>) -> u32 {
        let id = self.globals.next_serial(Rc::new(global.into()));
        self.listeners.notify();
        id
    }

    fn get(&self, id: u32) -> Option<&Rc<G>> {
        self.globals.get(id)
    }

    fn remove(&mut self, id: u32) -> bool {
        let removed = self.globals.expire(id);
        if removed {
            self.listeners.notify();
        }
        removed
    }

    fn iter(&self) -> Self::Iter<'_> {
        self.globals.iter()
    }

    fn add_update_listener(&self, handle: (crate::events::EventHandle, usize)) {
        self.listeners.add_listener(handle);
    }

    fn remove_update_listener(&self, handle: (crate::events::EventHandle, usize)) -> bool {
        self.listeners.remove_listener(handle)
    }
}
