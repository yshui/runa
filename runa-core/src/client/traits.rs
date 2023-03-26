//! Traits related to the client context

use std::{any::Any, error::Error, pin::Pin};

use futures_core::{Future, Stream};
use runa_io::traits::WriteMessage;

use crate::{events, objects};

/// Things that can happen to objects in the [`Store`]
///
/// Note the extra [`Replaced`](Self::Replaced) event. This is because if an
/// event handler is slow at handling an event, the object may be inserted, then
/// removed and replaced by another object with the same ID before the handler
/// is notified. The event handler would be confused by the object when it
/// finally starts to handle the first `Inserted` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StoreUpdateKind {
    /// An object is inserted into the store
    Inserted {
        /// Interface of this newly inserted object
        interface: &'static str,
    },
    /// An object is removed from the store
    Removed {
        /// Interface of the object before it was removed
        interface: &'static str,
    },
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

/// Events emitted by [`Store`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoreUpdate {
    /// The ID of the inserted or removed object
    pub object_id: u32,
    /// What happened to the object
    pub kind:      StoreUpdateKind,
}

/// Error that can be returned by [`Store::get`]
#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum GetError {
    /// The ID is not found
    #[error("Object ID {0} not found")]
    IdNotFound(u32),
    /// The object is not of the requested type
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

/// Object store
///
/// For storing bound objects for a client.
///
/// Besides storing objects, the object store also needs to store states
/// associated with types of objects. These are states that are shared by all
/// objects of a certain type. For example, there could be a single instance of
/// "SurfaceObjectState", that is shared and used by all "Surface" objects in
/// the object store. States are created when the first object of a certain type
/// is inserted into the store, and destroyed when the last object of that type
/// is removed from the store. See
/// [`MonoObject::SingletonState`](crate::objects::MonoObject::SingletonState)
/// for more information.
///
/// The store is also an event source, and will emit events when objects are
/// inserted or removed from the store.
pub trait Store<O>: events::EventSource<StoreUpdate> {
    /// Type of iterator returned by [`by_type`](Self::by_type)
    type ByType<'a, T>: Iterator<Item = (u32, &'a T)> + 'a
    where
        O: 'a,
        T: objects::MonoObject + 'a,
        Self: 'a;

    /// Type of iterator returned by [`ids_by_type_mut`](Self::ids_by_type)
    type IdsByType<'a, T>: Iterator<Item = u32> + 'a
    where
        O: 'a,
        T: 'static,
        Self: 'a;

    /// Insert object into the store with the given ID. Returns a unique
    /// reference to the inserted object if successful, Err(T) if
    /// the ID is already in use.
    ///
    /// This is for when the client wants to allocate an object with the given
    /// ID. According to the wayland spec, the ID must be less than
    /// 0xff000000
    fn insert<T: Into<O> + 'static>(&mut self, id: u32, object: T) -> Result<&mut T, T>;

    /// Insert object into the store with the given ID. Returns a unique
    /// reference to the inserted object and its singleton state if
    /// successful, Err(T) if the ID is already in use.
    fn insert_with_state<T: Into<O> + objects::MonoObject + 'static>(
        &mut self,
        id: u32,
        object: T,
    ) -> Result<(&mut T, &mut T::SingletonState), T>;

    /// Allocate a new ID for the client, associate `object` for it. This is for
    /// inserting server allocated objects.
    /// According to the wayland spec, the ID must start from 0xff000000
    fn allocate<T: Into<O> + 'static>(&mut self, object: T) -> Result<(u32, &mut T), T>;

    /// Remove an object from the store. Returns the removed object if it is
    /// found.
    fn remove(&mut self, id: u32) -> Option<O>;

    /// Returns the singleton state associated with an object type. Returns
    /// `None` if no object of that type is in the store, or if the
    /// object type does not have a singleton state.
    ///
    /// # Panics
    ///
    /// Panics if [`AnyObject::type_id`](crate::objects::AnyObject::type_id) or
    /// [`AnyObject::new_singleton_state`](crate::objects::AnyObject::new_singleton_state)
    /// is not properly implemented for `O`.
    fn get_state<T: objects::MonoObject>(&self) -> Option<&T::SingletonState>;

    /// See [`Store::get_state`]
    fn get_state_mut<T: objects::MonoObject>(&mut self) -> Option<&mut T::SingletonState>;

    /// Get a reference an object with its associated singleton state
    ///
    /// # Panics
    ///
    /// Panics if [`AnyObject::type_id`](crate::objects::AnyObject::type_id) or
    /// [`AnyObject::new_singleton_state`](crate::objects::AnyObject::new_singleton_state)
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
    /// Panics if [`AnyObject::type_id`](crate::objects::AnyObject::type_id) or
    /// [`AnyObject::new_singleton_state`](crate::objects::AnyObject::new_singleton_state)
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

    /// Returns an iterator for all objects in the store with a specific
    /// type.
    fn by_type<T: objects::MonoObject + 'static>(&self) -> Self::ByType<'_, T>;

    /// Returns an iterator that yields all the object IDs for objects in the
    /// store with a specific type.
    fn ids_by_type<T: 'static>(&self) -> Self::IdsByType<'_, T>;
}

/// What should happen to an event handler
///
/// See [`EventHandler::handle_event`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventHandlerAction {
    /// This event handler should be stopped.
    Stop,
    /// This event handler should be kept.
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
    /// The event type this event handler expects
    type Message;

    /// Type of future returned by [`handle_event`](Self::handle_event).
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
    /// requires exclusive access to the set of event handlers (i.e. the event
    /// dispatcher). So we split a `Ctx` apart, remove the access to the
    /// event dispatcher, and only pass the remaining parts.
    ///
    /// If an error is returned, the client connection should be closed with
    /// an error.
    fn handle_event<'ctx>(
        &'ctx mut self,
        objects: &'ctx mut Ctx::ObjectStore,
        connection: &'ctx mut Ctx::Connection,
        server_context: &'ctx Ctx::ServerContext,
        message: &'ctx mut Self::Message,
    ) -> Self::Future<'ctx>;
}

/// Event dispatcher
///
/// Use this trait to register an async function to be called whenever a new
/// event is received from an event source.
///
/// See also [`EventDispatcher`](super::event_dispatcher::EventDispatcher) for a
/// reference implementation of this trait.
pub trait EventDispatcher<Ctx: Client> {
    /// Register an event handler
    ///
    /// This takes a stream of events, and an event handler, which defines an
    /// async function that will be called for each of the events received
    /// from the stream.
    ///
    /// # Arguments
    ///
    /// - `event_source`: A stream of items. Each item is an event. See
    ///   [`EventSource`](crate::events::EventSource), which is how event
    ///   streams are created.
    /// - `handler`: The event handler.
    fn add_event_handler<M: Any>(
        &mut self,
        event_stream: impl Stream<Item = M> + 'static,
        handler: impl EventHandler<Ctx, Message = M> + 'static,
    );
}

/// Members of a client context
///
/// See [`Client::as_mut_parts`].
#[derive(Debug)]
#[non_exhaustive]
pub struct ClientParts<'a, C: Client> {
    /// The server context
    pub server_context:   &'a C::ServerContext,
    /// The object store
    pub objects:          &'a mut C::ObjectStore,
    /// The connection to the client
    pub connection:       &'a mut C::Connection,
    /// The event dispatcher
    pub event_dispatcher: &'a mut C::EventDispatcher,
}

impl<'a, C: Client> ClientParts<'a, C> {
    /// Create a new `ClientParts`
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

/// A per-client context
///
/// A connection with a wayland client has a set of states associated with it.
/// Most prominently, is the set of objects that are bound to the client. This
/// is represented by the [`ObjectStore`](Self::ObjectStore) associated type.
///
/// For these objects to implement the wayland protocol, some of them would need
/// to access a server global context. For example, input devices, like the
/// mouse and keyboard; output devices, like a monitor, etc. These are stored in
/// the [`ServerContext`](Self::ServerContext) associated type.
///
/// Some objects might also define events to be sent to the client. For that, we
/// need a connection to the client for sending data. That is represented by the
/// [`Connection`](Self::Connection) associated type.
///
/// Those events are not always triggered in response to a client request. For
/// example, the `wl_pointer.motion` event is sent when the user moves the
/// mouse. The mouse is a server global resource, so there needs to be a
/// mechanism to notify the per-client context from the global context.
/// The notification mechanism is documented better in
/// [`events`](crate::events). Once notified, the object implementations need to
/// schedule work in response to the notification, that is
/// supported by the [`EventDispatcher`](Self::EventDispatcher) associated type.
pub trait Client: Sized + 'static {
    /// Server/compositor global context
    type ServerContext: crate::server::traits::Server<ClientContext = Self> + 'static;

    /// The object store
    type ObjectStore: Store<Self::Object>;

    /// The connection to the client
    type Connection: WriteMessage + Unpin + 'static;

    /// The object type. This is typically an `enum` of all the object types
    /// used by your compositor, with a `#[derive(Object)]` to implement the
    /// required `Object` trait.
    type Object: objects::Object<Self> + objects::AnyObject + std::fmt::Debug;

    /// The event dispatcher. Object implementations can register event handler
    /// with the event dispatcher. See the trait and [`events`](crate::events)
    /// for a more detailed explanation.
    type EventDispatcher: EventDispatcher<Self> + 'static;

    /// Future returned by the [`dispatch`](Self::dispatch) method
    type DispatchFut<'a, R>: Future<Output = bool> + 'a
    where
        Self: 'a,
        R: runa_io::traits::buf::AsyncBufReadWithFd + 'a;

    /// Return a shared reference to the server context.
    fn server_context(&self) -> &Self::ServerContext;

    /// Return a shared references to the object store
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

    /// Get unique access to all members of the client context. This is for
    /// accessing all members of the client context at the same time. Otherwise
    /// accessing one of these members will exclusively borrow the whole client
    /// context, preventing access to the other members.
    fn as_mut_parts(&mut self) -> ClientParts<'_, Self>;

    /// Read a message from `reader`, deserialize it then dispatch it to the
    /// appropriate object in the object store. The return future should resolve
    /// to a boolean which, if true, should cause the client to be
    /// disconnected. Typically indicates the client has made a protocol error.
    ///
    /// A default implementations is provided at [`super::dispatch_to`].
    fn dispatch<'a, R>(&'a mut self, reader: Pin<&'a mut R>) -> Self::DispatchFut<'a, R>
    where
        R: runa_io::traits::buf::AsyncBufReadWithFd;
}
