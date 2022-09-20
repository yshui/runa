#![feature(type_alias_impl_trait, generic_associated_types)]
use std::{os::unix::net::UnixStream, pin::Pin, rc::Rc};

use anyhow::Result;
use futures_util::TryStreamExt;
use log::debug;
use wl_io::buf::BufReaderWithFd;
use wl_macros::message_broker;

#[message_broker]
#[wayland(connection_context = "CrescentClient")]
pub enum Messages {
    #[wayland(impl = "::wl_server::objects::Display")]
    WlDisplay,
}

#[derive(Debug, Clone)]
pub struct CrescentState;

#[derive(Debug, Clone)]
pub struct Crescent(Rc<CrescentState>);

impl wl_server::server::Server for Crescent {
    type Connection = CrescentClient;
}

impl wl_server::server::Globals for Crescent {
    fn bind(&self, client: &Self::Connection, id: u32) -> Box<dyn wl_server::connection::InterfaceMeta> {
        todo!()
    }
    fn bind_registry(&self, client: &Self::Connection) -> Box<dyn wl_server::connection::InterfaceMeta> {
        todo!()
    }
}

#[derive(Debug, Clone)]
pub struct CrescentClient {
    store: Rc<wl_server::connection::Store>,
    serial: Rc<wl_server::connection::EventSerial<()>>,
    state: Crescent,
}

impl wl_server::connection::Connection for CrescentClient {
    type Context = Crescent;

    fn server_context(&self) -> &Self::Context {
        &self.state
    }
}

impl wl_server::connection::Serial for CrescentClient {
    type Data = ();
    fn next_serial(&self, data: Self::Data) -> u32 {
        self.serial.next_serial(data)
    }
    fn get_data(&self, serial: u32) -> Option<Self::Data> {
        self.serial.get_data(serial)
    }
}

// Forwarding implementation of ObjectStore
impl wl_server::connection::Objects for CrescentClient {
    type Entry<'a> = <wl_server::connection::Store as wl_server::connection::Objects>::Entry<'a>;

    fn insert<T: wl_server::connection::InterfaceMeta + 'static>(
        &self,
        id: u32,
        object: T,
    ) -> Result<(), T> {
        self.store.insert(id, object)
    }

    fn get(&self, id: u32) -> Option<Rc<dyn wl_server::connection::InterfaceMeta>> {
        self.store.get(id)
    }

    fn remove(&self, id: u32) -> Option<Rc<dyn wl_server::connection::InterfaceMeta>> {
        self.store.remove(id)
    }

    fn global_remove(&self, global: u32) {
        self.store.global_remove(global)
    }

    fn with_entry<T>(&self, id: u32, f: impl FnOnce(Self::Entry<'_>) -> T) -> T {
        self.store.with_entry(id, f)
    }
}

impl<'a> wl_server::AsyncContext<'a, UnixStream> for Crescent {
    type Error = ::wl_server::error::Error;

    type Task = impl std::future::Future<Output = Result<(), Self::Error>> + 'a;

    fn new_connection(&mut self, conn: UnixStream) -> Self::Task {
        debug!("New connection");
        use wl_server::connection::{Objects, Store, EventSerial};
        let client_ctx = CrescentClient {
            store: Rc::new(Store::new()),
            serial: Rc::new(EventSerial::new(std::time::Duration::from_secs(2))),
            state: self.clone(),
        };
        client_ctx
            .store
            .insert(1, wl_server::objects::Display)
            .unwrap();
        Box::pin(async move {
            let (rx, tx) = ::wl_io::split_unixstream(conn)?;
            let mut read = BufReaderWithFd::new(rx);
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
