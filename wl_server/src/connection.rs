//! This mod is for traits that are usually implemented by per client connection
//! context objects, and their related traits.
//!
//! Here are also some default implementations of these traits.

use std::{
    cell::{Cell, RefCell},
    future::Future,
    pin::Pin,
    rc::Rc,
    task::ready,
};

use hashbrown::{hash_map, HashMap};
use wl_protocol::wayland::wl_display::v1 as wl_display;

use crate::{
    objects::{DropObject, InterfaceMeta},
    provide_any::{request_mut, request_ref, Demand, Provider},
};

/// Per client mapping from object ID to objects. This is the reference
/// implementation of [`Objects`].
///
/// # Important
///
/// This implementation DOES NOT handle calling [`DropObject`] handle when
/// objects are removed. Because this does not have the access to your
/// per-client context. When you implement [`Objects`] for your per-client
/// context using this, you should handle this yourself. Which can be done using
/// the [`drop_object`] helper function.
///
/// Other methods can be simply forwarded.
pub struct Store<Ctx> {
    map:  RefCell<HashMap<u32, Rc<dyn InterfaceMeta>>>,
    _ctx: std::marker::PhantomData<Ctx>,
}

impl<Ctx> std::fmt::Debug for Store<Ctx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct DebugMap<'a>(&'a HashMap<u32, Rc<dyn InterfaceMeta>>);
        let map = self.map.borrow();
        let debug_map = DebugMap(&map);
        impl std::fmt::Debug for DebugMap<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_map()
                    .entries(self.0.iter().map(|(k, v)| (k, v.interface())))
                    .finish()
            }
        }
        f.debug_struct("Store").field("map", &debug_map).finish()
    }
}

pub trait Entry<'a> {
    fn is_vacant(&self) -> bool;
    fn or_insert_boxed(self, v: Box<dyn InterfaceMeta>) -> &'a mut Rc<dyn InterfaceMeta>;
}

impl<'a> Entry<'a> for StoreEntry<'a> {
    fn is_vacant(&self) -> bool {
        match self {
            hash_map::Entry::Vacant(_) => true,
            hash_map::Entry::Occupied(_) => false,
        }
    }

    fn or_insert_boxed(self, v: Box<dyn InterfaceMeta>) -> &'a mut Rc<dyn InterfaceMeta> {
        hash_map::Entry::or_insert(self, v.into())
    }
}

/// A bundle of objects.
///
/// Usually this is the set of objects a client has bound to.
pub trait Objects<Ctx: ?Sized> {
    type Entry<'a>: Entry<'a>;
    /// Insert object into the store with the given ID. Returns Ok(()) if
    /// successful, Err(T) if the ID is already in use.
    fn insert<T: InterfaceMeta + 'static>(&self, id: u32, object: T) -> Result<(), T>;
    fn remove(&self, ctx: &Ctx, id: u32) -> Option<Rc<dyn InterfaceMeta>>;
    fn clear(&self, ctx: &Ctx);
    fn get(&self, id: u32) -> Option<Rc<dyn InterfaceMeta>>;
    fn with_entry<T>(&self, id: u32, f: impl FnOnce(Self::Entry<'_>) -> T) -> T;
    fn try_insert<T: InterfaceMeta + 'static>(&self, id: u32, object: T) -> Result<(), T> {
        self.with_entry(id, |e| {
            if e.is_vacant() {
                e.or_insert_boxed(Box::new(object));
                Ok(())
            } else {
                Err(object)
            }
        })
    }
}

impl<Ctx> Store<Ctx> {
    pub fn new() -> Self {
        Self {
            map:  RefCell::new(HashMap::new()),
            _ctx: Default::default(),
        }
    }
}

pub type StoreEntry<'a> =
    hash_map::Entry<'a, u32, Rc<dyn InterfaceMeta>, hash_map::DefaultHashBuilder>;
impl<Ctx: 'static> Objects<Ctx> for Store<Ctx> {
    type Entry<'a> = StoreEntry<'a>;

    fn insert<T: InterfaceMeta + 'static>(&self, object_id: u32, object: T) -> Result<(), T> {
        match self.map.borrow_mut().entry(object_id) {
            hash_map::Entry::Occupied(_) => Err(object),
            hash_map::Entry::Vacant(v) => {
                v.insert(Rc::new(object));
                Ok(())
            },
        }
    }

    fn remove(&self, ctx: &Ctx, object_id: u32) -> Option<Rc<dyn InterfaceMeta>> {
        if let Some(obj) = self.map.borrow_mut().remove(&object_id) {
            if let Some(drop_object) = request_ref::<dyn DropObject<Ctx>, _>(&*obj) {
                drop_object.drop_object(ctx);
            }
            Some(obj)
        } else {
            None
        }
    }

    fn get(&self, object_id: u32) -> Option<Rc<dyn InterfaceMeta>> {
        self.map.borrow().get(&object_id).map(Clone::clone)
    }

    fn with_entry<T>(&self, id: u32, f: impl FnOnce(StoreEntry<'_>) -> T) -> T {
        f(self.map.borrow_mut().entry(id))
    }

    /// Remove all objects from the store. MUST be called before the store is
    /// dropped, to ensure drop_object is called for all objects.
    fn clear(&self, ctx: &Ctx) {
        let mut map = self.map.borrow_mut();
        for (_, obj) in map.drain() {
            if let Some(drop_object) = request_ref::<dyn DropObject<Ctx>, _>(&*obj) {
                drop_object.drop_object(ctx);
            }
        }
    }
}

impl<Ctx> Drop for Store<Ctx> {
    fn drop(&mut self) {
        // This is a safety check to ensure that clear() is called before the store is
        // dropped. If this is not called, then drop_object will not be called
        // for all objects.
        assert!(self.map.get_mut().is_empty());
    }
}

/// A client connection
pub trait Connection {
    type Error;
    type Context: crate::server::Server<Connection = Self> + 'static;
    type Send<'a, M>: Future<Output = Result<(), Self::Error>> + 'a
    where
        Self: 'a,
        M: 'a;
    type Flush<'a>: Future<Output = Result<(), Self::Error>> + 'a
    where
        Self: 'a;
    type Objects: Objects<Self>;
    /// Return the server context singleton.
    fn server_context(&self) -> &Self::Context;

    /// Send a message to the client.
    fn send<'a, 'b, 'c, M: wl_io::Serialize + Unpin + std::fmt::Debug + 'b>(
        &'a self,
        object_id: u32,
        msg: M,
    ) -> Self::Send<'c, M>
    where
        'a: 'c,
        'b: 'c;

    /// Flush connection
    fn flush(&self) -> Self::Flush<'_>;
    fn objects(&self) -> &Self::Objects;
}

/// Implementation helper for Connection::send. This assumes you stored the
/// connection object in a RefCell. This function makes sure to not hold RefMut
/// across await.
pub fn send_to<'a, 'b, 'c, M, C, E>(
    conn: &'a RefCell<C>,
    object_id: u32,
    msg: M,
) -> impl Future<Output = Result<(), E>> + 'c
where
    M: wl_io::Serialize + Unpin + std::fmt::Debug + 'b,
    C: wl_io::AsyncBufWriteWithFd + Unpin,
    E: From<std::io::Error> + 'static,
    'a: 'c,
    'b: 'c,
{
    use std::task::{Context, Poll};
    struct Send<'a, M, C, E> {
        // Save a reference to the RefCell, if we save a Pin<&mut> here, we will be keeping the
        // RefMut across await. Same for flush.
        conn:      &'a RefCell<C>,
        object_id: u32,
        msg:       Option<M>,
        _err:      std::marker::PhantomData<Pin<Box<E>>>,
    }
    impl<'a, M, C, E> Future for Send<'a, M, C, E>
    where
        M: wl_io::Serialize + Unpin,
        C: wl_io::AsyncBufWriteWithFd + Unpin,
        E: From<std::io::Error>,
    {
        type Output = Result<(), E>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.get_mut();
            let msg_ref = this.msg.as_ref().expect("Send polled after completion");
            let len = msg_ref.len();
            let nfds = msg_ref.nfds();
            let mut conn = this.conn.borrow_mut();
            ready!(Pin::new(&mut *conn).poll_reserve(cx, len as usize, nfds as usize))?;
            let object_id = this.object_id.to_ne_bytes();
            Pin::new(&mut *conn).try_write(&object_id[..]);
            this.msg
                .take()
                .expect("Send polled after completion")
                .serialize(Pin::new(&mut *conn));
            Poll::Ready(Ok(()))
        }
    }
    tracing::debug!("Sending {:?}", msg);
    Send {
        conn,
        object_id,
        msg: Some(msg),
        _err: std::marker::PhantomData,
    }
}

/// Implementation helper for Connection::flush. This assumes you stored the
/// connection object in a RefCell. This function makes sure to not hold RefMut
/// across await.
pub fn flush_to<'a, E>(
    conn: &'a RefCell<impl wl_io::AsyncBufWriteWithFd + Unpin>,
) -> impl Future<Output = Result<(), E>> + 'a
where
    E: From<std::io::Error> + 'static,
{
    use std::task::{Context, Poll};
    struct Flush<'a, C, E> {
        conn: &'a RefCell<C>,
        _err: std::marker::PhantomData<Pin<Box<E>>>,
    }
    impl<'a, C, E> Future for Flush<'a, C, E>
    where
        C: wl_io::AsyncBufWriteWithFd + Unpin,
        E: From<std::io::Error> + 'static,
    {
        type Output = Result<(), E>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.get_mut();
            let mut conn = this.conn.borrow_mut();
            ready!(Pin::new(&mut *conn).poll_flush(cx))?;
            Poll::Ready(Ok(()))
        }
    }
    Flush {
        conn,
        _err: std::marker::PhantomData,
    }
}

/// A event receiver, which can be notified via its `event_handle` and a event
/// slot. Each slot also has user state attached.
pub trait Evented<Ctx> {
    type States: EventStates;
    /// Get the event flags handle that can be used to wake up the client
    /// connection task.
    fn event_handle(&self) -> crate::events::EventHandle;
    /// Reset the event flags and return the flags that are set.
    fn reset_events(&self) -> crate::events::Flags;
    fn event_states(&self) -> &Self::States;
}

pub trait EventStates {
    fn len(&self) -> usize;
    /// Remove a state at the given slot. Returns the state if it was set, or
    /// None. If slot is OOB, this panics
    fn remove(&self, slot: u8) -> Option<Box<dyn Provider>>;
    /// Set the state at slot `slot`. Returns Err(state) if the slot is already
    /// taken.
    ///
    /// # Panics
    ///
    /// Panics if the slot is OOB.
    fn set<T: Provider + 'static>(&self, slot: u8, state: T) -> Result<(), T>;
    /// Call `f` with the state stored in slot `slot`, if it exists. If slot is
    /// OOB, this panics, if the state does not provide a value of type `T`,
    /// this returns Err(())
    fn with<T: 'static, S>(&self, slot: u8, f: impl FnOnce(&T) -> S) -> Result<Option<S>, ()>;
    /// Same as `with`, but mutable.
    fn with_mut<T: 'static, S>(
        &self,
        slot: u8,
        f: impl FnOnce(&mut T) -> S,
    ) -> Result<Option<S>, ()>;
}

/// For storing arbitrary additional states in the connection object. State
/// slots are statically assigned.
pub struct SlottedStates<const N: usize> {
    states: RefCell<[Option<Box<dyn Provider>>; N]>,
}

impl<const N: usize> std::fmt::Debug for SlottedStates<N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct DebugList<'a>(&'a [Option<Box<dyn Provider>>]);
        let map = self.states.borrow();
        let debug_list = DebugList(&map[..]);
        impl std::fmt::Debug for DebugList<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_list()
                    .entries(self.0.iter().map(|o| o.is_some()))
                    .finish()
            }
        }
        f.debug_struct("SlottedStates")
            .field("is_set", &debug_list)
            .field("states", &"…")
            .finish()
    }
}

impl<const N: usize> EventStates for SlottedStates<N> {
    fn len(&self) -> usize {
        N
    }

    fn remove(&self, slot: u8) -> Option<Box<dyn Provider>> {
        let mut states = self.states.borrow_mut();
        states[slot as usize].take()
    }

    fn with<T: 'static, S>(&self, slot: u8, f: impl FnOnce(&T) -> S) -> Result<Option<S>, ()> {
        let states = self.states.borrow();
        let r = states[slot as usize]
            .as_ref()
            .map(|r| request_ref(r.as_ref()));
        match r {
            Some(Some(r)) => Ok(Some(f(r))),
            Some(None) => Err(()),
            None => Ok(None),
        }
    }

    /// Same as [`with`] but mutable.
    fn with_mut<T: 'static, S>(
        &self,
        slot: u8,
        f: impl FnOnce(&mut T) -> S,
    ) -> Result<Option<S>, ()> {
        let mut states = self.states.borrow_mut();
        let r = states[slot as usize]
            .as_mut()
            .map(|r| request_mut(r.as_mut()));
        match r {
            Some(Some(r)) => Ok(Some(f(r))),
            Some(None) => Err(()),
            None => Ok(None),
        }
    }

    fn set<T: Provider + 'static>(&self, slot: u8, state: T) -> Result<(), T> {
        let mut states = self.states.borrow_mut();
        if states[slot as usize].is_some() {
            Err(state)
        } else {
            states[slot as usize] = Some(Box::new(state));
            Ok(())
        }
    }
}

impl<const N: usize> Default for SlottedStates<N> {
    fn default() -> Self {
        const NONE: Option<Box<dyn Provider>> = None;
        Self {
            states: RefCell::new([NONE; N]),
        }
    }
}

pub struct EventSerial<D> {
    serials:     RefCell<HashMap<u32, (D, std::time::Instant)>>,
    last_serial: RefCell<u32>,
    expire:      std::time::Duration,
}

impl<D> std::fmt::Debug for EventSerial<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct DebugMap<'a, D>(&'a HashMap<u32, D>);
        let map = self.serials.borrow();
        let debug_map = DebugMap(&map);
        impl<D> std::fmt::Debug for DebugMap<'_, D> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_map()
                    .entries(self.0.iter().map(|(k, _)| (k, "…")))
                    .finish()
            }
        }
        f.debug_struct("EventSerial")
            .field("serials", &debug_map)
            .field("last_serial", &self.last_serial.borrow())
            .finish()
    }
}

/// A serial allocator for event serials. Serials are automatically forgotten
/// after a set amount of time.
impl<D> EventSerial<D> {
    pub fn new(expire: std::time::Duration) -> Self {
        Self {
            serials: RefCell::new(HashMap::new()),
            last_serial: RefCell::new(0),
            expire,
        }
    }
}

impl<D: Clone> wl_common::Serial for EventSerial<D> {
    type Data = D;

    fn next_serial(&self, data: Self::Data) -> u32 {
        let mut last_serial = self.last_serial.borrow_mut();
        *last_serial += 1;
        let serial = *last_serial;

        let mut serials = self.serials.borrow_mut();
        let now = std::time::Instant::now();
        serials.retain(|_, (_, t)| *t + self.expire > now);
        match serials.insert(serial, (data, std::time::Instant::now())) {
            Some(_) => {
                panic!(
                    "serial {} already in use, expiration duration too long?",
                    serial
                );
            },
            None => (),
        }
        serial
    }

    fn get(&self, serial: u32) -> Option<Self::Data> {
        let mut serials = self.serials.borrow_mut();
        let now = std::time::Instant::now();
        serials.retain(|_, (_, t)| *t + self.expire > now);
        serials.get(&serial).map(|(d, _)| d.clone())
    }

    fn with<F, R>(&self, serial: u32, f: F) -> Option<R>
    where
        F: FnOnce(&Self::Data) -> R,
    {
        self.serials.borrow().get(&serial).map(|(d, _)| f(d))
    }

    fn for_each<F>(&self, mut f: F)
    where
        F: FnMut(u32, &Self::Data),
    {
        self.serials
            .borrow()
            .iter()
            .for_each(|(k, (v, _))| f(*k, v));
    }

    fn expire(&self, serial: u32) -> bool {
        self.serials.borrow_mut().remove(&serial).is_some()
    }

    fn find_map<F, R>(&self, mut f: F) -> Option<R>
    where
        F: FnMut(&Self::Data) -> Option<R>,
    {
        self.serials.borrow().values().find_map(|(d, _)| f(d))
    }
}
