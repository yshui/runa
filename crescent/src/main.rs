#![feature(type_alias_impl_trait)]
use std::{os::unix::net::UnixStream, pin::Pin, rc::Rc};

use anyhow::Result;
use futures_util::TryStreamExt;
use log::debug;
use wl_io::{buf::BufReaderWithFd, WithFd};
use wl_macros::message_broker;

#[message_broker]
#[wayland(connection_context = "CrescentClient")]
pub enum Messages {
    #[wayland(impl = "::wl_server::display::Display")]
    WlDisplay,
}

#[derive(Debug, Clone)]
pub struct CrescentState;

#[derive(Debug, Clone)]
pub struct Crescent(Rc<CrescentState>);

#[derive(Debug, Clone)]
pub struct CrescentClient {
    store: wl_server::object_store::Store,
    state: Rc<CrescentState>,
}

// Forwarding implementation of ObjectStore
impl wl_server::object_store::ObjectStore for CrescentClient {
    fn insert<T: wl_server::object_store::InterfaceMeta + 'static>(
        &self,
        id: u32,
        object: T,
    ) -> Result<(), T> {
        self.store.insert(id, object)
    }

    fn get(&self, id: u32) -> Option<Rc<dyn wl_server::object_store::InterfaceMeta>> {
        self.store.get(id)
    }
}

impl<'a> wl_server::AsyncContext<'a, UnixStream> for Crescent {
    type Error = ::wl_server::error::Error;

    type Task = impl std::future::Future<Output = Result<(), Self::Error>> + 'a;

    fn new_connection(&mut self, conn: UnixStream) -> Self::Task {
        debug!("New connection");
        use wl_server::object_store::ObjectStore;
        let client_ctx = CrescentClient {
            store: wl_server::object_store::Store::new(),
            state: self.0.clone(),
        };
        client_ctx.store.insert(1, wl_server::display::Display);
        Box::pin(async move {
            let mut read = BufReaderWithFd::new(WithFd::try_from(conn)?);
            let _span = tracing::debug_span!("main loop").entered();
            loop {
                Messages::dispatch(&client_ctx, Pin::new(&mut read)).await?;
            }
        })
    }
}

fn main() -> Result<()> {
    use futures_util::future;
    tracing_subscriber::fmt::init();
    let (listener, _guard) = wl_server::wayland_listener_auto()?;
    let listener = smol::Async::new(listener)?;
    let server = Crescent(Rc::new(CrescentState));
    let executor = smol::LocalExecutor::new();
    let server = wl_server::AsyncServer::new(server, &executor);
    let incoming = Box::pin(
        listener
            .incoming()
            .and_then(|conn| future::ready(conn.into_inner())),
    );
    let mut cm = wl_server::ConnectionManager::new(incoming, server);

    Ok(futures_executor::block_on(executor.run(cm.run()))?)
}
