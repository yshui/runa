#![allow(incomplete_features)]
#![feature(type_alias_impl_trait, trait_upcasting)]

use futures_lite::stream::StreamExt;
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
    pub use wl_common::InterfaceMessageDispatch;
    pub use wl_io::{
        de::Deserializer as DeserializerImpl,
        traits::{
            buf::{AsyncBufReadWithFd, AsyncBufReadWithFdExt},
            de::Deserializer, de::Deserialize,
        },
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
///     #[derive(Debug, InterfaceMessageDispatch)]
///     #[wayland(context = "ClientContext")]
///     pub enum Objects {
///         // Extra objects
///     }
/// }
/// ```
///
/// The name `Globals` and `Objects` can be changed.
///
/// This will generate a `From<Variant> for Globals` for each of the variants of `Globals`. And for
/// each global, a `From<Global::Object> for Objects` will be generated.
///
/// `Objects` will be filled with variants from `Global::Object` for each of the globals. It can
/// also contain extra variants, for object types that aren't associated with a particular global.
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
            fn bind<'a>(&'a self, client: &'a mut $ctx, object_id: u32) ->
                ::std::pin::Pin<Box<dyn ::std::future::Future<Output = ::std::io::Result<Self::Objects>> + 'a>>
            {
                Box::pin(async move {
                Ok(match self {
                    $(
                        $N::$var(f) => <$f as $crate::globals::Bind<$ctx>>::bind(f, client, object_id).await?.into(),
                    )*
                })})
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
                        wl_types, wl_display::v1 as wl_display, ProtocolError, DeserializerImpl,
                    },
                    objects::DISPLAY_ID
                };
                let (object_id, len, buf, fd) =
                    match R::next_message(reader.as_mut()).await {
                        Ok(v) => v,
                        // I/O error, no point sending the error to the client
                        Err(e) => return true,
                    };
                let mut de = DeserializerImpl::new(buf, fd);
                use $crate::connection::Objects;
                let Some(obj) = self.objects().borrow().get(object_id).map(Rc::clone) else {
                    let _ = self.send(DISPLAY_ID, wl_display::events::Error {
                        object_id: wl_types::Object(object_id),
                        code: 0,
                        message: wl_types::str!("Invalid object ID"),
                    }).await; // don't care about the error.
                    return true;
                };
                let ret = <<Self as $crate::connection::ClientContext>::Object as $crate::__private::InterfaceMessageDispatch<Self>>::dispatch(
                    obj.as_ref(),
                    self,
                    object_id,
                    de.borrow_mut()
                ).await;
                let (mut fatal, error) = match ret {
                    Ok(()) => (false, None),
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
                    let (bytes_read, fds_read) = de.consumed();
                    if bytes_read != len as usize {
                        tracing::error!("unparsed bytes in buffer {object_id}");
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
