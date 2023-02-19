//! These are traits that are typically implemented by the server context
//! singleton, and related types and traits.

use std::{cell::RefCell, future::Future, rc::Rc};

use derivative::Derivative;

use crate::{
    connection::traits::Client,
    events::{BroadcastEventSource, EventSource},
    globals::Global,
    Serial,
};

/// A server context.
///
/// A server context must be cloneable. Clone of the server context should
/// create another shared reference to the same server context. (i.e. work like
/// an Rc)
pub trait Server: Clone + Sized {
    /// The per client context type.
    type ClientContext: Client<ServerContext = Self>;
    type Conn;
    type Error;
    type GlobalStore: Globals<Self::Global>;
    type Global: Global<Self::ClientContext, Object = <Self::ClientContext as Client>::Object>;

    fn globals(&self) -> &RefCell<Self::GlobalStore>;
    fn new_connection(&self, conn: Self::Conn) -> Result<(), Self::Error>;
}

#[derive(Derivative)]
#[derivative(Clone(bound = ""))]
pub enum GlobalsUpdate<G> {
    /// A global was added.
    Added(u32, Rc<G>),
    /// A global was removed.
    Removed(u32),
}

impl<G> std::fmt::Debug for GlobalsUpdate<G> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GlobalsUpdate::Added(id, _) => f.debug_tuple("Added").field(id).field(&"...").finish(),
            GlobalsUpdate::Removed(id) => f.debug_tuple("Removed").field(id).finish(),
        }
    }
}

pub trait Globals<G>: EventSource<GlobalsUpdate<G>> {
    type Iter<'a>: Iterator<Item = (u32, &'a Rc<G>)>
    where
        Self: 'a,
        G: 'a;
    type InsertFut<'a, IntoG>: Future<Output = u32> + 'a
    where
        Self: 'a,
        IntoG: 'a + Into<G>;
    type RemoveFut<'a>: Future<Output = bool> + 'a
    where
        Self: 'a;
    /// Add a global to the store, return its allocated ID.
    fn insert<'a, IntoG: Into<G> + 'a>(&'a mut self, global: IntoG) -> Self::InsertFut<'a, IntoG>;
    /// Get the global with the given id.
    fn get(&self, id: u32) -> Option<&Rc<G>>;
    /// Remove the global with the given id.
    fn remove(&mut self, id: u32) -> Self::RemoveFut<'_>;
    fn iter(&self) -> Self::Iter<'_>;
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct GlobalStore<G> {
    globals:      crate::IdAlloc<Rc<G>>,
    update_event: BroadcastEventSource<GlobalsUpdate<G>>,
}

impl<G> FromIterator<G> for GlobalStore<G> {
    fn from_iter<T: IntoIterator<Item = G>>(iter: T) -> Self {
        let mut id_alloc = crate::IdAlloc::default();
        for global in iter.into_iter() {
            id_alloc.next_serial(Rc::new(global));
        }
        GlobalStore {
            globals:      id_alloc,
            update_event: Default::default(),
        }
    }
}

impl<G: 'static> EventSource<GlobalsUpdate<G>> for GlobalStore<G> {
    type Source = <BroadcastEventSource<GlobalsUpdate<G>> as EventSource<GlobalsUpdate<G>>>::Source;

    fn subscribe(&self) -> Self::Source {
        self.update_event.subscribe()
    }
}

/// GlobalStore will notify listeners when globals are added or removed. The
/// notification will be sent to the slot registered as "wl_registry".
impl<G: 'static> Globals<G> for GlobalStore<G> {
    type Iter<'a> = <&'a Self as IntoIterator>::IntoIter where Self: 'a, G: 'a;

    type InsertFut<'a, I> = impl Future<Output = u32> + 'a where Self: 'a, I: 'a + Into<G>;
    type RemoveFut<'a> = impl Future<Output = bool> + 'a where Self: 'a;

    fn insert<'a, I: Into<G> + 'a>(&'a mut self, global: I) -> Self::InsertFut<'_, I> {
        let global = Rc::new(global.into());
        let id = self.globals.next_serial(global.clone());
        async move {
            self.update_event
                .broadcast(GlobalsUpdate::Added(id, global))
                .await;
            id
        }
    }

    fn get(&self, id: u32) -> Option<&Rc<G>> {
        self.globals.get(id)
    }

    fn remove(&mut self, id: u32) -> Self::RemoveFut<'_> {
        let removed = self.globals.expire(id);
        async move {
            if removed {
                self.update_event
                    .broadcast(GlobalsUpdate::Removed(id))
                    .await;
            }
            removed
        }
    }

    fn iter(&self) -> Self::Iter<'_> {
        self.into_iter()
    }
}
impl<'a, G> IntoIterator for &'a GlobalStore<G> {
    type IntoIter = <&'a crate::IdAlloc<Rc<G>> as IntoIterator>::IntoIter;
    type Item = (u32, &'a Rc<G>);

    fn into_iter(self) -> Self::IntoIter {
        self.globals.iter()
    }
}
