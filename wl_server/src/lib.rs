#![allow(incomplete_features)]
#![feature(type_alias_impl_trait, trait_upcasting)]

use futures_lite::stream::StreamExt;
use hashbrown::{HashMap, hash_map};
use thiserror::Error;

pub mod connection;
pub mod error;
pub mod events;
pub mod globals;
pub mod objects;
pub mod provide_any;
pub mod renderer_capability;
pub mod server;
pub mod utils;

#[doc(hidden)]
pub mod __private {
    // Re-exports used by macros
    pub use static_assertions::assert_impl_all;
    pub use wl_io::traits::{
        buf::{AsyncBufReadWithFd, AsyncBufReadWithFdExt},
        de::{Deserialize, Error as DeserError},
        ser::Serialize,
    };
    pub use wl_protocol::{wayland::wl_display, ProtocolError};
    pub use wl_types;
}

#[derive(Error, Debug)]
pub enum ConnectionManagerError<SE, LE> {
    #[error("Server error: {0}")]
    Server(#[source] SE),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Listener error: {0}")]
    Listener(#[source] LE),
}

pub struct ConnectionManager<S, Ctx> {
    listeners: S,
    ctx:       Ctx,
}

impl<S, Ctx, I, E> ConnectionManager<S, Ctx>
where
    S: futures_lite::stream::Stream<Item = Result<I, E>> + Unpin,
    Ctx: crate::server::Server<Conn = I>,
{
    pub fn new(listeners: S, ctx: Ctx) -> Self {
        Self { listeners, ctx }
    }

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

#[derive(Error, Debug)]
pub enum ListenerError {
    #[error("I/O error: {0}")]
    SmolIo(#[from] std::io::Error),
    #[error("xdg base directory error: {0}")]
    XdgBaseDirectory(#[from] xdg::BaseDirectoriesError),
    #[error("unix error: {0}")]
    Unix(#[from] rustix::io::Errno),
}

pub struct FlockGuard {
    fd: rustix::io::OwnedFd,
}

impl Drop for FlockGuard {
    fn drop(&mut self) {
        use rustix::fd::AsFd;
        rustix::fs::flock(self.fd.as_fd(), rustix::fs::FlockOperation::Unlock).unwrap();
    }
}

pub fn wayland_listener(
    display: &str,
) -> Result<(std::os::unix::net::UnixListener, FlockGuard), ListenerError> {
    use rustix::fd::AsFd;
    let xdg_dirs = xdg::BaseDirectories::new()?;
    let path = xdg_dirs.place_runtime_file(display)?;
    let lock_path = xdg_dirs.place_runtime_file(format!("{}.lock", display))?;
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

pub fn wayland_listener_auto(
) -> Result<(std::os::unix::net::UnixListener, FlockGuard), ListenerError> {
    const MAX_DISPLAYNO: u32 = 32;
    let mut last_err = None;
    for i in 0..MAX_DISPLAYNO {
        let display = format!("wayland-{}", i);
        match wayland_listener(&display) {
            Ok((listener, guard)) => return Ok((listener, guard)),
            e @ Err(_) => {
                last_err = Some(e);
            },
        }
    }
    last_err.unwrap()
}

/// Event serial management.
///
/// This trait allocates serial numbers, while keeping track of allocated
/// numbers and their associated data.
///
/// Some expiration scheme might be employed by the implementation to free up
/// old serial numbers.
pub trait Serial {
    type Data;
    type Iter<'a>: Iterator<Item = (u32, &'a Self::Data)> + 'a
    where
        Self: 'a;
    /// Get the next serial number in sequence. A piece of data can be attached
    /// to each serial, storing, for example, what this event is about.
    fn next_serial(&mut self, data: Self::Data) -> u32;
    /// Get the data associated with the given serial.
    fn get(&self, serial: u32) -> Option<&Self::Data>;
    fn iter(&self) -> Self::Iter<'_>;
    /// Remove the serial number from the list of allocated serials.
    fn expire(&mut self, serial: u32) -> bool;
}

pub struct IdAlloc<D> {
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
        impl<'a, K: std::fmt::Debug, V> std::fmt::Debug for DebugMap<'a, K, V> {
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

    type Iter<'a> = impl Iterator<Item = (u32, &'a Self::Data)> + 'a where Self: 'a;

    fn iter(&self) -> Self::Iter<'_> {
        self.data.iter().map(|(k, v)| (*k, v))
    }

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
///     #[derive(Debug, Object)]
///     #[wayland(context = "ClientContext")]
///     pub enum Objects {
///         // Extra objects
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
        $(#[$attr2:meta])* enum $N2:ident { $($var2:ident($f2:ty)),* $(,)? }
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
        impl $crate::globals::Bind<$ctx> for $N {
            type Objects = $N2;
            type BindFut<'a> = impl std::future::Future<Output = std::io::Result<Self::Objects>> + 'a
            where
                Self: 'a;
            fn interface(&self) -> &'static str {
                match self {
                    $(
                        $N::$var(f) => <$f as $crate::globals::Bind<$ctx>>::interface(f),
                    )*
                }
            }
            fn version(&self) -> u32 {
                match self {
                    $(
                        $N::$var(f) => <$f as $crate::globals::Bind<$ctx>>::version(f),
                    )*
                }
            }
            fn bind<'a>(&'a self, client: &'a mut $ctx, object_id: u32) -> Self::BindFut<'a> {
                async move {
                    Ok(match self {$(
                        $N::$var(f) => <$f as $crate::globals::Bind<$ctx>>::bind(f, client, object_id).await?.into(),
                    )*})
                }
            }
        }
        impl $N {
            $($vis)? fn globals() -> impl Iterator<Item = $N> {
                [$($N::$var(<$f as $crate::globals::ConstInit>::INIT)),*].into_iter()
            }
        }
        impl $ctx {
            $($vis)? async fn dispatch<R>(
                &mut self,
                mut reader: Pin<&mut R>) -> bool
            where
                R: $crate::__private::AsyncBufReadWithFd
            {
                use $crate::__private::AsyncBufReadWithFdExt;
                use $crate::{
                    __private::{
                        wl_types, wl_display::v1 as wl_display, ProtocolError,
                    },
                    objects::DISPLAY_ID
                };
                let (object_id, len, buf, fd) =
                    match R::next_message(reader.as_mut()).await {
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
                let (ret, bytes_read, fds_read) = <<Self as $crate::connection::ClientContext>::Object as $crate::objects::Object<Self>>::dispatch(
                    obj.as_ref(),
                    self,
                    object_id,
                    (buf, fd),
                ).await;
                let (mut fatal, error) = match ret {
                    Ok(_) => (false, None),
                    Err(e) =>(
                        e.fatal(),
                        e.wayland_error()
                        .map(|(object_id, error_code)|
                             (
                                 object_id,
                                 error_code,
                                 std::ffi::CString::new(e.to_string()).unwrap()
                             )
                        )
                    )
                };
                if let Some((object_id, error_code, msg)) = error {
                    // We are going to disconnect the client so we don't care about the
                    // error.
                    fatal |= self.send(DISPLAY_ID, wl_display::events::Error {
                        object_id: wl_types::Object(object_id),
                        code: error_code,
                        message: wl_types::Str(msg.as_c_str()),
                    }).await.is_err();
                }
                if !fatal {
                    use $crate::objects::Object;
                    if bytes_read != len as usize {
                        let len_opcode = u32::from_ne_bytes(buf[0..4].try_into().unwrap());
                        let opcode = len_opcode & 0xffff;
                        tracing::error!("unparsed bytes in buffer, {bytes_read} != {len}. object_id: {}@{object_id}, opcode: {opcode}",
                                        obj.interface());
                        fatal = true;
                    }
                    reader.consume(bytes_read, fds_read);
                }
                fatal
            }
        }
        $(#[$attr2])*
        $($vis)? enum $N2 {
            $($var(<$f as $crate::globals::Bind<$ctx>>::Objects)),*,
            $($var2($f2)),*
        }
    };
    (
        type ClientContext = $ctx:ty;
        $(#[$attr:meta])* pub enum $N:ident { $($var:ident($f:ty)),+ $(,)? }
        $(#[$attr2:meta])* pub enum $N2:ident { $($var2:ident($f2:ty)),* $(,)? }
    ) => {
        $crate::globals!(
            __internal,
            $ctx,
            $(#[$attr])* $(#[$attr])* (pub) enum $N { $($var($f)),* } $(#[$attr2])*
            enum $N2 { $($var2($f2)),* }
        );
    };
    (
        type ClientContext = $ctx:ty;
        $(#[$attr:meta])* enum $N:ident { $($var:ident($f:ty)),+ $(,)? }
        $(#[$attr2:meta])* enum $N2:ident { $($var2:ident($f2:ty)),* $(,)? }
    ) => {
        $crate::globals!(
            __internal,
            $ctx,
            $(#[$attr])* $(#[$attr])* () enum $N { $($var($f)),* } $(#[$attr2])*
            enum $N2 { $($var2($f2)),* }
        );
    };
}
