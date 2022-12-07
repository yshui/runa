//! This mod is for traits that are usually implemented by per client connection
//! context objects, and their related traits.
//!
//! Here are also some default implementations of these traits.

use std::{cell::RefCell, future::Future, pin::Pin, rc::Rc, task::ready};

use derivative::Derivative;
use hashbrown::{hash_map, HashMap};
use wl_io::traits::{buf::AsyncBufWriteWithFd, ser};

use crate::objects::Object;

const CLIENT_MAX_ID: u32 = 0xfeffffff;

/// Per client mapping from object ID to objects. This is the reference
/// implementation of [`Objects`].
#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub struct Store<Object> {
    map:     HashMap<u32, Rc<Object>>,
    #[derivative(Default(value = "Some(CLIENT_MAX_ID + 1)"))]
    next_id: Option<u32>,
    id_used: u32,
}

pub trait Entry<'a, Object>: Sized {
    fn is_vacant(&self) -> bool;
    fn or_insert(self, v: impl Into<Object>) -> &'a mut Object;
}

impl<'a, Object> Entry<'a, Object> for StoreEntry<'a, Object> {
    fn is_vacant(&self) -> bool {
        match self {
            hash_map::Entry::Vacant(_) => true,
            hash_map::Entry::Occupied(_) => false,
        }
    }

    fn or_insert(self, v: impl Into<Object>) -> &'a mut Object {
        let r = hash_map::Entry::or_insert(self, Rc::new(v.into()));
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
    /// Allocate a new ID for the client, associate `object` for it.
    /// According to the wayland spec, the ID must start from 0xff000000
    fn allocate<T: Into<O>>(&mut self, object: T) -> Result<u32, T>;
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
        if object_id > CLIENT_MAX_ID {
            return Err(object)
        }

        match self.map.entry(object_id) {
            hash_map::Entry::Occupied(_) => Err(object),
            hash_map::Entry::Vacant(v) => {
                v.insert(Rc::new(object.into()));
                Ok(())
            },
        }
    }

    #[inline]
    fn allocate<T: Into<O>>(&mut self, object: T) -> Result<u32, T> {
        let Some(id) = self.next_id else { return Err(object) };
        self.id_used += 1;

        self.next_id = if self.id_used >= u32::MAX - CLIENT_MAX_ID {
            None
        } else {
            let mut curr = id;
            // Find the next unused id
            loop {
                curr = curr.wrapping_add(1);
                if !self.map.contains_key(&curr) {
                    break
                }
            }
            Some(curr)
        };

        let inserted = self.map.insert(id, Rc::new(object.into()));
        assert!(inserted.is_none());
        Ok(id)
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
pub trait Client: Sized + crate::events::EventMux + 'static {
    type ServerContext: crate::server::Server<ClientContext = Self> + 'static;
    type Send<'a, M>: Future<Output = Result<(), std::io::Error>> + 'a
    where
        Self: 'a,
        M: 'a + wl_io::traits::ser::Serialize + Unpin + std::fmt::Debug;
    type Flush<'a>: Future<Output = Result<(), std::io::Error>> + 'a
    where
        Self: 'a;
    type ObjectStore: Objects<Self::Object>;
    type Object: Object<Self> + std::fmt::Debug;
    type DispatchFut<'a, R>: Future<Output = bool> + 'a
    where
        Self: 'a,
        R: wl_io::traits::buf::AsyncBufReadWithFd + 'a;
    /// Return the server context singleton.
    fn server_context(&self) -> &Self::ServerContext;

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
    fn objects(&self) -> &RefCell<Self::ObjectStore>;
    /// Spawn a client dependent task from the context. When the client is
    /// disconnected, the task should be cancelled, otherwise the task
    /// should keep running.
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
                use $crate::connection::Objects;
                let Some(obj) = self.objects().borrow().get(object_id).map(Rc::clone) else {
                                    let _ = self.send(DISPLAY_ID, wl_display::events::Error {
                                        object_id: wl_types::Object(object_id),
                                        code: 0,
                                        message: wl_types::str!("Invalid object ID"),
                                    }).await; // don't care about the error.
                                    return true;
                                };
                let (ret, bytes_read, fds_read) =
                    <<Self as $crate::connection::Client>::Object as $crate::objects::Object<
                        Self,
                    >>::dispatch(obj.as_ref(), self, object_id, (buf, fd))
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
                        .send(DISPLAY_ID, wl_display::events::Error {
                            object_id: wl_types::Object(object_id),
                            code:      error_code,
                            message:   wl_types::Str(msg.as_c_str()),
                        })
                        .await
                        .is_err();
                }
                if !fatal {
                    use $crate::objects::Object;
                    if bytes_read != len as usize {
                        let len_opcode = u32::from_ne_bytes(buf[0..4].try_into().unwrap());
                        let opcode = len_opcode & 0xffff;
                        tracing::error!(
                            "unparsed bytes in buffer, {bytes_read} != {len}. object_id: \
                             {}@{object_id}, opcode: {opcode}",
                            obj.interface()
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
    fn state(&mut self) -> (&Self, &T);
    /// Get a mutable reference to the state of type `T`.
    fn state_mut(&mut self) -> &mut T;
}

/// A helper macro to implement `State` generically for all `T: Any + Default`.
///
/// Takes 2 arguments:
///    - The type name to implement `State` for
///    - The name of the field in the type that holds the `UnboundedAggregate`.
#[macro_export]
macro_rules! impl_state_any_for {
    ($ty:ty, $member:ident) => {
        impl<T: std::any::Any + Default> $crate::connection::State<T> for $ty {
            fn state<'a>(&mut self) -> (&Self, &T) {
                // Safety: We are doing this to bypass the borrow checker, we make it forget
                // that this &T came from a &mut self, so we will be able to return a &self
                // later. This is safe because after this we never use self mutably again after the
                // trick.
                unsafe {
                    let t = &*(self.$member.get_or_default::<T>() as *const _);
                    (self, t)
                }
            }

            fn state_mut(&mut self) -> &mut T {
                self.$member.get_or_default::<T>()
            }
        }
    };
}
