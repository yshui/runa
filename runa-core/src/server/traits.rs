//! Traits related to the server context

use std::{cell::RefCell, future::Future, rc::Rc};

use derive_where::derive_where;
use crate::{
    client::traits::Client,
    events::EventSource,
    globals::{AnyGlobal, Bind},
};

/// A server context
pub trait Server: Sized {
    /// The per client context type.
    type ClientContext: Client<ServerContext = Self>;
    /// Type of connection to the client, this is what the connection
    /// listener yields. For example,
    /// see [`wayland_listener_auto`](crate::wayland_listener_auto).
    type Conn;
    /// Type of error returned by [`Self::new_connection`].
    type Error;
    /// The global store
    type GlobalStore: GlobalStore<Self::Global>;
    /// Type of globals
    type Global: Bind<Self::ClientContext>
        + AnyGlobal<Object = <Self::ClientContext as Client>::Object>;

    /// Returns a reference to the global store.
    fn globals(&self) -> &RefCell<Self::GlobalStore>;

    /// Call each time a new client connects.
    fn new_connection(&self, conn: Self::Conn) -> Result<(), Self::Error>;
}

/// Global updates
///
/// Note, unlike [`StoreUpdate`](crate::client::traits::StoreUpdate), this
/// doesn't have a "Replaced" event to represent a global being added after
/// it's removed. This is because the global IDs are entirely controlled
/// by the compositor, and the compositor should avoid reusing global
/// IDs anyway, to avoid race condition with the client. So we could
/// assume ID reuse doesn't happen within a comfortably long time frame,
/// which should be long enough for event listeners to handle their
/// events.
#[derive_where(Clone)]
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

/// A global store
pub trait GlobalStore<G>: EventSource<GlobalsUpdate<G>> {
    /// Type of iterator returned by [`Self::iter`].
    type Iter<'a>: Iterator<Item = (u32, &'a Rc<G>)>
    where
        Self: 'a,
        G: 'a;
    /// Type of future returned by [`Self::insert`].
    type InsertFut<'a, IntoG>: Future<Output = u32> + 'a
    where
        Self: 'a,
        IntoG: 'a + Into<G>;
    /// Type of future returned by [`Self::remove`].
    type RemoveFut<'a>: Future<Output = bool> + 'a
    where
        Self: 'a;
    /// Add a global to the store, return its allocated ID.
    fn insert<'a, IntoG: Into<G> + 'a>(&'a mut self, global: IntoG) -> Self::InsertFut<'a, IntoG>;
    /// Get the global with the given id.
    fn get(&self, id: u32) -> Option<&Rc<G>>;
    /// Remove the global with the given id.
    fn remove(&mut self, id: u32) -> Self::RemoveFut<'_>;
    /// Iterate over all the globals.
    fn iter(&self) -> Self::Iter<'_>;
}
