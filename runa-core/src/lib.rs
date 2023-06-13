//! Fundamental types and traits for a wayland compositor
//!
//! This crate provides the scaffolding for writing a wayland compositor. It
//! defines the traits for per-client and server global contexts, and traits for
//! implementing wayland objects. It also provides helper for implementing these
//! traits.
//!
//! # Structure of a compositor
//!
//! Let's break down a wayland compositor by looking at its data flow. (I used
//! the word "data flow" loosely here.)
//!
//! A wayland compositor starts by creating a socket and listening on it. Once a
//! client connects to the socket, the compositor accepts the connection, and
//! creates a new per-client context for it. The per-client context contains the
//! object store, which will have a wl_display object already bound in it. The
//! client can issue requests to objects in its object store, by referring to
//! them with their object ID. The compositor handles these requests by calling
//! the wayland interface implementations associated with the objects.
//!
//! # How this crate fits in
//!
//! Function [`wayland_listener`] and [`wayland_listener_auto`] can be used to
//! create the socket for incoming connections.
//!
//! The traits in [`objects`](crate::objects) defines the interface of wayland
//! objects. You can choose to implement wayland objects with them and the
//! `RequestDispatch` traits generated from the wayland protocol spec (See the
//! `runa-wayland-protocols` crate). Or you can also use the `runa-orbiter`
//! crate which provides implementations for many of the wayland objects.
//!
//! The [`Client`](crate::client::traits::Client) trait defines the interface of
//! a per-client context. See its documentation for more details. That trait
//! explains how most of the other stuffs fit together.
//!
//! The [`Server`](crate::server::traits::Server) trait is responsible for
//! creating a new client context each time a new client connects. It also
//! provides access to the globals.

// We can drop trait_upcasting if necessary. `impl_trait_in_assoc_type` is
// the indispensable feature here.
#![allow(incomplete_features)]
#![feature(impl_trait_in_assoc_type, trait_upcasting)]
#![warn(
    missing_debug_implementations,
    missing_copy_implementations,
    missing_docs,
    rust_2018_idioms,
    single_use_lifetimes
)]

use futures_lite::stream::StreamExt;
use hashbrown::{hash_map, HashMap};
use thiserror::Error;

pub mod client;
pub mod error;
pub mod events;
pub mod globals;
pub mod objects;
pub mod provide_any;
pub mod server;
mod utils;

#[doc(hidden)]
pub mod __private {
    // Re-exports used by macros
    pub use runa_io::traits::{
        buf::AsyncBufReadWithFd,
        de::{Deserialize, Error as DeserError},
        ser::Serialize,
        WriteMessage,
    };
    pub use runa_wayland_protocols::wayland::wl_display;
    pub use runa_wayland_types as types;
    pub use static_assertions::assert_impl_all;
}

/// Errors that can be reported by a connection manager.
#[derive(Error, Debug)]
pub enum ConnectionManagerError<SE, LE> {
    /// Errors returned by the server type
    #[error("Server error: {0}")]
    Server(#[source] SE),
    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Errors returned by the listener
    #[error("Listener error: {0}")]
    Listener(#[source] LE),
}

/// A connection manager that accepts incoming connections from a listener, and
/// then calls
/// [`Server::new_connection`](crate::server::traits::Server::new_connection)
/// for each connection.
#[derive(Debug)]
pub struct ConnectionManager<S, Ctx> {
    listeners: S,
    ctx:       Ctx,
}

impl<S, Ctx, I, E> ConnectionManager<S, Ctx>
where
    S: futures_lite::stream::Stream<Item = Result<I, E>> + Unpin,
    Ctx: crate::server::traits::Server<Conn = I>,
{
    /// Create a new connection manager.
    ///
    /// # Arguments
    ///
    /// - `listeners`: A stream of incoming connections.
    /// - `ctx`: The server context.
    pub fn new(listeners: S, ctx: Ctx) -> Self {
        Self { listeners, ctx }
    }

    /// Run the connection manager.
    pub async fn run(&mut self) -> Result<(), ConnectionManagerError<Ctx::Error, E>> {
        while let Some(conn) = self.listeners.next().await {
            let conn = conn.map_err(ConnectionManagerError::Listener)?;
            self.ctx
                .new_connection(conn)
                .map_err(ConnectionManagerError::Server)?;
        }
        Ok(())
    }
}

/// Errors that can occur when creating a wayland listener.
#[derive(Error, Debug)]
pub enum ListenerError {
    /// I/O error
    #[error("I/O error: {0}")]
    SmolIo(#[from] std::io::Error),
    /// Error when accessing the xdg base directory
    #[error("xdg base directory error: {0}")]
    XdgBaseDirectory(#[from] xdg::BaseDirectoriesError),
    /// Unix error
    #[error("unix error: {0}")]
    Unix(#[from] rustix::io::Errno),
}

/// A guard that unlocks the lock file when dropped.
#[derive(Debug)]
pub struct FlockGuard {
    fd: rustix::fd::OwnedFd,
}

impl Drop for FlockGuard {
    fn drop(&mut self) {
        use rustix::fd::AsFd;
        rustix::fs::flock(self.fd.as_fd(), rustix::fs::FlockOperation::Unlock).unwrap();
    }
}

/// Create a `UnixListener` for accepting incoming connections from wayland
/// clients.
///
/// The path of the socket is determined by the `display` argument, as specified
/// by the wayland specification. This function also creates a lock file for the
/// socket. The lock file is used to prevent multiple compositor instances from
/// trying to use the same socket. The lock file is unlocked when the returned
/// [`FlockGuard`] is dropped.
pub fn wayland_listener(
    display: &str,
) -> Result<(std::os::unix::net::UnixListener, FlockGuard), ListenerError> {
    use rustix::fd::AsFd;
    let xdg_dirs = xdg::BaseDirectories::new()?;
    let path = xdg_dirs.place_runtime_file(display)?;
    let lock_path = xdg_dirs.place_runtime_file(format!("{display}.lock"))?;
    let lock = rustix::fs::openat(
        rustix::fs::cwd(),
        lock_path,
        rustix::fs::OFlags::CREATE,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )?;
    rustix::fs::flock(
        lock.as_fd(),
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )?;
    // We successfully locked the file, so we can remove the socket file if that
    // exists.
    let _ = std::fs::remove_file(&path);
    Ok((std::os::unix::net::UnixListener::bind(path)?, FlockGuard {
        fd: lock,
    }))
}

/// Create a `UnixListener` for accepting incoming connections from wayland
/// clients.
///
/// Similar to [`wayland_listener`], but tries to find a free display number
/// automatically.
pub fn wayland_listener_auto(
) -> Result<(std::os::unix::net::UnixListener, FlockGuard), ListenerError> {
    const MAX_DISPLAYNO: u32 = 32;
    let mut last_err = None;
    for i in 0..MAX_DISPLAYNO {
        let display = format!("wayland-{i}");
        match wayland_listener(&display) {
            Ok((listener, guard)) => return Ok((listener, guard)),
            e @ Err(_) => {
                last_err = Some(e);
            },
        }
    }
    last_err.unwrap()
}

/// Event serial management. (WIP, not currently used)
///
/// This trait allocates serial numbers, while keeping track of allocated
/// numbers and their associated data.
///
/// Some expiration scheme might be employed by the implementation to free up
/// old serial numbers.
///
/// TODO: this is incomplete and not used.
#[allow(missing_docs)]
pub trait Serial {
    type Data;
    type Iter<'a>: Iterator<Item = (u32, &'a Self::Data)>
    where
        Self: 'a,
        Self::Data: 'a;
    /// Get the next serial number in sequence. A piece of data can be attached
    /// to each serial, storing, for example, what this event is about.
    fn next_serial(&mut self, data: Self::Data) -> u32;
    /// Get the data associated with the given serial.
    fn get(&self, serial: u32) -> Option<&Self::Data>;
    fn iter(&self) -> Self::Iter<'_>;
    /// Remove the serial number from the list of allocated serials.
    fn expire(&mut self, serial: u32) -> bool;
}

struct IdAlloc<D> {
    next: u32,
    data: HashMap<u32, D>,
}

impl<D> Default for IdAlloc<D> {
    fn default() -> Self {
        Self {
            // 0 is reserved for the null object
            next: 1,
            data: HashMap::new(),
        }
    }
}

impl<D> std::fmt::Debug for IdAlloc<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct DebugMap<'a, K, V>(&'a HashMap<K, V>);
        impl<K: std::fmt::Debug, V> std::fmt::Debug for DebugMap<'_, K, V> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_set().entries(self.0.keys()).finish()
            }
        }
        f.debug_struct("IdAlloc")
            .field("next", &self.next)
            .field("data", &DebugMap(&self.data))
            .finish()
    }
}

impl<D> Serial for IdAlloc<D> {
    type Data = D;

    type Iter<'a> = impl Iterator<Item=(u32, &'a D)> where Self: 'a;

    fn next_serial(&mut self, data: Self::Data) -> u32 {
        loop {
            // We could wrap around, so check for used IDs.
            // If the occupation rate is high, this could be slow. But IdAlloc is used for
            // things like allocating client/object IDs, so we expect at most a
            // few thousand IDs used at a time, out of 4 billion available.
            let id = self.next;
            self.next += 1;
            match self.data.entry(id) {
                hash_map::Entry::Vacant(e) => {
                    e.insert(data);
                    break id
                },
                hash_map::Entry::Occupied(_) => (),
            }
        }
    }

    fn get(&self, serial: u32) -> Option<&Self::Data> {
        self.data.get(&serial)
    }

    fn expire(&mut self, serial: u32) -> bool {
        self.data.remove(&serial).is_some()
    }

    fn iter(&self) -> Self::Iter<'_> {
        self.into_iter()
    }
}

impl<'a, D: 'a> IntoIterator for &'a IdAlloc<D> {
    type Item = (u32, &'a D);

    type IntoIter = impl Iterator<Item = (u32, &'a D)>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.iter().map(|(k, v)| (*k, v))
    }
}

/// Generate a corresponding Objects enum from a Globals enum.
///
/// # Example
///
/// ```rust
/// globals! {
///    type ClientContext = ClientContext;
///    #[derive(Debug)]
///    pub enum Globals {
///        Display(wl_server::objects::Display),
///        Registry(wl_server::objects::Registry),
///     }
/// }
/// ```
///
/// The name `Globals` and `Objects` can be changed.
///
/// This will generate a `From<Variant> for Globals` for each of the variants of
/// `Globals`. And for each global, a `From<Global::Object> for Objects` will be
/// generated.
///
/// `Objects` will be filled with variants from `Global::Object` for each of the
/// globals. It can also contain extra variants, for object types that aren't
/// associated with a particular global.
#[macro_export]
macro_rules! globals {
    (
        __internal, $ctx:ty, $(#[$attr:meta])* ($($vis:tt)?) enum $N:ident { $($var:ident($f:ty)),+ $(,)? }
    ) => {
        $(#[$attr])*
        $($vis)? enum $N {
            $($var($f)),*
        }
        $(
            impl From<$f> for $N {
                fn from(f: $f) -> Self {
                    $N::$var(f)
                }
            }
        )*
        impl $crate::globals::AnyGlobal for $N {
            type Object = <$ctx as $crate::client::traits::Client>::Object;
            fn interface(&self) -> &'static str {
                match self {
                    $(
                        $N::$var(f) => <$f as $crate::globals::MonoGlobal>::INTERFACE,
                    )*
                }
            }
            fn version(&self) -> u32 {
                match self {
                    $(
                        $N::$var(f) => <$f as $crate::globals::MonoGlobal>::VERSION,
                    )*
                }
            }
            fn new_object(&self) -> Self::Object {
                match self {
                    $(
                        $N::$var(f) => <$f as $crate::globals::MonoGlobal>::new_object().into(),
                    )*
                }
            }
            fn cast<T: 'static>(&self) -> Option<&T> {
                match self {
                    $(
                        $N::$var(f) => (f as &dyn ::std::any::Any).downcast_ref::<T>(),
                    )*
                }
            }
        }
        impl $crate::globals::Bind<$ctx> for $N {
            type BindFut<'a> = impl std::future::Future<Output = std::io::Result<()>> + 'a
            where
                Self: 'a;
            fn bind<'a>(&'a self, client: &'a mut $ctx, object_id: u32) -> Self::BindFut<'a> {
                async move {
                    Ok(match self {$(
                        $N::$var(f) => <$f as $crate::globals::Bind<$ctx>>::bind(f, client, object_id).await?,
                    )*})
                }
            }
        }
        impl $N {
            $($vis)? fn globals() -> impl Iterator<Item = $N> {
                [$(<$f as $crate::globals::MonoGlobal>::MAYBE_DEFAULT.map($N::$var)),*].into_iter().flatten()
            }
        }
    };
    (
        type ClientContext = $ctx:ty;
        $(#[$attr:meta])* pub enum $N:ident { $($var:ident($f:ty)),+ $(,)? }
    ) => {
        $crate::globals!(
            __internal,
            $ctx,
            $(#[$attr])* $(#[$attr])* (pub) enum $N { $($var($f)),* }
        );
    };
    (
        type ClientContext = $ctx:ty;
        $(#[$attr:meta])* enum $N:ident { $($var:ident($f:ty)),+ $(,)? }
    ) => {
        $crate::globals!(
            __internal,
            $ctx,
            $(#[$attr])* $(#[$attr])* () enum $N { $($var($f)),* }
        );
    };
}
