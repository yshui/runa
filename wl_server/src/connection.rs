//! This mod is for traits that are usually implemented by per client connection
//! context objects, and their related traits.
//!
//! Here are also some default implementations of these traits.

use std::{cell::RefCell, future::Future, pin::Pin, rc::Rc, task::ready};

use derivative::Derivative;
use hashbrown::{hash_map, HashMap};
use wl_common::InterfaceMessageDispatch;
use wl_io::traits::{buf::AsyncBufWriteWithFd, ser};

use crate::objects::Object;

/// Per client mapping from object ID to objects. This is the reference
/// implementation of [`Objects`].
#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub struct Store<Object> {
    map: HashMap<u32, Rc<Object>>,
}

pub trait Entry<'a, Object>: Sized {
    fn is_vacant(&self) -> bool;
    fn or_insert(self, v: Object) -> &'a mut Object;
}

impl<'a, Object> Entry<'a, Object> for StoreEntry<'a, Object> {
    fn is_vacant(&self) -> bool {
        match self {
            hash_map::Entry::Vacant(_) => true,
            hash_map::Entry::Occupied(_) => false,
        }
    }

    fn or_insert(self, v: Object) -> &'a mut Object {
        let r = hash_map::Entry::or_insert(self, Rc::new(v));
        // Safety: we just created the Rc, so it must have only one reference
        unsafe { Rc::get_mut(r).unwrap_unchecked() }
    }
}

/// A bundle of objects.
///
/// Usually this is the set of objects a client has bound to.
pub trait Objects<O> {
    type Entry<'a>: Entry<'a, O>
    where
        Self: 'a,
        O: 'a;
    /// Insert object into the store with the given ID. Returns Ok(()) if
    /// successful, Err(T) if the ID is already in use.
    fn insert<T: Into<O>>(&mut self, id: u32, object: T) -> Result<(), T>;
    fn remove(&mut self, id: u32) -> Option<Rc<O>>;
    /// Get an object from the store.
    ///
    /// Why does this return a `Rc`? Because we need mutable access to the
    /// client context while holding a reference to the object, which would
    /// in turn borrow the client context if it's not a Rc.
    fn get(&self, id: u32) -> Option<&Rc<O>>;
    fn entry(&mut self, id: u32) -> Self::Entry<'_>;
}
impl<O> Store<O> {
    /// Remove all objects from the store. MUST be called before the store is
    /// dropped, to ensure on_disconnect is called for all objects.
    pub fn clear_for_disconnect<Ctx>(&mut self, ctx: &mut Ctx)
    where
        O: Object<Ctx>,
    {
        for (_, ref mut obj) in self.map.drain() {
            Rc::get_mut(obj).unwrap().on_disconnect(ctx);
        }
    }
}
impl<Object> Drop for Store<Object> {
    fn drop(&mut self) {
        assert!(self.map.is_empty(), "Store not cleared before drop");
    }
}

pub type StoreEntry<'a, O> = hash_map::Entry<'a, u32, Rc<O>, hash_map::DefaultHashBuilder>;
impl<O> Objects<O> for Store<O> {
    type Entry<'a> = StoreEntry<'a, O> where O: 'a;

    #[inline]
    fn insert<T: Into<O>>(&mut self, object_id: u32, object: T) -> Result<(), T> {
        match self.map.entry(object_id) {
            hash_map::Entry::Occupied(_) => Err(object),
            hash_map::Entry::Vacant(v) => {
                v.insert(Rc::new(object.into()));
                Ok(())
            },
        }
    }

    fn remove(&mut self, object_id: u32) -> Option<Rc<O>> {
        self.map.remove(&object_id)
    }

    fn get(&self, object_id: u32) -> Option<&Rc<O>> {
        self.map.get(&object_id)
    }

    fn entry(&mut self, id: u32) -> Self::Entry<'_> {
        self.map.entry(id)
    }
}

/// A client connection
pub trait ClientContext: Sized + crate::events::EventMux + 'static {
    type Context: crate::server::Server<ClientContext = Self> + 'static;
    type Send<'a, M>: Future<Output = Result<(), std::io::Error>> + 'a
    where
        Self: 'a,
        M: 'a + wl_io::traits::ser::Serialize + Unpin + std::fmt::Debug;
    type Flush<'a>: Future<Output = Result<(), std::io::Error>> + 'a
    where
        Self: 'a;
    type Objects: Objects<Self::Object>;
    type Object: Object<Self> + std::fmt::Debug;
    /// Return the server context singleton.
    fn server_context(&self) -> &Self::Context;

    /// Send a message to the client.
    fn send<'a, 'b, 'c, M: ser::Serialize + Unpin + std::fmt::Debug + 'b>(
        &'a self,
        object_id: u32,
        msg: M,
    ) -> Self::Send<'c, M>
    where
        'a: 'c,
        'b: 'c;

    /// Flush connection
    fn flush(&self) -> Self::Flush<'_>;
    fn objects(&self) -> &RefCell<Self::Objects>;
    /// Spawn a client dependent task from the context. When the client is
    /// disconnected, the task should be cancelled, otherwise the task
    /// should keep running.
    fn spawn(&self, fut: impl Future<Output = ()> + 'static);
}

/// Implementation helper for Connection::send. This assumes you stored the
/// connection object in a RefCell. This function makes sure to not hold RefMut
/// across await.
pub fn send_to<'a, 'b, 'c, M, C>(
    conn: &'a RefCell<C>,
    object_id: u32,
    msg: M,
) -> impl Future<Output = Result<(), std::io::Error>> + 'c
where
    M: ser::Serialize + Unpin + std::fmt::Debug + 'b,
    C: AsyncBufWriteWithFd + Unpin,
    'a: 'c,
    'b: 'c,
{
    use std::task::{Context, Poll};
    struct Send<'a, M, C> {
        // Save a reference to the RefCell, if we save a Pin<&mut> here, we will be keeping the
        // RefMut across await. Same for flush.
        conn:      &'a RefCell<C>,
        object_id: u32,
        msg:       Option<M>,
    }
    impl<'a, M, C> Future for Send<'a, M, C>
    where
        M: ser::Serialize + Unpin,
        C: AsyncBufWriteWithFd + Unpin,
    {
        type Output = Result<(), std::io::Error>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.get_mut();
            let msg_ref = this.msg.as_ref().expect("Send polled after completion");
            let len = msg_ref.len();
            let nfds = msg_ref.nfds();
            let mut conn = this.conn.borrow_mut();
            ready!(Pin::new(&mut *conn).poll_reserve(cx, len as usize, nfds as usize))?;
            let object_id = this.object_id.to_ne_bytes();
            Pin::new(&mut *conn).write(&object_id[..]);
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
    }
}

/// Implementation helper for Connection::flush. This assumes you stored the
/// connection object in a RefCell. This function makes sure to not hold RefMut
/// across await.
pub fn flush_to(
    conn: &RefCell<impl AsyncBufWriteWithFd + Unpin>,
) -> impl Future<Output = Result<(), std::io::Error>> + '_ {
    use std::task::{Context, Poll};
    struct Flush<'a, C> {
        conn: &'a RefCell<C>,
    }
    impl<'a, C> Future for Flush<'a, C>
    where
        C: AsyncBufWriteWithFd + Unpin,
    {
        type Output = Result<(), std::io::Error>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.get_mut();
            let mut conn = this.conn.borrow_mut();
            ready!(Pin::new(&mut *conn).poll_flush(cx))?;
            Poll::Ready(Ok(()))
        }
    }
    Flush { conn }
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

impl<D: 'static> wl_common::Serial for EventSerial<D> {
    type Data = D;

    type Iter<'a> = impl Iterator<Item = (u32, &'a D)> + 'a where Self: 'a;

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
        self.serials.iter().map(|(k, (d, _))| (*k, d))
    }
}

/// Allow storing per-client states in the client context.
/// Your client context type must implement `State` for types that need this.
/// Otherwise you will get compile errors stating unmet trait bounds.
///
/// See [`UnboundedAggregate`] which is a helper type which you can use to
/// implement this generically for all `T: Any`. You can easily embed it in your
/// own client context type and forward the methods to it.
pub trait State<T: Default>: Sized {
    /// Get a reference to the state of type `T`, if `state_mut` has not been
    /// called before, this can return `None`.
    fn state(&self) -> Option<&T>;
    /// Get a mutable reference to the state of type `T`.
    fn state_mut(&mut self) -> &mut T;
}
