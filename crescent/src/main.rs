#![feature(type_alias_impl_trait, generic_associated_types)]
use std::{
    cell::{RefCell, RefMut},
    future::Future,
    os::unix::net::UnixStream,
    pin::Pin,
    rc::Rc,
};

use anyhow::Result;
use futures_util::TryStreamExt;
use log::debug;
use wl_io::buf::{BufReaderWithFd, BufWriterWithFd};
use wl_macros::message_broker;
use wl_server::connection::{self, Connection as _};

#[message_broker]
#[wayland(connection_context = "CrescentClient")]
pub enum Messages {
    #[wayland(impl = "::wl_server::objects::Display")]
    WlDisplay,
    #[wayland(impl = "::wl_server::objects::Registry::<Ctx>")]
    WlRegistry,
}

#[derive(Debug, Default)]
pub struct CrescentState {
    globals: wl_server::server::GlobalStore<Crescent>,
}

#[derive(Debug, Clone)]
pub struct Crescent(Rc<CrescentState>);

impl wl_server::server::Server for Crescent {
    type Connection = CrescentClient;
    type Globals = wl_server::server::GlobalStore<Self>;

    fn globals(&self) -> &Self::Globals {
        &self.0.globals
    }
}

#[derive(Debug)]
pub struct CrescentClient {
    store:        connection::Store,
    serial:       connection::EventSerial<()>,
    event_handle: wl_server::events::EventFlags,
    event_states: wl_server::connection::SlottedStates,
    state:        Crescent,
    tx:           Rc<RefCell<BufWriterWithFd<wl_io::WriteWithFd>>>,
}

impl wl_server::connection::Evented<CrescentClient> for CrescentClient {
    fn event_handle(&self) -> &wl_server::events::EventFlags {
        &self.event_handle
    }

    fn set_state<T: wl_server::provide_any::Provider + 'static>(
        &self,
        slot: u8,
        state: T,
    ) -> Result<(), T> {
        self.event_states.set(slot, state)
    }

    fn with_state<T: 'static, S>(
        &self,
        slot: u8,
        f: impl FnOnce(&T) -> S,
    ) -> Result<Option<S>, ()> {
        self.event_states.with(slot, f)
    }

    fn with_state_mut<T: 'static, S>(
        &self,
        slot: u8,
        f: impl FnOnce(&mut T) -> S,
    ) -> Result<Option<S>, ()> {
        self.event_states.with_mut(slot, f)
    }

    fn remove_state(&self, slot: u8) -> Result<Box<dyn wl_server::provide_any::Provider>, ()> {
        self.event_states.remove(slot)
    }
}

impl connection::Connection for CrescentClient {
    type Context = Crescent;
    type Error = std::io::Error;

    type Flush<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type Send<'a, M> = impl Future<Output = Result<(), Self::Error>> + 'a where M: 'a;

    fn server_context(&self) -> &Self::Context {
        &self.state
    }

    fn send<'a, 'b, 'c, M: wl_io::Serialize + Unpin + std::fmt::Debug + 'b>(
        &'a self,
        object_id: u32,
        msg: M,
    ) -> Self::Send<'c, M>
    where
        'a: 'c,
        'b: 'c,
    {
        connection::send_to(&self.tx, object_id, msg)
    }

    fn flush(&self) -> Self::Flush<'_> {
        connection::flush_to(&self.tx)
    }
}

impl wl_common::Serial for CrescentClient {
    type Data = ();

    fn next_serial(&self, data: Self::Data) -> u32 {
        self.serial.next_serial(data)
    }

    fn get(&self, serial: u32) -> Option<Self::Data> {
        self.serial.get(serial)
    }

    fn expire(&self, serial: u32) -> bool {
        self.serial.expire(serial)
    }

    fn with<F, R>(&self, serial: u32, f: F) -> Option<R>
    where
        F: FnOnce(&Self::Data) -> R,
    {
        self.serial.with(serial, f)
    }

    fn for_each<F>(&self, f: F)
        where
            F: FnMut(u32, &Self::Data) {
        self.serial.for_each(f)
    }

    fn find_map<F, R>(&self, f: F) -> Option<R>
    where
        F: FnMut(&Self::Data) -> Option<R>,
    {
        self.serial.find_map(f)
    }
}

// Forwarding implementation of ObjectStore
impl connection::Objects for CrescentClient {
    type Entry<'a> = <connection::Store as connection::Objects>::Entry<'a>;

    fn insert<T: connection::InterfaceMeta + 'static>(&self, id: u32, object: T) -> Result<(), T> {
        self.store.insert(id, object)
    }

    fn get(&self, id: u32) -> Option<Rc<dyn connection::InterfaceMeta>> {
        self.store.get(id)
    }

    fn remove(&self, id: u32) -> Option<Rc<dyn connection::InterfaceMeta>> {
        let ret = self.store.remove(id);
        ret.as_ref()
            .map(|obj| wl_server::connection::drop_object(&**obj, self));
        ret
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
        use wl_server::connection::{EventSerial, Objects, Store};
        let state = self.clone();
        Box::pin(async move {
            let (rx, tx) = ::wl_io::split_unixstream(conn)?;
            let mut client_ctx = CrescentClient {
                store: Store::new(),
                serial: EventSerial::new(std::time::Duration::from_secs(2)),
                event_handle: Default::default(),
                state,
                event_states: Default::default(),
                tx: Rc::new(RefCell::new(BufWriterWithFd::new(tx))),
            };
            client_ctx
                .store
                .insert(1, wl_server::objects::Display)
                .unwrap();
            let mut read = BufReaderWithFd::new(rx);
            let _span = tracing::debug_span!("main loop").entered();
            loop {
                Messages::dispatch(&mut client_ctx, Pin::new(&mut read)).await?;
                Messages::handle_events(&mut client_ctx).await?;
                client_ctx.flush().await?;
            }
        })
    }
}

fn main() -> Result<()> {
    use futures_util::future;
    tracing_subscriber::fmt::init();
    let (listener, _guard) = wl_server::wayland_listener_auto()?;
    let listener = smol::Async::new(listener)?;
    let mut server = Crescent(Rc::new(Default::default()));
    Messages::init_server(&mut server);
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
