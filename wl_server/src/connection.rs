//! This mod is for traits that are usually implemented by per client connection
//! context objects, and their related traits.
//!
//! Here are also some default implementations of these traits.

use std::{
    any::{Any, TypeId},
    future::Future,
    marker::PhantomData,
    pin::Pin,
    task::{ready, Context, Poll, Waker},
};

use derivative::Derivative;
use futures_core::{FusedFuture, Stream};
use futures_util::stream::{FuturesUnordered, StreamExt, StreamFuture};
use hashbrown::{hash_map, HashMap, HashSet};
use indexmap::IndexMap;

use crate::{events, objects, utils::one_shot_signal};

const CLIENT_MAX_ID: u32 = 0xfeffffff;

pub mod traits {
    use std::{any::Any, error::Error, pin::Pin};

    use futures_core::{Future, Stream};
    use wl_io::traits::WriteMessage;

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
    pub trait Store<O>: events::EventSource<StoreEvent> {
        /// See [`crate::utils::AsIteratorItem`] for why this is so complicated.
        type ByType<'a, T>: Iterator<Item = (u32, &'a T)> + 'a
        where
            O: 'a,
            T: objects::MonoObject + 'a,
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
        //fn try_insert_with_states<T: Into<O> + objects::MonoObject + 'static>(
        //    &mut self,
        //    id: u32,
        //    f: impl FnOnce(Option<&mut T::SingletonState>) -> T,
        //) -> Option<(&mut T, Option<&mut T::SingletonState>)>;

        /// Return an iterator for all objects in the store with a specific
        /// type.
        fn by_type<T: objects::MonoObject + 'static>(&self) -> Self::ByType<'_, T>;
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

    pub struct ClientParts<'a, C: Client> {
        pub server_context:   &'a C::ServerContext,
        pub objects:          &'a mut C::ObjectStore,
        pub connection:       &'a mut C::Connection,
        pub event_dispatcher: &'a mut C::EventDispatcher,
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
            R: wl_io::traits::buf::AsyncBufReadWithFd + 'a;
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
            R: wl_io::traits::buf::AsyncBufReadWithFd;
    }
}

#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub struct Store<Object> {
    map:          HashMap<u32, Object>,
    by_type:      HashMap<TypeId, (Box<dyn Any>, HashSet<u32>)>,
    /// Next ID to use for server side object allocation
    #[derivative(Default(value = "CLIENT_MAX_ID + 1"))]
    next_id:      u32,
    /// Number of server side IDs left
    #[derivative(Default(value = "u32::MAX - CLIENT_MAX_ID"))]
    ids_left:     u32,
    event_source: events::aggregate::Sender<StoreEventAggregate>,
}

impl<Object: objects::AnyObject> Store<Object> {
    #[inline]
    fn remove(&mut self, id: u32) -> Option<Object> {
        let object = self.map.remove(&id)?;
        let type_id = objects::AnyObject::type_id(&object);
        let interface = object.interface();
        match self.by_type.entry(type_id) {
            hash_map::Entry::Occupied(mut v) => {
                let (_, ids) = v.get_mut();
                let removed = ids.remove(&id);
                assert!(removed);
                if ids.is_empty() {
                    v.remove();
                }
            },
            hash_map::Entry::Vacant(_) => panic!("Incosistent object store state"),
        };

        self.event_source.send(traits::StoreEvent {
            kind:      traits::StoreEventKind::Removed { interface },
            object_id: id,
        });
        Some(object)
    }

    #[inline]
    fn try_insert_with<F: FnOnce() -> Object>(
        &mut self,
        id: u32,
        f: F,
    ) -> Option<(&mut Object, &mut dyn Any)> {
        let entry = self.map.entry(id);
        match entry {
            hash_map::Entry::Occupied(_) => None,
            hash_map::Entry::Vacant(v) => {
                let object = f();
                let interface = object.interface();
                let type_id = objects::AnyObject::type_id(&object);
                let (state, ids) = self
                    .by_type
                    .entry(type_id)
                    .or_insert_with(|| (object.new_singleton_state(), HashSet::new()));
                ids.insert(id);
                let ret = v.insert(object);
                self.event_source.send(traits::StoreEvent {
                    kind:      traits::StoreEventKind::Inserted { interface },
                    object_id: id,
                });
                Some((ret, &mut **state))
            },
        }
    }
}

#[derive(Debug, Default)]
struct StoreEventAggregate {
    /// Map object ID to the diff that needs to be applied.
    events: IndexMap<u32, traits::StoreEventKind>,
}

impl Iterator for StoreEventAggregate {
    type Item = traits::StoreEvent;

    fn next(&mut self) -> Option<Self::Item> {
        self.events
            .pop()
            .map(|(object_id, kind)| traits::StoreEvent { object_id, kind })
    }
}

impl StoreEventAggregate {
    fn extend_one(&mut self, id: u32, item: traits::StoreEventKind) {
        use indexmap::map::Entry;
        match self.events.entry(id) {
            Entry::Vacant(v) => {
                v.insert(item);
            },
            Entry::Occupied(mut o) => {
                use traits::StoreEventKind::*;
                match (o.get(), &item) {
                    (Inserted { .. }, Removed { .. }) => {
                        // Inserted + Removed = ()
                        o.remove();
                    },
                    (
                        Removed {
                            interface: old_interface,
                        },
                        Inserted {
                            interface: new_interface,
                        },
                    ) => {
                        // Removed + Inserted = Replaced
                        o.insert(Replaced {
                            old_interface,
                            new_interface,
                        });
                    },
                    (Replaced { old_interface, .. }, Removed { .. }) => {
                        // Replaced + Removed = Removed + Inserted + Removed = Removed
                        o.insert(Removed {
                            interface: old_interface,
                        });
                    },
                    (Inserted { .. }, Inserted { .. }) |
                    (Removed { .. }, Removed { .. }) |
                    (Replaced { .. }, Inserted { .. }) => {
                        // Inserted + Inserted =  Removed + Removed = !
                        panic!("Object {id} inserted or removed twice");
                    },
                    // A replaced event should never be sent directly, it should only be generated
                    // from aggregating a removed and an inserted event.
                    (_, Replaced { .. }) => unreachable!(),
                }
            },
        }
    }
}

impl Extend<traits::StoreEvent> for StoreEventAggregate {
    fn extend<T: IntoIterator<Item = traits::StoreEvent>>(&mut self, iter: T) {
        for event in iter {
            self.extend_one(event.object_id, event.kind);
        }
    }
}

impl<O> Store<O> {
    /// Remove all objects from the store. MUST be called before the store is
    /// dropped, to ensure on_disconnect is called for all objects.
    pub fn clear_for_disconnect<Ctx>(&mut self, server_ctx: &mut Ctx::ServerContext)
    where
        Ctx: traits::Client,
        O: objects::AnyObject + objects::Object<Ctx>,
    {
        tracing::debug!("Clearing store for disconnect");
        for (_, (mut state, ids)) in self.by_type.drain() {
            for id in ids {
                let obj = self.map.get_mut(&id).unwrap();
                tracing::debug!(
                    "Calling on_disconnect for object {id}, interface {}",
                    obj.interface()
                );
                obj.on_disconnect(server_ctx, &mut *state);
            }
        }
        self.map.clear();
        self.ids_left = u32::MAX - CLIENT_MAX_ID;
        self.next_id = CLIENT_MAX_ID + 1;
    }
}

impl<Object> Drop for Store<Object> {
    fn drop(&mut self) {
        assert!(self.map.is_empty(), "Store not cleared before drop");
    }
}

impl<O: objects::AnyObject> events::EventSource<traits::StoreEvent> for Store<O> {
    type Source = impl Stream<Item = traits::StoreEvent> + 'static;

    fn subscribe(&self) -> Self::Source {
        self.event_source.subscribe()
    }
}

impl<O: objects::AnyObject> traits::Store<O> for Store<O> {
    type ByType<'a, T> = impl Iterator<Item = (u32, &'a T)> + 'a where O: 'a, T: objects::MonoObject + 'a;

    #[inline]
    fn insert<T: Into<O> + 'static>(&mut self, object_id: u32, object: T) -> Result<&mut T, T> {
        if object_id > CLIENT_MAX_ID {
            return Err(object)
        }

        let mut orig = Some(object);
        let ret = Self::try_insert_with(self, object_id, || orig.take().unwrap().into());
        if let Some(orig) = orig {
            Err(orig)
        } else {
            Ok(ret.unwrap().0.cast_mut().unwrap())
        }
    }

    #[inline]
    fn insert_with_state<T: Into<O> + objects::MonoObject + 'static>(
        &mut self,
        object_id: u32,
        object: T,
    ) -> Result<(&mut T, &mut T::SingletonState), T> {
        if object_id > CLIENT_MAX_ID {
            return Err(object)
        }

        let mut orig = Some(object);
        let ret = Self::try_insert_with(self, object_id, || orig.take().unwrap().into());
        if let Some(orig) = orig {
            Err(orig)
        } else {
            let (obj, state) = ret.unwrap();
            let state = state.downcast_mut().unwrap();
            Ok((obj.cast_mut().unwrap(), state))
        }
    }

    #[inline]
    fn allocate<T: Into<O> + 'static>(&mut self, object: T) -> Result<(u32, &mut T), T> {
        if self.ids_left == 0 {
            // Store full
            return Err(object)
        }

        let mut curr = self.next_id;

        // Find the next unused id
        loop {
            if !self.map.contains_key(&curr) {
                break
            }
            if curr == u32::MAX {
                curr = CLIENT_MAX_ID + 1;
            } else {
                curr += 1;
            }
        }

        self.next_id = if curr == u32::MAX {
            CLIENT_MAX_ID + 1
        } else {
            curr + 1
        };
        self.ids_left -= 1;

        let inserted = Self::try_insert_with(self, curr, || object.into()).unwrap();
        Ok((curr, inserted.0.cast_mut().unwrap()))
    }

    fn remove(&mut self, object_id: u32) -> Option<O> {
        if object_id > CLIENT_MAX_ID {
            self.ids_left += 1;
        }
        Self::remove(self, object_id)
    }

    fn get_state<T: objects::MonoObject>(&self) -> Option<&T::SingletonState> {
        self.by_type
            .get(&std::any::TypeId::of::<T>())
            .map(|(s, _)| s.downcast_ref().unwrap())
    }

    fn get_state_mut<T: objects::MonoObject>(&mut self) -> Option<&mut T::SingletonState> {
        self.by_type
            .get_mut(&std::any::TypeId::of::<T>())
            .map(|(s, _)| s.downcast_mut().unwrap())
    }

    fn get_with_state<T: objects::MonoObject>(
        &self,
        id: u32,
    ) -> Result<(&T, &T::SingletonState), traits::GetError> {
        let o = self.map.get(&id).ok_or(traits::GetError::IdNotFound(id))?;
        let obj = o.cast::<T>().ok_or(traits::GetError::TypeMismatch(id))?;
        let state = self
            .by_type
            .get(&std::any::TypeId::of::<T>())
            .map(|(s, _)| s.downcast_ref().unwrap())
            .unwrap();
        Ok((obj, state))
    }

    fn get_with_state_mut<'a, T: objects::MonoObject>(
        &'a mut self,
        id: u32,
    ) -> Result<(&'a mut T, &'a mut T::SingletonState), traits::GetError> {
        let o: &'a mut O = self
            .map
            .get_mut(&id)
            .ok_or(traits::GetError::IdNotFound(id))?;
        let obj = o
            .cast_mut::<T>()
            .ok_or(traits::GetError::TypeMismatch(id))?;
        let state = self
            .by_type
            .get_mut(&std::any::TypeId::of::<T>())
            .map(|(s, _)| s.downcast_mut().unwrap())
            .unwrap();
        Ok((obj, state))
    }

    fn get<T: 'static>(&self, object_id: u32) -> Result<&T, traits::GetError> {
        let o = self
            .map
            .get(&object_id)
            .ok_or(traits::GetError::IdNotFound(object_id))?;
        o.cast().ok_or(traits::GetError::TypeMismatch(object_id))
    }

    fn get_mut<T: 'static>(&mut self, id: u32) -> Result<&mut T, traits::GetError> {
        let o = self
            .map
            .get_mut(&id)
            .ok_or(traits::GetError::IdNotFound(id))?;
        o.cast_mut().ok_or(traits::GetError::TypeMismatch(id))
    }

    fn contains(&self, id: u32) -> bool {
        self.map.contains_key(&id)
    }

    fn try_insert_with(&mut self, id: u32, f: impl FnOnce() -> O) -> Option<&mut O> {
        if id > CLIENT_MAX_ID {
            return None
        }
        Self::try_insert_with(self, id, f).map(|(o, _)| o)
    }

    fn by_type<T: objects::MonoObject>(&self) -> Self::ByType<'_, T> {
        self.by_type
            .get(&std::any::TypeId::of::<T>())
            .into_iter()
            .flat_map(move |(_, ids)| {
                ids.iter()
                    .map(move |id| (*id, self.map.get(id).unwrap().cast().unwrap()))
            })
    }
}

#[macro_export]
macro_rules! impl_dispatch {
    // This can be a default impl in the trait, but for that we need one of the
    // following features:
    //   1. async_fn_in_trait
    //   2. return_position_impl_trait_in_trait
    //   3. associated_type_defaults
    // Neither of those are on clear paths to stabilization.
    () => {
        type DispatchFut<'a, R> = impl Future<Output = bool> + 'a
                                                where
                                                    Self: 'a,
                                                    R: wl_io::traits::buf::AsyncBufReadWithFd + 'a;

        fn dispatch<'a, R>(&'a mut self, mut reader: Pin<&'a mut R>) -> Self::DispatchFut<'a, R>
        where
            R: $crate::__private::AsyncBufReadWithFd,
        {
            async move {
                use $crate::{
                    __private::{
                        wl_display::v1 as wl_display, wl_types, AsyncBufReadWithFd,
                        ProtocolError, WriteMessage,
                    },
                    objects::DISPLAY_ID,
                };
                let (object_id, len, buf, fd) = match R::next_message(reader.as_mut()).await {
                    Ok(v) => v,
                    // I/O error, no point sending the error to the client
                    Err(e) => return true,
                };
                let (ret, bytes_read, fds_read) =
                    <<Self as $crate::connection::traits::Client>::Object as $crate::objects::Object<
                        Self,
                    >>::dispatch(self, object_id, (buf, fd))
                    .await;
                let (mut fatal, error) = match ret {
                    Ok(_) => (false, None),
                    Err(e) => (
                        e.fatal(),
                        e.wayland_error().map(|(object_id, error_code)| {
                            (
                                object_id,
                                error_code,
                                e.to_string(),
                            )
                        }),
                    ),
                };
                if let Some((object_id, error_code, msg)) = error {
                    // We are going to disconnect the client so we don't care about the
                    // error.
                    fatal |= self.connection_mut().send(DISPLAY_ID, wl_display::events::Error {
                            object_id: wl_types::Object(object_id),
                            code:      error_code,
                            message:   wl_types::Str(msg.as_bytes()),
                        })
                        .await
                        .is_err();
                }
                if !fatal {
                    use $crate::{objects::AnyObject, connection::Store};
                    if bytes_read != len as usize {
                        let len_opcode = u32::from_ne_bytes(buf[0..4].try_into().unwrap());
                        let opcode = len_opcode & 0xffff;
                        tracing::error!(
                            "unparsed bytes in buffer, read ({bytes_read}) != received ({len}). object_id: \
                             {}@{object_id}, opcode: {opcode}",
                            self.objects().get::<Self::Object>(object_id)
                                .map(|o| o.interface()).unwrap_or("unknown")
                        );
                        fatal = true;
                    }
                    reader.consume(bytes_read, fds_read);
                }
                fatal
            }
        }
    };
}

#[derive(Debug)]
pub struct EventSerial<D> {
    serials:     HashMap<u32, (D, std::time::Instant)>,
    last_serial: u32,
    expire:      std::time::Duration,
}

/// A serial allocator for event serials. Serials are automatically forgotten
/// after a set amount of time.
impl<D> EventSerial<D> {
    pub fn new(expire: std::time::Duration) -> Self {
        Self {
            serials: HashMap::new(),
            last_serial: 0,
            expire,
        }
    }

    fn clean_up(&mut self) {
        let now = std::time::Instant::now();
        self.serials.retain(|_, (_, t)| *t + self.expire > now);
    }
}

impl<D: 'static> crate::Serial for EventSerial<D> {
    type Data = D;
    type Iter<'a> = <&'a Self as IntoIterator>::IntoIter;

    fn next_serial(&mut self, data: Self::Data) -> u32 {
        self.last_serial += 1;

        self.clean_up();
        if self
            .serials
            .insert(self.last_serial, (data, std::time::Instant::now()))
            .is_some()
        {
            panic!(
                "serial {} already in use, expiration duration too long?",
                self.last_serial
            );
        }
        self.last_serial
    }

    fn get(&self, serial: u32) -> Option<&Self::Data> {
        let now = std::time::Instant::now();
        self.serials.get(&serial).and_then(|(d, timestamp)| {
            if *timestamp + self.expire > now {
                Some(d)
            } else {
                None
            }
        })
    }

    fn expire(&mut self, serial: u32) -> bool {
        self.clean_up();
        self.serials.remove(&serial).is_some()
    }

    fn iter(&self) -> Self::Iter<'_> {
        self.into_iter()
    }
}
impl<'a, D: 'a> IntoIterator for &'a EventSerial<D> {
    type Item = (u32, &'a D);

    type IntoIter = impl Iterator<Item = Self::Item> + 'a where Self: 'a;

    fn into_iter(self) -> Self::IntoIter {
        self.serials.iter().map(|(k, (d, _))| (*k, d))
    }
}

type PairedEventHandlerFut<'a, H, Ctx>
where
    H: traits::EventHandler<Ctx>,
    Ctx: traits::Client,
= impl Future<Output = Result<(EventHandlerOutput, H), (H::Message, H)>> + 'a;

/// A helper function for storing the event handler's future.
///
/// Event handler's generate a future that references the event handler itself,
/// if we store that directly in [`PairedEventHandler`] we would have a
/// self-referential struct, so we use this async fn to get around that.
///
/// The stop signal is needed so when the future can to be stopped early, for
/// example when a [`PendingEventFut`] is dropped.
fn paired_event_handler_driver<'ctx, H, Ctx: traits::Client>(
    mut handler: H,
    mut stop_signal: one_shot_signal::Receiver,
    mut message: H::Message,
    objects: &'ctx mut Ctx::ObjectStore,
    connection: &'ctx mut Ctx::Connection,
    server_context: &'ctx Ctx::ServerContext,
) -> PairedEventHandlerFut<'ctx, H, Ctx>
where
    H: traits::EventHandler<Ctx>,
{
    async move {
        use futures_util::{select, FutureExt};
        select! {
            () = stop_signal => {
                Err((message, handler))
            }
            ret = handler.handle_event(objects, connection, server_context, &mut message).fuse() => {
                Ok((ret, handler))
            }
        }
    }
}

type EventHandlerOutput =
    Result<traits::EventHandlerAction, Box<dyn std::error::Error + Send + Sync + 'static>>;
#[pin_project::pin_project]
struct PairedEventHandler<'fut, Ctx: traits::Client, ES: Stream, H: traits::EventHandler<Ctx>> {
    #[pin]
    event_source:  ES,
    should_retain: bool,
    handler:       Option<H>,
    message:       Option<ES::Item>,
    #[pin]
    fut:           Option<PairedEventHandlerFut<'fut, H, Ctx>>,
    stop_signal:   Option<one_shot_signal::Sender>,
    _ctx:          PhantomData<Ctx>,
}

impl<Ctx: traits::Client, ES: Stream, H: traits::EventHandler<Ctx>> Stream
    for PairedEventHandler<'_, Ctx, ES, H>
{
    type Item = ();

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        if this.message.is_none() {
            let Some(message) = ready!(this.event_source.poll_next(cx)) else {
                return Poll::Ready(None);
            };
            *this.message = Some(message);
        };
        Poll::Ready(Some(()))
    }
}

trait AnyEventHandler: Stream<Item = ()> {
    type Ctx: traits::Client;
    fn poll_handle(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<EventHandlerOutput>;

    fn start_handle<'a>(
        self: Pin<Box<Self>>,
        objects: &'a mut <Self::Ctx as traits::Client>::ObjectStore,
        connection: &'a mut <Self::Ctx as traits::Client>::Connection,
        server_context: &'a <Self::Ctx as traits::Client>::ServerContext,
    ) -> Pin<Box<dyn AnyEventHandler<Ctx = Self::Ctx> + 'a>>;

    fn stop_handle(
        self: Pin<Box<Self>>,
    ) -> (
        Pin<Box<dyn AnyEventHandler<Ctx = Self::Ctx>>>,
        traits::EventHandlerAction,
    );
}

impl<Ctx, ES, H> PairedEventHandler<'_, Ctx, ES, H>
where
    Ctx: traits::Client,
    ES: Stream + 'static,
    H: traits::EventHandler<Ctx> + 'static,
{
    /// Lengthen or shorten the lifetime parameter of the returned
    /// `PairedEventHandler`.
    ///
    /// # Panics
    ///
    /// This function verifies the `fut` field of `Self` is `None`, i.e. `Self`
    /// does not contain any references. If this is not the case, this function
    /// panics.
    fn coerce_lifetime<'a>(self: Pin<Box<Self>>) -> Pin<Box<PairedEventHandler<'a, Ctx, ES, H>>> {
        assert!(self.fut.is_none());
        // Safety: this is safe because `fut` is `None` and thus does not contain any
        // references. And we do not move `self` out of the `Pin<Box<Self>>`.
        unsafe {
            let raw = Box::into_raw(Pin::into_inner_unchecked(self));
            Pin::new_unchecked(Box::from_raw(raw.cast()))
        }
    }
}

impl<Ctx, ES, H> AnyEventHandler for PairedEventHandler<'_, Ctx, ES, H>
where
    Ctx: traits::Client,
    ES: Stream + 'static,
    H: traits::EventHandler<Ctx, Message = ES::Item> + 'static,
{
    type Ctx = Ctx;

    fn poll_handle(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<EventHandlerOutput> {
        let mut this = self.project();
        let fut = this.fut.as_mut().as_pin_mut().unwrap();
        let (ret, handler) = ready!(fut
            .poll(cx)
            .map(|ret| ret.unwrap_or_else(|_| unreachable!("future stopped unexpectedly"))));
        *this.handler = Some(handler);
        *this.stop_signal = None;
        this.fut.set(None);
        // EventHandlerAction::Stop or Err() means we should stop handling events
        *this.should_retain =
            *this.should_retain && matches!(ret, Ok(traits::EventHandlerAction::Keep));
        Poll::Ready(ret)
    }

    fn start_handle<'a>(
        self: Pin<Box<Self>>,
        objects: &'a mut <Self::Ctx as traits::Client>::ObjectStore,
        connection: &'a mut <Self::Ctx as traits::Client>::Connection,
        server_context: &'a <Self::Ctx as traits::Client>::ServerContext,
    ) -> Pin<Box<dyn AnyEventHandler<Ctx = Self::Ctx> + 'a>> {
        // Shorten the lifetime of `Self`. So we can store `fut` with lifetime `'a` in
        // it.
        let mut shortened = self.coerce_lifetime();
        let mut this = shortened.as_mut().project();
        let message = this.message.take().unwrap();
        let handler = this.handler.take().unwrap();
        assert!(this.stop_signal.is_none());
        assert!(*this.should_retain);
        let (tx, stop_signal) = one_shot_signal::new_pair();
        *this.stop_signal = Some(tx);

        let new_fut = paired_event_handler_driver(
            handler,
            stop_signal,
            message,
            objects,
            connection,
            server_context,
        );
        this.fut.set(Some(new_fut));
        shortened
    }

    fn stop_handle(
        mut self: Pin<Box<Self>>,
    ) -> (
        Pin<Box<dyn AnyEventHandler<Ctx = Self::Ctx>>>,
        traits::EventHandlerAction,
    ) {
        use futures_util::task::noop_waker_ref;
        let mut this = self.as_mut().project();
        // Stop the handler, so when we poll it, it will give us the handler back.
        let Some(stop_signal) = this.stop_signal.take() else {
            // Already stopped
            let should_retain = *this.should_retain;
            assert!(self.handler.is_some());
            return (self.coerce_lifetime(), if should_retain {
                traits::EventHandlerAction::Keep
            } else {
                traits::EventHandlerAction::Stop
            });
        };
        stop_signal.send();

        let mut cx = Context::from_waker(noop_waker_ref());
        let mut fut = this.fut.as_mut().as_pin_mut().unwrap();
        let result = loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(result) => break result,
                Poll::Pending => {},
            }
        };
        match result {
            Ok((ret, handler)) => {
                // The handler completed before it was stopped by `stop_signal`
                *this.handler = Some(handler);
                this.fut.set(None);
                *this.should_retain =
                    *this.should_retain && matches!(ret, Ok(traits::EventHandlerAction::Keep));
                let should_retain = *this.should_retain;
                (
                    self.coerce_lifetime(),
                    if should_retain {
                        traits::EventHandlerAction::Keep
                    } else {
                        traits::EventHandlerAction::Stop
                    },
                )
            },
            Err((msg, handler)) => {
                // The handler was stopped by `stop_signal`
                *this.handler = Some(handler);
                *this.message = Some(msg);
                // The handler was not completed, so it's inconlusive whether it would have
                // returned `EventHandlerAction::Keep` or not. So we keep it just in case.
                (self.coerce_lifetime(), traits::EventHandlerAction::Keep)
            },
        }
    }
}

type BoxedAnyEventHandler<Ctx> = Pin<Box<dyn AnyEventHandler<Ctx = Ctx>>>;

pub struct EventDispatcher<Ctx> {
    handlers:       FuturesUnordered<StreamFuture<BoxedAnyEventHandler<Ctx>>>,
    active_handler: Option<BoxedAnyEventHandler<Ctx>>,
    waker:          Option<Waker>,
}

impl<Ctx> std::fmt::Debug for EventDispatcher<Ctx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventDispatcher").finish()
    }
}

impl<Ctx> Default for EventDispatcher<Ctx> {
    fn default() -> Self {
        Self {
            handlers:       FuturesUnordered::new(),
            active_handler: None,
            waker:          None,
        }
    }
}

impl<Ctx> EventDispatcher<Ctx> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<Ctx: traits::Client + 'static> traits::EventDispatcher<Ctx> for EventDispatcher<Ctx> {
    fn add_event_handler<M: Any>(
        &mut self,
        event_source: impl Stream<Item = M> + 'static,
        handler: impl traits::EventHandler<Ctx, Message = M> + 'static,
    ) {
        let pinned = Box::pin(PairedEventHandler {
            event_source,
            handler: Some(handler),
            should_retain: true,
            message: None,
            fut: None,
            stop_signal: None,
            _ctx: PhantomData::<Ctx>,
        });
        let pinned = pinned as Pin<Box<dyn AnyEventHandler<Ctx = Ctx>>>;
        let pinned = pinned.into_future();
        self.handlers.push(pinned);
        if let Some(w) = self.waker.take() {
            w.wake();
        }
    }
}

impl<Ctx: traits::Client + 'static> EventDispatcher<Ctx> {
    /// Poll for the next event, which needs to be handled with the use of the
    /// client context.
    ///
    /// # Caveats
    ///
    /// If this is called from multiple tasks, those tasks will just keep waking
    /// up each other and waste CPU cycles.
    pub fn poll_next<'a>(&'a mut self, cx: &mut Context<'_>) -> Poll<PendingEvent<'a, Ctx>> {
        loop {
            if self.active_handler.is_some() {
                return Poll::Ready(PendingEvent { dispatcher: self })
            }
            match Pin::new(&mut self.handlers).poll_next(cx) {
                Poll::Ready(Some((Some(()), handler))) => {
                    self.active_handler = Some(handler);
                },
                Poll::Ready(Some((None, _))) => (),
                Poll::Ready(None) | Poll::Pending => {
                    // There is no active handler. `FuturesUnordered` will wake us up if there are
                    // handlers that are ready. But we also need to wake up if there are new
                    // handlers added. So we store the waker.
                    if let Some(w) = self.waker.take() {
                        if w.will_wake(cx.waker()) {
                            self.waker = Some(w);
                        } else {
                            // Wake the previous waker, because it's going to be replaced.
                            w.wake();
                            self.waker = Some(cx.waker().clone());
                        }
                    } else {
                        self.waker = Some(cx.waker().clone());
                    }
                    return Poll::Pending
                },
            }
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> impl FusedFuture<Output = PendingEvent<'_, Ctx>> + '_ {
        struct Next<'a, Ctx> {
            dispatcher: Option<&'a mut EventDispatcher<Ctx>>,
        }
        impl<'a, Ctx: traits::Client + 'static> Future for Next<'a, Ctx> {
            type Output = PendingEvent<'a, Ctx>;

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = self.dispatcher.as_deref_mut().unwrap();
                ready!(this.poll_next(cx));

                let this = self.dispatcher.take().unwrap();
                Poll::Ready(PendingEvent { dispatcher: this })
            }
        }

        impl<'a, Ctx: traits::Client + 'static> FusedFuture for Next<'a, Ctx> {
            fn is_terminated(&self) -> bool {
                self.dispatcher.is_none()
            }
        }

        Next {
            dispatcher: Some(self),
        }
    }

    /// Handle all events that are current queued. The futures returned will
    /// resolve as soon as there are no more events queued in this event
    /// dispatcher. It will not wait for new events to be queued, but it may
    /// wait for the futures returned by the event handlers to resolve.
    pub async fn handle_queued_events<'ctx>(
        &'ctx mut self,
        objects: &'ctx mut Ctx::ObjectStore,
        connection: &'ctx mut Ctx::Connection,
        server_context: &'ctx Ctx::ServerContext,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // We are not going to wait on the event dispatcher, so we use a noop waker.
        let waker = futures_util::task::noop_waker();
        let cx = &mut Context::from_waker(&waker);
        while let Poll::Ready(pending_event) = self.poll_next(cx) {
            pending_event
                .handle(objects, connection, server_context)
                .await?;
        }
        Ok(())
    }
}

pub struct PendingEvent<'a, Ctx> {
    dispatcher: &'a mut EventDispatcher<Ctx>,
}

pub struct PendingEventFut<'dispatcher, 'ctx, Ctx: traits::Client> {
    dispatcher: &'dispatcher mut EventDispatcher<Ctx>,
    fut:        Option<Pin<Box<dyn AnyEventHandler<Ctx = Ctx> + 'ctx>>>,
}

impl<'dispatcher, 'ctx, Ctx: traits::Client> Future for PendingEventFut<'dispatcher, 'ctx, Ctx> {
    type Output = EventHandlerOutput;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.fut.as_mut().unwrap().as_mut().poll_handle(cx)
    }
}

impl<'dispatcher, 'ctx, Ctx: traits::Client> Drop for PendingEventFut<'dispatcher, 'ctx, Ctx> {
    fn drop(&mut self) {
        if let Some(fut) = self.fut.take() {
            let (fut, action) = fut.stop_handle();
            if action == traits::EventHandlerAction::Keep {
                self.dispatcher.handlers.push(fut.into_future());
                if let Some(w) = self.dispatcher.waker.take() {
                    w.wake();
                }
            }
        }
    }
}

impl<'this, Ctx: traits::Client> PendingEvent<'this, Ctx> {
    /// Start handling the event.
    ///
    /// # Notes
    ///
    /// If you `std::mem::forget` the returned future, the event handler will be
    /// permanently removed from the event dispatcher.
    ///
    /// Like [`traits::EventHandler::handle_event`], the future returned cannot
    /// race with other futures in the same client context.
    pub fn handle<'a>(
        self,
        objects: &'a mut Ctx::ObjectStore,
        connection: &'a mut Ctx::Connection,
        server_context: &'a Ctx::ServerContext,
    ) -> PendingEventFut<'this, 'a, Ctx>
    where
        'this: 'a,
    {
        let fut = self.dispatcher.active_handler.take().unwrap();
        let fut = fut.start_handle(objects, connection, server_context);
        PendingEventFut {
            dispatcher: self.dispatcher,
            fut:        Some(fut),
        }
    }
}

pub mod event_handler {
    use std::future::Future;

    use super::traits;
    use crate::utils::one_shot_signal;

    /// A wrapper around an event handler that allows itself to be aborted
    /// via an [`AbortHandle`];
    #[derive(Debug)]
    pub struct Abortable<E> {
        event_handler: E,
        stop_signal:   Option<one_shot_signal::Receiver>,
    }

    impl<E> Abortable<E> {
        /// Wrap an event handler so it may be aborted.
        pub fn new(event_handler: E) -> (Self, AbortHandle) {
            let (tx, rx) = one_shot_signal::new_pair();
            let abort_handle = AbortHandle { inner: tx };
            let abortable = Abortable {
                event_handler,
                stop_signal: Some(rx),
            };
            (abortable, abort_handle)
        }
    }

    impl<Ctx: traits::Client, E: traits::EventHandler<Ctx>> traits::EventHandler<Ctx> for Abortable<E> {
        type Message = E::Message;

        type Future<'ctx> = impl Future<Output = super::EventHandlerOutput> + 'ctx;

        fn handle_event<'ctx>(
            &'ctx mut self,
            objects: &'ctx mut <Ctx as traits::Client>::ObjectStore,
            connection: &'ctx mut <Ctx as traits::Client>::Connection,
            server_context: &'ctx <Ctx as traits::Client>::ServerContext,
            message: &'ctx mut Self::Message,
        ) -> Self::Future<'ctx> {
            async move {
                use futures_util::{select, FutureExt};
                let mut stop_signal = self.stop_signal.take().unwrap();
                select! {
                    () = stop_signal => {
                        Ok(super::traits::EventHandlerAction::Stop)
                    },
                    res = self.event_handler.handle_event(objects, connection, server_context, message).fuse() => {
                        self.stop_signal = Some(stop_signal);
                        res
                    }
                }
            }
        }
    }

    #[derive(Debug)]
    pub struct AbortHandle {
        inner: one_shot_signal::Sender,
    }

    impl AbortHandle {
        /// Abort the event handler associated with this abort handle.
        /// This will cause the event handler to be stopped the next time
        /// it is polled.
        pub fn abort(&self) {
            self.inner.send();
        }

        /// Turn this abort handle into an [`AutoAbortHandle`], which will
        /// automatically abort the event handler when it is dropped.
        pub fn auto_abort(self) -> AutoAbortHandle {
            AutoAbortHandle { inner: self.inner }
        }
    }

    /// An abort handle that will automatically abort the event handler
    /// when dropped.
    #[derive(Debug)]
    pub struct AutoAbortHandle {
        inner: one_shot_signal::Sender,
    }

    impl Drop for AutoAbortHandle {
        fn drop(&mut self) {
            self.inner.send();
        }
    }
}
