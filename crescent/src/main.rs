#![feature(type_alias_impl_trait)]
use std::{cell::RefCell, future::Future, os::unix::net::UnixStream, pin::Pin, rc::Rc};

use anyhow::Result;
use apollo::shell::DefaultShell;
use futures_util::TryStreamExt;
use log::debug;
use wl_io::buf::{BufReaderWithFd, BufWriterWithFd};
use wl_server::{
    connection::{self, Connection as _, Objects, Store},
    renderer_capability::RendererCapability,
    Extra,
};

#[derive(Debug)]
pub struct CrescentState {
    globals:   wl_server::server::GlobalStore<Crescent>,
    listeners: wl_server::server::Listeners,
    shell:     RefCell<DefaultShell>,
}

#[derive(Debug, Clone)]
pub struct Crescent(Rc<CrescentState>);

impl Crescent {
    wl_server::message_switch! {
        ctx: CrescentClient,
        error: wl_server::error::Error,
        globals: [
            wl_server::globals::Display,
            wl_server::globals::Registry,
            apollo::globals::Compositor<DefaultShell>,
            apollo::globals::Subcompositor<DefaultShell>,
            apollo::globals::Shm,
            apollo::globals::xdg_shell::WmBase<DefaultShell>,
        ],
    }
}

impl wl_server::server::Server for Crescent {
    type Connection = CrescentClient;
    type Globals = wl_server::server::GlobalStore<Self>;

    fn globals(&self) -> &Self::Globals {
        &self.0.globals
    }
}

impl Extra<RefCell<DefaultShell>> for Crescent {
    fn extra(&self) -> &RefCell<DefaultShell> {
        &self.0.shell
    }
}

impl wl_server::server::EventSource for Crescent {
    fn add_listener(&self, handle: wl_server::events::EventHandle) {
        self.0.listeners.add_listener(handle);
    }

    fn remove_listener(&self, handle: wl_server::events::EventHandle) -> bool {
        self.0.listeners.remove_listener(handle)
    }

    fn notify(&self, slot: u8) {
        self.0.listeners.notify(slot);
    }
}

impl RendererCapability for Crescent {
    fn formats(&self) -> Vec<wl_server::renderer_capability::Format> {
        use wl_server::renderer_capability::Format;
        vec![Format::Argb8888]
    }
}

#[derive(Debug)]
pub struct CrescentClient {
    store:        RefCell<connection::Store<Self>>,
    serial:       connection::EventSerial<()>,
    event_flags:  wl_server::events::EventFlags,
    event_states: wl_server::connection::SlottedStates,
    state:        Crescent,
    tx:           Rc<RefCell<BufWriterWithFd<wl_io::WriteWithFd>>>,
}

impl wl_server::connection::Evented<CrescentClient> for CrescentClient {
    type States = wl_server::connection::SlottedStates;

    fn event_handle(&self) -> wl_server::events::EventHandle {
        self.event_flags.as_handle()
    }

    fn reset_events(&self) -> wl_server::events::Flags {
        self.event_flags.reset()
    }

    fn event_states(&self) -> &Self::States {
        &self.event_states
    }
}

impl connection::Connection for CrescentClient {
    type Context = Crescent;
    type Objects = Store<Self>;

    type Flush<'a> = impl Future<Output = Result<(), std::io::Error>> + 'a;
    type Send<'a, M> = impl Future<Output = Result<(), std::io::Error>> + 'a where M: 'a;

    fn server_context(&self) -> &Self::Context {
        &self.state
    }

    fn send<'a, 'b, 'c, M: wl_io::traits::ser::Serialize + Unpin + std::fmt::Debug + 'b>(
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

    fn objects(&self) -> &RefCell<Self::Objects> {
        &self.store
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
        F: FnMut(u32, &Self::Data),
    {
        self.serial.for_each(f)
    }

    fn find_map<F, R>(&self, f: F) -> Option<R>
    where
        F: FnMut(&Self::Data) -> Option<R>,
    {
        self.serial.find_map(f)
    }
}

impl Drop for CrescentClient {
    fn drop(&mut self) {
        self.store.borrow_mut().clear(self);
    }
}

impl<'a> wl_server::AsyncContext<'a, UnixStream> for Crescent {
    type Error = ::wl_server::error::Error;

    type Task = impl std::future::Future<Output = Result<(), Self::Error>> + 'a;

    fn new_connection(&mut self, conn: UnixStream) -> Self::Task {
        debug!("New connection");
        use wl_server::connection::EventSerial;
        let state = self.clone();
        Box::pin(async move {
            let (rx, tx) = ::wl_io::split_unixstream(conn)?;
            let mut client_ctx = CrescentClient {
                store: RefCell::new(Store::new()),
                serial: EventSerial::new(std::time::Duration::from_secs(2)),
                event_flags: Default::default(),
                state,
                event_states: Default::default(),
                tx: Rc::new(RefCell::new(BufWriterWithFd::new(tx))),
            };
            client_ctx
                .objects()
                .borrow_mut()
                .insert(1, wl_server::objects::Display)
                .unwrap();
            let mut read = BufReaderWithFd::new(rx);
            let _span = tracing::debug_span!("main loop").entered();
            while !Crescent::dispatch(&mut client_ctx, Pin::new(&mut read)).await {
                Crescent::handle_events(&mut client_ctx).await?;
                client_ctx.flush().await?;
            }
            client_ctx.flush().await?;
            Ok(())
        })
    }
}

fn main() -> Result<()> {
    use futures_util::future;
    tracing_subscriber::fmt::init();
    let (listener, _guard) = wl_server::wayland_listener_auto()?;
    let listener = smol::Async::new(listener)?;
    let server = Crescent(Rc::new(CrescentState {
        globals:   Crescent::globals().collect(),
        listeners: Default::default(),
        shell:     Default::default(),
    }));
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
