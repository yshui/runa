//! These are traits that are typically implemented by the server context
//! singleton, and related types and traits.

use std::{cell::RefCell, rc::Rc};

use wl_common::Serial;
use derivative::Derivative;

use crate::{events::EventHandle, globals::Global};

pub trait Server: Sized + 'static {
    /// The per client context type.
    type Connection: crate::connection::Connection<Context = Self> + 'static;
    type Globals: Globals<Self::Connection>;

    fn globals(&self) -> &RefCell<Self::Globals>;
}

pub trait Globals<Ctx> {
    type Iter<'a>: Iterator<Item = (u32, &'a Rc<dyn Global<Ctx>>)> + 'a
    where
        Ctx: 'a,
        Self: 'a;
    /// Add a global to the store, return its allocated ID.
    fn insert<T: Global<Ctx> + 'static>(&mut self, global: T) -> u32;
    /// Get the global with the given id.
    fn get(&self, id: u32) -> Option<&Rc<dyn Global<Ctx>>>;
    /// Remove the global with the given id.
    fn remove(&mut self, id: u32) -> bool;
    fn iter(&self) -> Self::Iter<'_>;
    fn add_update_listener(&self, listener: (EventHandle, usize));
    fn remove_update_listener(&self, listener: (EventHandle, usize)) -> bool;
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct GlobalStore<Ctx: 'static> {
    globals:   wl_common::IdAlloc<Rc<dyn Global<Ctx>>>,
    listeners: crate::events::Listeners,
}

impl<S: 'static> FromIterator<Box<dyn Global<S>>> for GlobalStore<S> {
    fn from_iter<T: IntoIterator<Item = Box<dyn Global<S>>>>(iter: T) -> Self {
        let mut id_alloc = wl_common::IdAlloc::default();
        for global in iter.into_iter() {
            id_alloc.next_serial(global.into());
        }
        GlobalStore {
            globals:   id_alloc,
            listeners: Default::default(),
        }
    }
}

/// GlobalStore will notify listeners when globals are added or removed. The
/// notification will be sent to the slot registered as "wl_registry".
impl<Ctx: 'static> Globals<Ctx> for GlobalStore<Ctx> {
    type Iter<'a> = <wl_common::IdAlloc<Rc<dyn Global<Ctx>>> as Serial>::Iter<'a> where Ctx: 'a, Self: 'a;

    fn insert<T: Global<Ctx>>(&mut self, global: T) -> u32 {
        let id = self.globals.next_serial(Rc::new(global));
        self.listeners.notify();
        id
    }

    fn get(&self, id: u32) -> Option<&Rc<dyn Global<Ctx>>> {
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
