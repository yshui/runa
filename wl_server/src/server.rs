//! These are traits that are typically implemented by the server context
//! singleton, and related types and traits.

use std::{cell::RefCell, marker::PhantomData, rc::Rc};

use hashbrown::{hash_map, HashMap};
use wl_common::Serial;
use wl_protocol::wayland::wl_registry::v1 as wl_registry;

use crate::globals::Global;

pub trait Server: EventSource {
    /// The per client context type.
    type Connection: crate::connection::Connection<Context = Self> + 'static;
    type Globals: Globals<Self>;
    type Builder: ServerBuilder<Output = Self>;

    fn globals(&self) -> &Self::Globals;
    fn builder() -> Self::Builder;
}

pub trait ServerBuilder {
    type Output: Server;
    /// Add a new global to the server
    fn global(&mut self, global: impl Global<Self::Output> + 'static) -> &mut Self;
    /// Add a new event slot to the server
    fn event_slot(&mut self, event: &'static str) -> &mut Self;
    /// Create the server object
    fn build(self) -> Self::Output;
}

pub trait EventSource {
    /// Add listener to get notified for events, If a handle with the same
    /// underlying pointer is added multiple times, it should only be
    /// notified once. But the EventSource should keep track of the number
    /// of times it was added, and only remove it when the number of times it
    /// was removed is equal to the number of times it was added.
    fn add_listener(&self, handle: crate::events::EventHandle);
    /// Remove a listener
    fn remove_listener(&self, handle: crate::events::EventHandle) -> bool;
    /// Notify all added listeners.
    fn notify(&self, slot: u8);
    /// Get information about allocated slots. Slots are fixed once an event
    /// source object is created. See [`ServerBuilder::event_slot`].
    fn slots(&self) -> &[&'static str];
}

#[derive(Default, Debug)]
pub struct Listeners {
    listeners: RefCell<HashMap<crate::events::EventHandle, usize>>,
    slots:     Vec<&'static str>,
}

impl Listeners {
    pub fn new(slots: Vec<&'static str>) -> Self {
        Self { slots, ..Default::default() }
    }
}

impl EventSource for Listeners {
    fn notify(&self, slot: u8) {
        self.listeners.borrow_mut().retain(|handle, _| {
            // Notify the listener, also check if the listen is still alive, removing all
            // the handles that has died.
            handle.set(slot)
        });
    }

    fn add_listener(&self, handle: crate::events::EventHandle) {
        self.listeners
            .borrow_mut()
            .entry(handle.clone())
            .and_modify(|x| *x += 1)
            .or_insert(1);
    }

    fn remove_listener(&self, handle: crate::events::EventHandle) -> bool {
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

    fn slots(&self) -> &[&'static str] {
        &self.slots
    }
}

pub trait Globals<S: Server + EventSource + ?Sized> {
    /// Add a global to the store, return its allocated ID.
    fn insert<T: Global<S> + 'static>(&self, server: &S, global: T) -> u32;
    /// Get the global with the given id.
    fn get(&self, id: u32) -> Option<Rc<dyn Global<S>>>;
    fn with<F, R>(&self, id: u32, f: F) -> Option<R>
    where
        F: FnOnce(&Rc<dyn Global<S>>) -> R;
    fn for_each<F>(&self, f: F)
    where
        F: FnMut(u32, &Rc<dyn Global<S>>);
    /// Remove the global with the given id.
    fn remove(&self, server: &S, id: u32) -> bool;
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

pub struct GlobalStore<S: Server> {
    globals:   wl_common::IdAlloc<Rc<dyn Global<S>>>,
    registry_event_slot: Option<u8>,
    _server:             PhantomData<S>,
}

pub struct GlobalStoreBuilder<S: Server> {
    globals:             Vec<Box<dyn Global<S>>>,
    registry_event_slot: Option<u8>,
}

impl<S: Server> Default for GlobalStoreBuilder<S> {
    fn default() -> Self {
        Self {
            globals:             Vec::new(),
            registry_event_slot: None,
        }
    }
}

impl<S: Server> GlobalStoreBuilder<S> {
    pub fn global(&mut self, global: impl Global<S> + 'static) -> &mut Self {
        self.globals.push(Box::new(global));
        self
    }

    pub fn registry_event_slot(&mut self, slot: u8) -> &mut Self {
        self.registry_event_slot = Some(slot);
        self
    }

    pub fn build(self) -> GlobalStore<S> {
        let Self {
            globals,
            registry_event_slot,
        } = self;
        let id_alloc = wl_common::IdAlloc::default();
        id_alloc.next_serial(Rc::new(crate::globals::Display) as Rc<dyn Global<S>>);
        for global in globals {
            id_alloc.next_serial(global.into());
        }
        GlobalStore {
            globals: id_alloc,
            registry_event_slot,
            _server: PhantomData,
        }
    }
}


/// GlobalStore will notify listeners when globals are added or removed. The notification will be
/// sent to the slot registered as "wl_registry".
impl<S: Server + EventSource> Globals<S> for GlobalStore<S> {
    fn insert<T: Global<S> + 'static>(&self, server: &S, global: T) -> u32
    where
        S: EventSource,
    {
        let id = self.globals.next_serial(Rc::new(global));
        if let Some(slot) = self.registry_event_slot {
            server.notify(slot);
        }
        id
    }

    fn get(&self, id: u32) -> Option<Rc<dyn Global<S>>> {
        self.globals.get(id)
    }

    fn with<F, R>(&self, id: u32, f: F) -> Option<R>
    where
        F: FnOnce(&Rc<dyn Global<S>>) -> R,
    {
        self.globals.with(id, f)
    }

    fn for_each<F>(&self, f: F)
    where
        F: FnMut(u32, &Rc<dyn Global<S>>),
    {
        self.globals.for_each(f)
    }

    fn remove(&self, server: &S, id: u32) -> bool
    where
        S: EventSource,
    {
        let removed = self.globals.expire(id);
        if removed {
            if let Some(slot) = self.registry_event_slot {
                server.notify(slot);
            }
        }
        removed
    }

    fn map_by_interface<F, R>(&self, interface: &str, f: F) -> Option<R>
    where
        F: FnOnce(&Rc<dyn Global<S>>) -> R,
    {
        let mut f = Some(f);
        self.globals.find_map(move |g| {
            if g.interface() == interface {
                Some(f.take().unwrap()(g))
            } else {
                None
            }
        })
    }
}

impl<S: Server> std::fmt::Debug for GlobalStore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobalStore").finish()
    }
}
