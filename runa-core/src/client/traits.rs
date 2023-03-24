use std::{any::Any, error::Error, pin::Pin};

use futures_core::{Future, Stream};
use runa_io::traits::WriteMessage;

use crate::{events, objects};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StoreEventKind {
    /// An object is inserted into the store
    Inserted { interface: &'static str },
    /// An object is removed from the store
    Removed { interface: &'static str },
    /// An object is removed and replaced by another object with the same
    /// object ID, possibly multiple times
    Replaced {
        /// The interface this object ID started with. If this ID is being
        /// replaced multiple times, this is the very first
        /// interface.
        old_interface: &'static str,
        /// The interface this object ID currently have.
        new_interface: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoreEvent {
    pub object_id: u32,
    pub kind:      StoreEventKind,
}

#[derive(Debug, thiserror::Error)]
pub enum GetError {
    // The ID is not found
    #[error("Object ID {0} not found")]
    IdNotFound(u32),
    // The object is not of the requested type
    #[error("Object ID {0} is not of the requested type")]
    TypeMismatch(u32),
}

impl From<GetError> for crate::error::Error {
    fn from(e: GetError) -> Self {
        use GetError::*;
        match e {
            IdNotFound(id) => Self::UnknownObject(id),
            TypeMismatch(id) => Self::InvalidObject(id),
        }
    }
}

pub trait Store<O>: events::EventSource<StoreEvent> {
    /// See [`crate::utils::AsIteratorItem`] for why this is so complicated.
    type ByType<'a, T>: Iterator<Item = (u32, &'a T)> + 'a
    where
        O: 'a,
        T: objects::MonoObject + 'a,
        Self: 'a;
    type IdsByType<'a, T>: Iterator<Item = u32> + 'a
    where
        O: 'a,
        T: 'static,
        Self: 'a;
    /// Insert object into the store with the given ID. Returns a unique
    /// reference to the inserted object if successful, Err(T) if
    /// the ID is already in use.
    fn insert<T: Into<O> + 'static>(&mut self, id: u32, object: T) -> Result<&mut T, T>;

    /// Insert object into the store with the given ID. Returns a unique
    /// reference to the inserted object and its singleton state if
    /// successful, Err(T) if the ID is already in use.
    fn insert_with_state<T: Into<O> + objects::MonoObject + 'static>(
        &mut self,
        id: u32,
        object: T,
    ) -> Result<(&mut T, &mut T::SingletonState), T>;

    /// Allocate a new ID for the client, associate `object` for it.
    /// According to the wayland spec, the ID must start from 0xff000000
    fn allocate<T: Into<O> + 'static>(&mut self, object: T) -> Result<(u32, &mut T), T>;
    fn remove(&mut self, id: u32) -> Option<O>;
    /// Returns the singleton state associated with an object type. Returns
    /// `None` if no object of that type is in the store, or if the
    /// object type does not have a singleton state.
    ///
    /// # Panics
    ///
    /// Panics if [`AnyObject::type_id`] or [`AnyObject::singleton_state`]
    /// is not properly implemented for `O`.
    fn get_state<T: objects::MonoObject>(&self) -> Option<&T::SingletonState>;
    /// See [`Store::get_state`]
    fn get_state_mut<T: objects::MonoObject>(&mut self) -> Option<&mut T::SingletonState>;
    /// Get a reference an object with its associated singleton state
    ///
    /// # Panics
    ///
    /// Panics if [`AnyObject::type_id`] or [`AnyObject::singleton_state`]
    /// is not properly implemented for `O`.
    fn get_with_state<T: objects::MonoObject>(
        &self,
        id: u32,
    ) -> Result<(&T, &T::SingletonState), GetError>;
    /// Get a unique reference to an object with its associated singleton
    /// state
    ///
    /// # Panics
    ///
    /// Panics if [`AnyObject::type_id`] or [`AnyObject::singleton_state`]
    /// is not properly implemented for `O`.
    fn get_with_state_mut<T: objects::MonoObject>(
        &mut self,
        id: u32,
    ) -> Result<(&mut T, &mut T::SingletonState), GetError>;
    /// Get a reference to an object from the store, and cast it down to the
    /// concrete type.
    fn get<T: 'static>(&self, id: u32) -> Result<&T, GetError>;
    /// Get a unique reference to an object from the store, and cast it down
    /// to the concrete type.
    fn get_mut<T: 'static>(&mut self, id: u32) -> Result<&mut T, GetError>;
    /// Returns whether the store contains a given ID
    fn contains(&self, id: u32) -> bool;
    /// Try to insert an object into the store with the given ID, the object
    /// is created by calling the closure `f`. `f` is never called
    /// if the ID already exists in the store.
    fn try_insert_with(&mut self, id: u32, f: impl FnOnce() -> O) -> Option<&mut O>;
    /// Try to insert an object into the store with the given ID, the object
    /// is created by calling the closure `f` with the singleton state
    /// that's associated with the object type. `f` is never called
    /// if the ID already exists in the store.
    fn try_insert_with_state<T: Into<O> + objects::MonoObject + 'static>(
        &mut self,
        id: u32,
        f: impl FnOnce(&mut T::SingletonState) -> T,
    ) -> Option<(&mut T, &mut T::SingletonState)>;

    /// Return an iterator for all objects in the store with a specific
    /// type.
    fn by_type<T: objects::MonoObject + 'static>(&self) -> Self::ByType<'_, T>;
    fn ids_by_type<T: 'static>(&self) -> Self::IdsByType<'_, T>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventHandlerAction {
    // This event handler should be stopped.
    Stop,
    // This event handler should be kept.
    Keep,
}

/// An event handler.
///
/// Occasionally, wayland object implementations need to handle events that
/// arise from the compositor. For example, when user moves the pointer,
/// the wl_surface objects maybe need to send motion events to the
/// client.
///
/// This can be achieved by implement this trait, and call
/// `Client::add_event_handler` to register event handlers. Event handlers
/// have an associated event source. Whenever a new event is received,
/// the event source will be polled, and the event handler will be
/// called with the received event.
pub trait EventHandler<Ctx: Client>: 'static {
    type Message;
    type Future<'ctx>: Future<
            Output = Result<
                EventHandlerAction,
                Box<dyn Error + std::marker::Send + Sync + 'static>,
            >,
        > + 'ctx;
    /// Handle an event. Every time an event is received, this will be
    /// called, and the returned future will be driven to
    /// completion. The returned value indicates whether this event
    /// handler should be removed. Event handler is also removed if the
    /// event stream has terminated.
    ///
    /// This function is not passed a `&mut Ctx`, because handling events
    /// may require exclusive access to the set of event handlers.
    /// So we split a `Ctx` apart, remove the access to the event
    /// handlers, and only pass the remaining parts.
    ///
    /// If an error is returned, the client connection should be closed with
    /// an error.
    ///
    /// # Notes
    ///
    /// As a compositor author, if you are using this interface, you have to
    /// make sure this future DOES NOT race with other futures
    /// running in the same client context. This includes spawning this
    /// future on some executor,  adding this future to a FuturesUnordered,
    /// use this future in a `select!` branch, etc. Instead you have to
    /// `.await` this future alone till completion.
    ///
    /// You probably shouldn't use this interface directly anyways, have a
    /// look at [`super::EventDispatcher`]
    fn handle_event<'ctx>(
        &'ctx mut self,
        objects: &'ctx mut Ctx::ObjectStore,
        connection: &'ctx mut Ctx::Connection,
        server_context: &'ctx Ctx::ServerContext,
        message: &'ctx mut Self::Message,
    ) -> Self::Future<'ctx>;
}

pub trait EventDispatcher<Ctx: Client> {
    fn add_event_handler<M: Any>(
        &mut self,
        event_source: impl Stream<Item = M> + 'static,
        handler: impl EventHandler<Ctx, Message = M> + 'static,
    );
}

#[non_exhaustive]
pub struct ClientParts<'a, C: Client> {
    pub server_context:   &'a C::ServerContext,
    pub objects:          &'a mut C::ObjectStore,
    pub connection:       &'a mut C::Connection,
    pub event_dispatcher: &'a mut C::EventDispatcher,
}

impl<'a, C: Client> ClientParts<'a, C> {
    pub fn new(
        server_context: &'a C::ServerContext,
        objects: &'a mut C::ObjectStore,
        connection: &'a mut C::Connection,
        event_dispatcher: &'a mut C::EventDispatcher,
    ) -> Self {
        Self {
            server_context,
            objects,
            connection,
            event_dispatcher,
        }
    }
}

/// A client connection
pub trait Client: Sized + 'static {
    type ServerContext: crate::server::Server<ClientContext = Self> + 'static;
    type ObjectStore: Store<Self::Object>;
    type Connection: WriteMessage + Unpin + 'static;
    type Object: objects::Object<Self> + objects::AnyObject + std::fmt::Debug;
    type EventDispatcher: EventDispatcher<Self> + 'static;
    type DispatchFut<'a, R>: Future<Output = bool> + 'a
    where
        Self: 'a,
        R: runa_io::traits::buf::AsyncBufReadWithFd + 'a;
    /// Return the server context singleton.
    fn server_context(&self) -> &Self::ServerContext;
    /// Return a references to the object store
    fn objects(&self) -> &Self::ObjectStore;

    /// Return a unique reference to the connection object
    fn connection_mut(&mut self) -> &mut Self::Connection {
        self.as_mut_parts().connection
    }

    /// Return a unique reference to the object store
    fn objects_mut(&mut self) -> &mut Self::ObjectStore {
        self.as_mut_parts().objects
    }

    /// Return a unique reference to the event dispatcher
    fn event_dispatcher_mut(&mut self) -> &mut Self::EventDispatcher {
        self.as_mut_parts().event_dispatcher
    }

    /// Get unique access to all members of the client context. Otherwise
    /// accessing one of these members will borrow the whole client
    /// context, preventing access to the other members.
    fn as_mut_parts(&mut self) -> ClientParts<'_, Self>;
    fn dispatch<'a, R>(&'a mut self, reader: Pin<&'a mut R>) -> Self::DispatchFut<'a, R>
    where
        R: runa_io::traits::buf::AsyncBufReadWithFd;
}
