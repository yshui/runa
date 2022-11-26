//! These are traits that are typically implemented by the server context
//! singleton, and related types and traits.

use std::{cell::RefCell, rc::Rc};

use derivative::Derivative;
use wl_common::Serial;

use crate::{events::EventHandle, globals::Bind, connection::ClientContext};

pub trait Server: Sized {
    /// The per client context type.
    type ClientContext: crate::connection::ClientContext<Context = Self>;
    type Conn;
    type Error;
    type Globals: Globals<Self::ClientContext>;
    type Global: Bind<Self::ClientContext, Objects = <Self::ClientContext as ClientContext>::Object>;

    fn globals(&self) -> &RefCell<Self::Globals>;
    fn new_connection(&self, conn: Self::Conn) -> Result<(), Self::Error>;
}

pub(crate) type GlobalOf<Ctx> = <<Ctx as ClientContext>::Context as Server>::Global;
pub trait Globals<Ctx: ClientContext> {
    type Iter<'a>: Iterator<Item = (u32, &'a Rc<GlobalOf<Ctx>>)> + 'a
    where
        Ctx: 'a,
        Self: 'a;
    /// Add a global to the store, return its allocated ID.
    fn insert(&mut self, global: impl Into<GlobalOf<Ctx>>) -> u32;
    /// Get the global with the given id.
    fn get(&self, id: u32) -> Option<&Rc<GlobalOf<Ctx>>>;
    /// Remove the global with the given id.
    fn remove(&mut self, id: u32) -> bool;
    fn iter(&self) -> Self::Iter<'_>;
    fn add_update_listener(&self, listener: (EventHandle, usize));
    fn remove_update_listener(&self, listener: (EventHandle, usize)) -> bool;
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct GlobalStore<Ctx: ClientContext> {
    globals:   wl_common::IdAlloc<Rc<GlobalOf<Ctx>>>,
    listeners: crate::events::Listeners,
}

impl<S: ClientContext> FromIterator<GlobalOf<S>> for GlobalStore<S> {
    fn from_iter<T: IntoIterator<Item = GlobalOf<S>>>(iter: T) -> Self {
        let mut id_alloc = wl_common::IdAlloc::default();
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
impl<Ctx: ClientContext> Globals<Ctx> for GlobalStore<Ctx> {
    type Iter<'a> = <wl_common::IdAlloc<Rc<GlobalOf<Ctx>>> as Serial>::Iter<'a> where Ctx: 'a, Self: 'a;

    fn insert(&mut self, global: impl Into<GlobalOf<Ctx>>) -> u32 {
        let id = self.globals.next_serial(Rc::new(global.into()));
        self.listeners.notify();
        id
    }

    fn get(&self, id: u32) -> Option<&Rc<GlobalOf<Ctx>>> {
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
