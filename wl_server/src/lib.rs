#![allow(incomplete_features)]
#![feature(type_alias_impl_trait, trait_upcasting)]
use std::pin::Pin;

use futures_lite::{stream::StreamExt, Future};
use thiserror::Error;
use wl_io::traits::buf::AsyncBufReadWithFd;

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
            de::Deserializer,
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

pub trait Server<Conn> {
    type Error;

    fn new_connection(&mut self, conn: Conn) -> Result<(), Self::Error>;
}

impl<S, Ctx, I, E> ConnectionManager<S, Ctx>
where
    S: futures_lite::stream::Stream<Item = Result<I, E>> + Unpin,
    Ctx: Server<I>,
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

/// A server implementation that spawn a new async task for each new connection.
/// The task is created by the AsyncContext implementation of `Ctx`.
/// If E is the LocalExecutor, then the Task created by `Ctx` doesn't need to be
/// Send + 'static. For now, AsyncServer is only implemented for E =
/// LocalExecutor.
pub struct AsyncServer<'exe, Ctx, E> {
    ctx:      Ctx,
    executor: &'exe E,
}

/// A trait implemented by the context used in AsyncServer. `new_connection` is
/// called for each new connection, and returns an async task that will be
/// spawned by the AsyncServer.
///
/// 'a is the lifetime of futures returned by `new_connection`, and errors
/// returned by the futures. For using with LocalExecutor, 'a should outlive the
/// LocalExecutor. For a threaded executor, 'a needs to be 'static.
pub trait AsyncContext<'a, Conn> {
    type Error: std::error::Error + 'a;
    type Task: Future<Output = Result<(), Self::Error>> + Unpin + 'a;
    fn new_connection(&mut self, conn: Conn) -> Self::Task;
}

impl<'a, 'b, Ctx> AsyncServer<'a, Ctx, smol::LocalExecutor<'b>> {
    pub fn new(ctx: Ctx, executor: &'a smol::LocalExecutor<'b>) -> Self {
        Self { ctx, executor }
    }
}

impl<'a, 'b, Conn: 'b, Ctx> Server<Conn> for AsyncServer<'a, Ctx, smol::LocalExecutor<'b>>
where
    Ctx: AsyncContext<'b, Conn> + 'b,
{
    type Error = Ctx::Error;

    fn new_connection(&mut self, conn: Conn) -> Result<(), Self::Error> {
        use futures_util::future::TryFutureExt;
        self.executor
            .spawn(
                self.ctx
                    .new_connection(conn)
                    .map_err(|e| tracing::error!("Error while handling connection: {}", e)),
            )
            .detach();
        Ok(())
    }
}

/// A server implements AsyncContext by calling wl_base::MessageDispatch
pub struct AsyncDispatchServer<Ctx> {
    _ctx: Ctx,
}

#[derive(Error, Debug)]
pub enum AsyncDispatchServerError<E> {
    #[error("Dispatch error: {0}")]
    Dispatch(#[source] E),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl<'a, Conn, Ctx> AsyncContext<'a, Conn> for AsyncDispatchServer<Ctx>
where
    Ctx: wl_common::MessageDispatch + Clone + 'a,
    Ctx::Error: std::error::Error + 'static,
    Conn: AsyncBufReadWithFd + std::fmt::Debug + Unpin + 'a,
{
    type Error = AsyncDispatchServerError<Ctx::Error>;
    type Task = Pin<Box<dyn Future<Output = Result<(), Self::Error>> + 'a>>;

    fn new_connection(&mut self, _conn: Conn) -> Self::Task {
        unimplemented!();
    }
}

/// Generate message dispatch and event handling functions, given a list of
/// global objects.
///
/// This macro generate these functions:
///
/// * Dispatch
///
/// ```rust
/// async fn dispatch<'a, 'b: 'a, R>(ctx: &'a mut Ctx, reader: Pin<&mutR>) -> bool;
/// ```
///
/// This uses the object store in `Ctx` to dispatch a message from `reader`.
/// Returns whether the client connection needs to be dropped.
///
/// * Event handling
///
/// ```rust
/// async handle_events(ctx: &mut Ctx) -> Result<(), Error>;
/// ```
///
/// This handles events set on `Ctx`'s event handle. See
/// [`crate::connection::Evented`] and [`crate::server::EventSource`] for more
/// info on the mechanism. See [`crate::server::Globals`] for an example of
/// `EventSource`.
///
/// # Example
///
/// ```rust
/// use wl_server::globals::*;
/// message_switch! {
///     error: wl_server::error::Error,
///     globals: [ Display, Registry ],
///     ctx: ClientContext,
/// }
/// ```
#[macro_export]
macro_rules! message_switch {
    (ctx: $ctx:ty, error: $error:ty, globals: [$($global:ty),+$(,)?],$(,)?) => {
        const _: () = {
            impl $ctx {
                async fn dispatch<'a, 'b: 'a, R>(
                    &'a mut self,
                    mut reader: Pin<&mut R>) -> bool
                where
                    R: $crate::__private::AsyncBufReadWithFd + 'b
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
                    let (mut fatal, error) = 'dispatch: {
                        let obj = self.objects().borrow().get(object_id).map(Rc::clone);
                        if let Some(obj) = obj {
                            $({
                                let tmp = <$global as $crate::globals::GlobalDispatch<_>>::
                                    dispatch(obj.as_ref(), self, object_id, de.borrow_mut()).await;
                                match tmp {
                                    Ok(true) => {
                                        tracing::trace!("Dispatched {} to {}", obj.interface(),
                                                        std::any::type_name::<$global>());
                                        break 'dispatch (false, None)
                                    },
                                    Err(e) => break 'dispatch (
                                        e.fatal(),
                                        e.wayland_error()
                                        .map(|(object_id, error_code)|
                                            (
                                                object_id,
                                                error_code,
                                                std::ffi::CString::new(e.to_string()).unwrap()
                                            )
                                        )
                                    ),
                                    Ok(false) => {
                                        // not dispatched
                                        assert_eq!(de.consumed(), (0, 0));
                                    }
                                }
                            })+
                            panic!("Unhandled interface {}", obj.interface());
                        } else {
                            (
                                true,
                                Some((
                                    DISPLAY_ID,
                                    wl_display::enums::Error::InvalidObject as u32,
                                    std::ffi::CString::new(format!("Invalid object id {}", object_id)).unwrap()
                                ))
                            )
                        }
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
                        assert_eq!(bytes_read, len as usize, "unparsed bytes in buffer {object_id}");
                        reader.consume(bytes_read, fds_read);
                    }
                    fatal
                }
                fn globals() -> impl Iterator<Item = Box<dyn $crate::globals::Global<$ctx>>> {
                    [$(Box::new(<$global as $crate::globals::GlobalDispatch<$ctx>>::INIT) as _),+].into_iter()
                }
            }
        };
    };

    (ctx: $ctx:ty, globals: [$($global:ty),+$(,)?], error: $error:ty $(,)?) => {
        $crate::message_switch! {
            ctx: $ctx,
            error: $error,
            globals: [ $($global),+ ],
        }
    };
    ($id:ident: $val:tt, $($id2:ident: $val2:tt),+$(,)?) => {
        $crate::message_switch! {$($id2: $val2),+, $id: $val }
    }
}
