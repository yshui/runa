//! These are traits that are typically implemented by the server context
//! singleton, and related types and traits.

use std::{cell::RefCell, marker::PhantomData, rc::Rc};
use wl_protocol::wayland::{wl_registry::v1 as wl_registry};

use hashbrown::{hash_map, HashMap};
use wl_common::Serial;

use crate::globals::Global;

pub trait Server {
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
    fn get_slots(&self) -> &[&'static str];
}

#[derive(Default)]
pub struct Listeners {
    listeners: RefCell<HashMap<crate::events::EventHandle, usize>>,
    slots:     Vec<&'static str>,
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

    fn get_slots(&self) -> &[&'static str] {
        &self.slots
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
    inner:               GlobalStoreInner<S>,
    registry_event_slot: Option<u8>,
    _server:             PhantomData<S>,
}

pub struct GlobalStoreBuilder<S: Server> {
    globals:     Vec<Box<dyn Global<S>>>,
    event_slots: Vec<&'static str>,
}

impl<S: Server> Default for GlobalStoreBuilder<S> {
    fn default() -> Self {
        Self {
            globals:     Vec::new(),
            event_slots: Vec::new(),
        }
    }
}

impl<S: Server> GlobalStoreBuilder<S> {
    pub fn global(&mut self, global: impl Global<S> + 'static) -> &mut Self {
        self.globals.push(Box::new(global));
        self
    }

    pub fn event_slot(&mut self, event: &'static str) -> &mut Self {
        self.event_slots.push(event);
        self
    }

    pub fn build(self) -> GlobalStore<S> {
        let Self {
            globals,
            event_slots,
        } = self;
        let registry_event_slot = event_slots
            .iter()
            .position(|x| *x == wl_registry::NAME)
            .map(|x| x as u8);
        let id_alloc = wl_common::IdAlloc::default();
        let listeners = Listeners {
            listeners: RefCell::new(HashMap::new()),
            slots:     event_slots,
        };
        id_alloc.next_serial(Rc::new(crate::globals::Display) as Rc<dyn Global<S>>);
        for global in globals {
            id_alloc.next_serial(global.into());
        }
        GlobalStore {
            inner: GlobalStoreInner {
                globals: id_alloc,
                listeners,
            },
            registry_event_slot,
            _server: PhantomData,
        }
    }
}

impl<S: Server> Globals<S> for GlobalStore<S> {
    fn insert<T: Global<S> + 'static>(&self, global: T) -> u32 {
        let id = self.inner.globals.next_serial(Rc::new(global));
        if let Some(slot) = self.registry_event_slot {
            self.notify(slot);
        }
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
            if let Some(slot) = self.registry_event_slot {
                self.notify(slot);
            }
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

/// EventSource implementation for GlobalStore. GlobalStore will notify
/// listeners when globals are added or removed. The notification will be sent
/// to the slot registered as "wl_registry".
impl<S: Server> EventSource for GlobalStore<S> {
    fn notify(&self, slot: u8) {
        self.inner.listeners.notify(slot)
    }

    fn add_listener(&self, handle: crate::events::EventHandle) {
        // Always notify a new listener so it can scan the current available globals
        if let Some(slot) = self.registry_event_slot {
            handle.set(slot);
        }
        self.inner.listeners.add_listener(handle)
    }

    fn remove_listener(&self, handle: crate::events::EventHandle) -> bool {
        self.inner.listeners.remove_listener(handle)
    }

    fn get_slots(&self) -> &[&'static str] {
        self.inner.listeners.get_slots()
    }
}

impl<S: Server> std::fmt::Debug for GlobalStore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobalStore").finish()
    }
}
