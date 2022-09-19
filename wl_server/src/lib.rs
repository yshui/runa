#![feature(type_alias_impl_trait, generic_associated_types)]
use std::pin::Pin;

use futures_lite::{stream::StreamExt, Future};
use thiserror::Error;
use wl_io::AsyncBufReadWithFd;

pub mod display;
pub mod error;
pub mod object_store;

#[derive(Error, Debug)]
pub enum Error<SE, LE> {
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

    pub async fn run(&mut self) -> Result<(), Error<Ctx::Error, E>> {
        while let Some(conn) = self.listeners.next().await {
            let conn = conn.map_err(Error::Listener)?;
            self.ctx.new_connection(conn).map_err(Error::Server)?;
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
        &lock_path,
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
    type Error: 'a;
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
        self.executor.spawn(self.ctx.new_connection(conn)).detach();
        Ok(())
    }
}

/// A server implements AsyncContext by calling wl_base::MessageDispatch
pub struct AsyncDispatchServer<Ctx> {
    ctx: Ctx,
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
    Conn: AsyncBufReadWithFd + std::fmt::Debug + Unpin + 'a,
{
    type Error = AsyncDispatchServerError<Ctx::Error>;
    type Task = Pin<Box<dyn Future<Output = Result<(), Self::Error>> + 'a>>;

    fn new_connection(&mut self, _conn: Conn) -> Self::Task {
        unimplemented!();
    }
}
