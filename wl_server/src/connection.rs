//! This mod is for traits that are usually implemented by per client connection
//! context objects, and their related traits.
//!
//! Here are also some default implementations of these traits.

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::HashMap,
    future::Future,
    pin::Pin,
};

/// This is the bottom type for all per client objects. This trait provides some
/// metadata regarding the object, as well as a way to cast objects into a
/// common dynamic type.
///
/// # Note
///
/// If a object is a proxy of a global, it has to recognize if the global's
/// lifetime has ended, and turn all message sent to it to no-ops. This can
/// often be achieved by holding a Weak reference to the global object.
pub trait InterfaceMeta {
    /// Return the interface name of this object.
    fn interface(&self) -> &'static str;
    /// Case self to &dyn Any
    fn as_any(&self) -> &dyn Any;
}

use std::rc::Rc;

use crate::provider_any::Provider;
/// Per client mapping from object ID to objects.
pub struct Store {
    map: RefCell<HashMap<u32, Rc<dyn InterfaceMeta>>>,
}

impl std::fmt::Debug for Store {
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

impl<'a, K> Entry<'a> for std::collections::hash_map::Entry<'a, K, Rc<dyn InterfaceMeta>>
where
    K: std::cmp::Eq + std::hash::Hash,
{
    fn is_vacant(&self) -> bool {
        match self {
            std::collections::hash_map::Entry::Vacant(_) => true,
            std::collections::hash_map::Entry::Occupied(_) => false,
        }
    }

    fn or_insert_boxed(self, v: Box<dyn InterfaceMeta>) -> &'a mut Rc<dyn InterfaceMeta> {
        std::collections::hash_map::Entry::or_insert(self, v.into())
    }
}

/// A bundle of objects.
///
/// Usually this is the set of objects a client has bound to.
pub trait Objects {
    type Entry<'a>: Entry<'a>;
    /// Insert object into the store with the given ID. Returns Ok(()) if
    /// successful, Err(T) if the ID is already in use.
    fn insert<T: InterfaceMeta + 'static>(&self, id: u32, object: T) -> Result<(), T>;
    fn remove(&self, id: u32) -> Option<Rc<dyn InterfaceMeta>>;
    fn get(&self, id: u32) -> Option<Rc<dyn InterfaceMeta>>;
    fn with_entry<T>(&self, id: u32, f: impl FnOnce(Self::Entry<'_>) -> T) -> T;
}

impl Store {
    pub fn new() -> Self {
        Self {
            map: RefCell::new(HashMap::new()),
        }
    }
}

impl Objects for Store {
    type Entry<'a> = std::collections::hash_map::Entry<'a, u32, Rc<dyn InterfaceMeta>>;

    fn insert<T: InterfaceMeta + 'static>(&self, object_id: u32, object: T) -> Result<(), T> {
        match self.map.borrow_mut().entry(object_id) {
            std::collections::hash_map::Entry::Occupied(_) => Err(object),
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(Rc::new(object));
                Ok(())
            },
        }
    }

    fn remove(&self, object_id: u32) -> Option<Rc<dyn InterfaceMeta>> {
        self.map.borrow_mut().remove(&object_id)
    }

    fn get(&self, object_id: u32) -> Option<Rc<dyn InterfaceMeta>> {
        self.map.borrow().get(&object_id).map(Clone::clone)
    }

    fn with_entry<T>(&self, id: u32, f: impl FnOnce(Self::Entry<'_>) -> T) -> T {
        f(self.map.borrow_mut().entry(id))
    }
}

/// A client connection
pub trait Connection {
    type Context;
    type Error;
    type Send<'a, M>: Future<Output = Result<(), Self::Error>> + 'a
    where
        Self: 'a,
        M: 'a;
    type Flush<'a>: Future<Output = Result<(), Self::Error>> + 'a
    where
        Self: 'a;
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
            use futures_lite::ready;
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
            use futures_lite::ready;
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

pub trait Evented<Ctx> {
    /// Get the event flags handle that can be used to wake up the client
    /// connection task.
    fn event_handle(&self) -> &Rc<crate::events::EventFlags>;
    fn add_event_handler(&self, slot: u8, handler: Box<dyn crate::events::EventHandler<Ctx>>);
}

/// For storing arbitrary additional states in the connection object. State
/// slots are statically assigned.
pub trait SlottedStates {
    /// Call `f` with the state stored in slot `slot`. If the state is not set,
    /// this returns Err(())
    fn with<T: 'static, S>(&self, slot: u8, f: impl FnOnce(&T) -> S) -> Result<Option<S>, ()>;
    fn set<T: Provider + 'static>(&self, slot: u8, state: T) -> Result<(), T>;
}
pub trait SlottedStatesExt: SlottedStates {
    fn set_default<T: Provider + Default + 'static>(&self, slot: u8) -> bool {
        self.set(slot, T::default()).is_ok()
    }
}

impl<T: SlottedStates> SlottedStatesExt for T {}

/// Store states used by event handlers, here the slot number is the same as the
/// event slot number.
pub struct States {
    is_set: Cell<u64>,
    states: RefCell<[Box<dyn Provider>; 64]>,
}

impl States {
    pub fn new() -> Self {
        Self {
            states: RefCell::new(std::array::from_fn(|_| Box::new(()) as Box<dyn Provider>)),
            is_set: Cell::new(0),
        }
    }
}

impl SlottedStates for States {
    fn with<T: 'static, S>(&self, slot: u8, f: impl FnOnce(&T) -> S) -> Result<Option<S>, ()> {
        if self.is_set.get() & (1 << slot) == 0 {
            return Err(())
        }
        let states = self.states.borrow();
        Ok(crate::provider_any::request_ref(&*states[slot as usize]).map(f))
    }

    fn set<T: Provider + 'static>(&self, slot: u8, state: T) -> Result<(), T> {
        let is_set = self.is_set.get();
        if is_set & (1 << slot) != 0 {
            Err(state)
        } else {
            self.is_set.set(is_set | (1 << slot));
            self.states.borrow_mut()[slot as usize] = Box::new(state);
            Ok(())
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
                    .entries(self.0.iter().map(|(k, _)| (k, "...")))
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

    fn expire(&self, serial: u32) -> bool {
        self.serials.borrow_mut().remove(&serial).is_some()
    }
}
