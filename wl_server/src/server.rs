//! These are traits that are typically implemented by the server context
//! singleton, and related types and traits.

use std::{
    cell::{Cell, RefCell},
    marker::PhantomData,
    rc::Rc,
};

use hashbrown::{hash_map, HashMap};
use wl_common::Serial;

use crate::{connection::InterfaceMeta, globals::Global};

pub trait Server {
    /// The per client context type.
    type Connection: crate::connection::Connection<Context = Self> + 'static;
    type Globals: Globals<Self>;

    fn globals(&self) -> &Self::Globals;
}

pub trait EventSource {
    /// Add listener to get notified for events, If a handle with the same
    /// underlying pointer is added multiple times, it should only be
    /// notified once. But the EventSource should keep track of the number
    /// of times it was added, and only remove it when the number of times it
    /// was removed is equal to the number of times it was added.
    fn add_listener(&self, handle: &crate::events::EventFlags);
    /// Remove a listener
    fn remove_listener(&self, handle: &crate::events::EventFlags) -> bool;
    /// Notify all added listeners.
    fn notify(&self, slot: u8);
}

#[derive(Default)]
pub struct Listeners {
    listeners: RefCell<HashMap<crate::events::EventFlags, usize>>,
}

impl EventSource for Listeners {
    fn notify(&self, slot: u8) {
        for listener in self.listeners.borrow().keys() {
            listener.set(slot)
        }
    }

    fn add_listener(&self, handle: &crate::events::EventFlags) {
        self.listeners
            .borrow_mut()
            .entry(handle.clone())
            .and_modify(|x| *x += 1)
            .or_insert(1);
    }

    fn remove_listener(&self, handle: &crate::events::EventFlags) -> bool {
        match self.listeners.borrow_mut().entry(handle.clone()) {
            hash_map::Entry::Occupied(mut entry) => {
                let count = entry.get_mut();
                assert!(*count > 0);
                *count -= 1;
                if *count == 0 {
                    entry.remove();
                }
                true
            },
            hash_map::Entry::Vacant(_) => false,
        }
    }
}

pub trait Globals<S: Server + ?Sized>: EventSource {
    /// Add a global to the store, return its allocated ID.
    fn insert<T: Global<S> + 'static>(&self, global: T) -> u32;
    /// Get the global with the given id.
    fn get(&self, id: u32) -> Option<Rc<dyn Global<S>>>;
    fn with<F, R>(&self, id: u32, f: F) -> Option<R>
    where
        F: FnOnce(&Rc<dyn Global<S>>) -> R;
    fn for_each<F>(&self, f: F)
    where
        F: FnMut(u32, &Rc<dyn Global<S>>);
    /// Remove the global with the given id.
    fn remove(&self, id: u32) -> bool;
    /// Search the registry for a global with the given interface, return any if
    /// there are multiple. Should only be used for finding singletons, like
    /// wl_registry.
    fn get_by_interface(&self, interface: &str) -> Option<Rc<dyn Global<S>>> {
        self.map_by_interface(interface, Clone::clone)
    }
    fn map_by_interface<F, R>(&self, interface: &str, f: F) -> Option<R>
    where
        F: FnOnce(&Rc<dyn Global<S>>) -> R;
}

struct GlobalStoreInner<S: Server + ?Sized> {
    globals:   wl_common::IdAlloc<Rc<dyn Global<S>>>,
    listeners: Listeners,
}

pub struct GlobalStore<S: Server> {
    inner:   Rc<GlobalStoreInner<S>>,
    _server: PhantomData<S>,
}

impl<S: Server> Clone for GlobalStore<S> {
    fn clone(&self) -> Self {
        Self {
            inner:   self.inner.clone(),
            _server: PhantomData,
        }
    }
}

impl<S: Server> Default for GlobalStore<S> {
    fn default() -> Self {
        let globals = wl_common::IdAlloc::default();
        globals.next_serial(Rc::new(crate::globals::Display) as Rc<dyn Global<S>>);
        Self {
            inner:   Rc::new(GlobalStoreInner {
                globals,
                listeners: Default::default(),
            }),
            _server: PhantomData,
        }
    }
}

impl<S: Server> Globals<S> for GlobalStore<S> {
    fn insert<T: Global<S> + 'static>(&self, global: T) -> u32 {
        let id = self.inner.globals.next_serial(Rc::new(global));
        self.notify(crate::events::EventSlot::REGISTRY as u8);
        id
    }

    fn get(&self, id: u32) -> Option<Rc<dyn Global<S>>> {
        self.inner.globals.get(id)
    }

    fn with<F, R>(&self, id: u32, f: F) -> Option<R>
    where
        F: FnOnce(&Rc<dyn Global<S>>) -> R,
    {
        self.inner.globals.with(id, f)
    }

    fn for_each<F>(&self, f: F)
    where
        F: FnMut(u32, &Rc<dyn Global<S>>),
    {
        self.inner.globals.for_each(f)
    }

    fn remove(&self, id: u32) -> bool {
        let removed = self.inner.globals.expire(id);
        if removed {
            self.notify(crate::events::EventSlot::REGISTRY as u8);
        }
        removed
    }

    fn map_by_interface<F, R>(&self, interface: &str, f: F) -> Option<R>
    where
        F: FnOnce(&Rc<dyn Global<S>>) -> R,
    {
        let mut f = Some(f);
        self.inner.globals.find_map(move |g| {
            if g.interface() == interface {
                Some(f.take().unwrap()(g))
            } else {
                None
            }
        })
    }
}

impl<S: Server> EventSource for GlobalStore<S> {
    fn notify(&self, slot: u8) {
        self.inner.listeners.notify(slot)
    }

    fn add_listener(&self, handle: &crate::events::EventFlags) {
        // Always notify a new listener so it can scan the current available globals
        handle.set(crate::events::EventSlot::REGISTRY as u8);
        self.inner.listeners.add_listener(handle)
    }

    fn remove_listener(&self, handle: &crate::events::EventFlags) -> bool {
        self.inner.listeners.remove_listener(handle)
    }
}

impl<S: Server> std::fmt::Debug for GlobalStore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobalStore").finish()
    }
}
