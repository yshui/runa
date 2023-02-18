//! This mod is for traits that are usually implemented by per client connection
//! context objects, and their related traits.
//!
//! Here are also some default implementations of these traits.

use std::{cell::RefCell, future::Future, pin::Pin, rc::Rc, task::ready};

use async_lock::{Mutex, MutexGuard};
use derivative::Derivative;
use hashbrown::{
    hash_map::{self, OccupiedError},
    HashMap, HashSet,
};
use wl_io::traits::{buf::AsyncBufWriteWithFd, ser};

use crate::objects::{Object, ObjectMeta};

const CLIENT_MAX_ID: u32 = 0xfeffffff;

pub mod traits {
    use futures_core::Future;

    use crate::objects::{ObjectMeta, StaticObjectMeta};

    pub trait Store<O> {
        /// See [`crate::utils::AsIteratorItem`] for why this is so complicated.
        type IfaceIter<'a>: Iterator<Item = (u32, &'a O)> + 'a
        where
            O: 'a,
            Self: 'a;
        /// Insert object into the store with the given ID. Returns Ok(()) if
        /// successful, Err(T) if the ID is already in use.
        fn insert<T: Into<O>>(&mut self, id: u32, object: T) -> Result<(), T>;
        /// Allocate a new ID for the client, associate `object` for it.
        /// According to the wayland spec, the ID must start from 0xff000000
        fn allocate<T: Into<O>>(&mut self, object: T) -> Result<u32, T>;
        fn remove(&mut self, id: u32) -> Option<O>;
        /// Get an object from the store.
        fn get(&self, id: u32) -> Option<&O>;
        fn get_mut(&mut self, id: u32) -> Option<&mut O>;
        fn try_insert_with(&mut self, id: u32, f: impl FnOnce() -> O) -> bool;
        /// Return an `AsIterator` for all objects in the store with a specific
        /// interface An `Iterator` can be obtain from an `AsIterator` by
        /// calling `as_iter()`
        fn by_interface<'a>(&'a self, interface: &'static str) -> Self::IfaceIter<'a>;
    }

    /// A bundle of objects.
    ///
    /// Usually this is the set of objects a client has bound to. When cloned,
    /// the result should reference to the same bundle of objects.
    ///
    /// Although all the methods are callable with a shared reference, if you
    /// are holding the value returned by a `get(...)` call, you should not
    /// try to modify the store, using methods like `insert`, `remove`,
    /// etc., which may cause a panic. Drop the `ObjectRef` first.
    pub trait LockableStore<O>: Clone {
        type LockedStore: Store<O>;
        type Guard<'a>: std::ops::DerefMut<Target = Self::LockedStore> + 'a
        where
            Self: 'a,
            O: 'a;
        type LockFut<'a>: Future<Output = Self::Guard<'a>> + 'a
        where
            O: 'a,
            Self: 'a;
        /// Lock the store for read/write accesses.
        fn lock(&self) -> Self::LockFut<'_>;
    }
}

#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub struct Store<Object> {
    map:          HashMap<u32, Object>,
    by_interface: HashMap<&'static str, HashSet<u32>>,
    /// Next ID to use for server side object allocation
    #[derivative(Default(value = "CLIENT_MAX_ID + 1"))]
    next_id:      u32,
    /// Number of server side IDs left
    #[derivative(Default(value = "u32::MAX - CLIENT_MAX_ID"))]
    ids_left:     u32,
}

impl<Object: crate::objects::ObjectMeta> Store<Object> {
    #[inline]
    fn insert(&mut self, id: u32, object: Object) -> Result<(), Object> {
        let interface = object.interface();
        if let Err(OccupiedError { value, .. }) = self.map.try_insert(id, object) {
            Err(value)
        } else {
            self.by_interface.entry(interface).or_default().insert(id);
            Ok(())
        }
    }

    #[inline]
    fn remove(&mut self, id: u32) -> Option<Object> {
        let object = self.map.remove(&id)?;
        let interface = object.interface();
        self.by_interface.get_mut(interface).unwrap().remove(&id);
        Some(object)
    }

    #[inline]
    fn try_insert_with(&mut self, id: u32, f: impl FnOnce() -> Object) -> bool {
        let entry = self.map.entry(id);
        match entry {
            hash_map::Entry::Occupied(_) => false,
            hash_map::Entry::Vacant(v) => {
                let object = f();
                let interface = object.interface();
                v.insert(object);
                self.by_interface.entry(interface).or_default().insert(id);
                true
            },
        }
    }
}

/// Per client mapping from object ID to objects. This is the reference
/// implementation of [`Objects`].
#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""), Clone(bound = ""))]
pub struct LockableStore<Object> {
    inner: Rc<Mutex<Store<Object>>>,
}

impl<Object: ObjectMeta> traits::LockableStore<Object> for LockableStore<Object> {
    type Guard<'a> = MutexGuard<'a, Store<Object>> where Object: 'a;
    type LockedStore = Store<Object>;

    type LockFut<'a> = impl Future<Output = Self::Guard<'a>> + 'a
    where
        Object: 'a,
        Self: 'a;

    fn lock(&self) -> Self::LockFut<'_> {
        self.inner.lock()
    }
}

impl<O> Store<O> {
    /// Remove all objects from the store. MUST be called before the store is
    /// dropped, to ensure on_disconnect is called for all objects.
    pub fn clear_for_disconnect<Ctx>(&mut self, ctx: &mut Ctx)
    where
        O: Object<Ctx>,
    {
        tracing::debug!("Clearing store for disconnect");
        for (_, ref mut obj) in self.map.drain() {
            tracing::debug!("Calling on_disconnect for {obj:p}");
            obj.on_disconnect(ctx);
        }
        self.ids_left = u32::MAX - CLIENT_MAX_ID;
        self.next_id = CLIENT_MAX_ID + 1;
    }
}

impl<Object> Drop for Store<Object> {
    fn drop(&mut self) {
        assert!(self.map.is_empty(), "Store not cleared before drop");
    }
}

impl<O: ObjectMeta> traits::Store<O> for Store<O> {
    type IfaceIter<'a> = impl Iterator<Item = (u32, &'a O)> + 'a where O: 'a;

    #[inline]
    fn insert<T: Into<O>>(&mut self, object_id: u32, object: T) -> Result<(), T> {
        if object_id > CLIENT_MAX_ID {
            return Err(object)
        }

        let mut orig = Some(object);
        Self::try_insert_with(self, object_id, || orig.take().unwrap().into());
        if let Some(orig) = orig {
            Err(orig)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn allocate<T: Into<O>>(&mut self, object: T) -> Result<u32, T> {
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

        Self::insert(self, curr, object.into()).unwrap_or_else(|_| unreachable!());
        Ok(curr)
    }

    fn remove(&mut self, object_id: u32) -> Option<O> {
        if object_id > CLIENT_MAX_ID {
            self.ids_left += 1;
        }
        Self::remove(self, object_id)
    }

    fn get(&self, object_id: u32) -> Option<&O> {
        self.map.get(&object_id)
    }

    fn get_mut(&mut self, id: u32) -> Option<&mut O> {
        self.map.get_mut(&id)
    }

    fn try_insert_with(&mut self, id: u32, f: impl FnOnce() -> O) -> bool {
        if id > CLIENT_MAX_ID {
            return false
        }
        Self::try_insert_with(self, id, f)
    }

    fn by_interface<'a>(&'a self, interface: &'static str) -> Self::IfaceIter<'a> {
        self.by_interface
            .get(interface)
            .into_iter()
            .flat_map(move |ids| ids.iter().map(move |id| (*id, self.map.get(id).unwrap())))
    }
}

pub trait WriteMessage {
    type Send<'a, M>: Future<Output = Result<(), std::io::Error>> + 'a
    where
        Self: 'a,
        M: 'a + wl_io::traits::ser::Serialize + Unpin + std::fmt::Debug;
    type Flush<'a>: Future<Output = Result<(), std::io::Error>> + 'a
    where
        Self: 'a;
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
}

/// A client connection
pub trait Client: Sized + 'static {
    type ServerContext: crate::server::Server<ClientContext = Self> + 'static;
    type ObjectStore: traits::LockableStore<Self::Object>;
    type Connection: WriteMessage + Clone + 'static;
    type Object: Object<Self> + std::fmt::Debug;
    type DispatchFut<'a, R>: Future<Output = bool> + 'a
    where
        Self: 'a,
        R: wl_io::traits::buf::AsyncBufReadWithFd + 'a;
    /// Return the server context singleton.
    fn server_context(&self) -> &Self::ServerContext;
    fn connection(&self) -> &Self::Connection;
    fn objects(&self) -> &Self::ObjectStore;
    /// Spawn a client dependent task from the context. When the client is
    /// disconnected, the task should be cancelled, otherwise the task
    /// should keep running.
    ///
    /// For simplicity's sake, we don't return a handle to the task. So if you
    /// want to stop your task early, you can wrap it in
    /// [`futures::future::Abortable`], or similar.
    fn spawn(&self, fut: impl Future<Output = ()> + 'static);
    fn dispatch<'a, R>(&'a mut self, reader: Pin<&'a mut R>) -> Self::DispatchFut<'a, R>
    where
        R: wl_io::traits::buf::AsyncBufReadWithFd;
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
                        wl_display::v1 as wl_display, wl_types, AsyncBufReadWithFdExt,
                        ProtocolError,
                    },
                    objects::DISPLAY_ID,
                };
                let (object_id, len, buf, fd) = match R::next_message(reader.as_mut()).await {
                    Ok(v) => v,
                    // I/O error, no point sending the error to the client
                    Err(e) => return true,
                };
                use $crate::connection::{traits::LockableStore, WriteMessage};
                let (ret, bytes_read, fds_read) =
                    <<Self as $crate::connection::Client>::Object as $crate::objects::Object<
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
                                std::ffi::CString::new(e.to_string()).unwrap(),
                            )
                        }),
                    ),
                };
                if let Some((object_id, error_code, msg)) = error {
                    // We are going to disconnect the client so we don't care about the
                    // error.
                    fatal |= self
                        .connection()
                        .send(DISPLAY_ID, wl_display::events::Error {
                            object_id: wl_types::Object(object_id),
                            code:      error_code,
                            message:   wl_types::Str(msg.as_c_str()),
                        })
                        .await
                        .is_err();
                }
                if !fatal {
                    use $crate::{objects::ObjectMeta, connection::Store};
                    if bytes_read != len as usize {
                        let len_opcode = u32::from_ne_bytes(buf[0..4].try_into().unwrap());
                        let opcode = len_opcode & 0xffff;
                        tracing::error!(
                            "unparsed bytes in buffer, {bytes_read} != {len}. object_id: \
                             {}@{object_id}, opcode: {opcode}",
                            self.objects().lock().await.get(object_id).map(|o| o.interface()).unwrap_or("unknown")
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

#[derive(Derivative)]
#[derivative(Debug, Clone(bound = ""))]
pub struct Connection<C> {
    conn: Rc<RefCell<C>>,
}

impl<C> Connection<C> {
    pub fn new(conn: C) -> Self {
        Connection {
            conn: Rc::new(RefCell::new(conn)),
        }
    }
}

impl<C: AsyncBufWriteWithFd + Unpin> WriteMessage for Connection<C> {
    type Flush<'a> = impl Future<Output = Result<(), std::io::Error>> + 'a where C: 'a;
    type Send<'a, M> = impl Future<Output = Result<(), std::io::Error>> + 'a where
        C: 'a,
        M: 'a + wl_io::traits::ser::Serialize + Unpin + std::fmt::Debug;

    fn send<'a, 'b, 'c, M: ser::Serialize + Unpin + std::fmt::Debug + 'b>(
        &'a self,
        object_id: u32,
        msg: M,
    ) -> Self::Send<'c, M>
    where
        'a: 'c,
        'b: 'c,
    {
        send_to(&self.conn, object_id, msg)
    }

    fn flush(&self) -> Self::Flush<'_> {
        flush_to(&self.conn)
    }
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
