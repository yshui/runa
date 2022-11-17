#![feature(type_alias_impl_trait)]
use std::{cell::RefCell, future::Future, os::unix::net::UnixStream, pin::Pin, rc::Rc};

use anyhow::Result;
use apollo::shell::{
    buffers::{HasBuffer, RendererBuffer},
    DefaultShell, HasShell,
};
use futures_util::{FutureExt, TryStreamExt};
use log::debug;
use wl_io::buf::{BufReaderWithFd, BufWriterWithFd};
use wl_server::{
    connection::{self, Connection as _, Objects, Store},
    events::EventMux,
    renderer_capability::RendererCapability, __private::AsyncBufReadWithFdExt,
};
mod render;

#[derive(Debug)]
pub struct CrescentState {
    globals: RefCell<wl_server::server::GlobalStore<CrescentClient>>,
    shell:   Rc<RefCell<<Crescent as HasShell>::Shell>>,
}

#[derive(Debug, Clone)]
pub struct Crescent(Rc<CrescentState>);

wl_server::message_switch! {
    ctx: CrescentClient,
    error: wl_server::error::Error,
    globals: [
        wl_server::globals::Display,
        wl_server::globals::Registry,
        apollo::globals::Compositor,
        apollo::globals::Subcompositor,
        apollo::globals::Shm,
        apollo::globals::xdg_shell::WmBase,
    ],
}

impl wl_server::server::Server for Crescent {
    type Connection = CrescentClient;
    type Globals = wl_server::server::GlobalStore<Self::Connection>;

    fn globals(&self) -> &RefCell<Self::Globals> {
        &self.0.globals
    }
}
impl HasBuffer for Crescent {
    type Buffer = RendererBuffer<render::BufferData>;
}

impl HasShell for Crescent {
    type Shell = DefaultShell<<Self as HasBuffer>::Buffer>;

    fn shell(&self) -> &RefCell<Self::Shell> {
        &self.0.shell
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
    store:       RefCell<connection::Store>,
    per_client:  wl_server::utils::UnboundedAggregate,
    event_flags: wl_server::events::EventFlags,
    state:       Crescent,
    tx:          Rc<RefCell<BufWriterWithFd<wl_io::WriteWithFd>>>,
}

impl Drop for CrescentClient {
    fn drop(&mut self) {
        let empty_store = Default::default();
        let mut store = self.store.replace(empty_store);
        store.clear_for_disconnect(self)
    }
}

impl<T: std::any::Any> connection::State<T> for CrescentClient {
    fn state(&self) -> Option<&T> {
        self.per_client.get::<T>()
    }

    fn state_mut(&mut self) -> Option<&mut T> {
        self.per_client.get_mut::<T>()
    }

    fn set_state(&mut self, state: T) {
        self.per_client.set::<T>(state)
    }
}

wl_server::event_multiplexer! {
    ctx: CrescentClient,
    error: wl_server::error::Error,
    receivers: [
        wl_server::globals::Registry,
        apollo::globals::Compositor,
    ],
}
impl EventMux for CrescentClient {
    fn event_handle(&self) -> wl_server::events::EventHandle {
        self.event_flags.as_handle()
    }
}

impl connection::Connection for CrescentClient {
    type Context = Crescent;
    type Objects = Store;

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

impl<'a> wl_server::AsyncContext<'a, UnixStream> for Crescent {
    type Error = ::wl_server::error::Error;

    type Task = impl std::future::Future<Output = Result<(), Self::Error>> + 'a;

    fn new_connection(&mut self, conn: UnixStream) -> Self::Task {
        debug!("New connection");
        let state = self.clone();
        Box::pin(async move {
            let (rx, tx) = ::wl_io::split_unixstream(conn)?;
            let mut client_ctx = CrescentClient {
                store: RefCell::new(Store::default()),
                per_client: Default::default(),
                event_flags: Default::default(),
                state,
                tx: Rc::new(RefCell::new(BufWriterWithFd::new(tx))),
            };
            client_ctx
                .objects()
                .borrow_mut()
                .insert(1, wl_server::objects::Display)
                .unwrap();
            let mut read = BufReaderWithFd::new(rx);
            let _span = tracing::debug_span!("main loop").entered();
            loop {
                // Flush output before we start waiting.
                client_ctx.flush().await?;
                futures_util::select! {
                    _ = Pin::new(&mut read).next_message().fuse() => {
                        if client_ctx.dispatch(Pin::new(&mut read)).await {
                            break;
                        }
                    },
                    _ = client_ctx.event_flags.listen().fuse() => {
                        tracing::trace!("got events");
                        let flags = client_ctx.event_flags.reset();
                        client_ctx.dispatch_events(flags).await?;
                    }
                }
            }
            client_ctx.flush().await?;
            Ok(())
        })
    }
}

fn main() -> Result<()> {
    use futures_util::future;
    tracing_subscriber::fmt::init();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let (event_tx, event_rx) = smol::channel::unbounded();
    let _el = std::thread::spawn(move || {
        use winit::platform::unix::EventLoopBuilderExtUnix;
        let el = winit::event_loop::EventLoopBuilder::new()
            .with_any_thread(true)
            .build();
        let window = winit::window::WindowBuilder::new()
            .with_title("Crescent")
            .with_maximized(true)
            .build(&el)
            .unwrap();
        tx.send(window).unwrap();
        let event_tx = event_tx;
        el.run(move |event, _, cf| {
            cf.set_wait();
            let Some(event) = event.to_static() else { return };
            smol::block_on(event_tx.send(event)).unwrap();
        })
    });
    let window = rx.recv().unwrap();
    let (listener, _guard) = wl_server::wayland_listener_auto()?;
    let listener = smol::Async::new(listener)?;
    let server = Crescent(Rc::new(CrescentState {
        globals: RefCell::new(CrescentClient::globals().collect()),
        shell:   Default::default(),
    }));
    let shell2 = server.0.shell.clone();
    let executor = smol::LocalExecutor::new();
    let server = wl_server::AsyncServer::new(server, &executor);
    let incoming = Box::pin(
        listener
            .incoming()
            .and_then(|conn| future::ready(conn.into_inner())),
    );
    tracing::debug!("Size: {:?}", window.inner_size());
    let renderer = smol::block_on(render::Renderer::new(&window, window.inner_size(), shell2));
    let mut cm = wl_server::ConnectionManager::new(incoming, server);
    let _render = executor.spawn(async move {
        renderer.render_loop(event_rx).await;
    });

    Ok(smol::block_on(executor.run(cm.run()))?)
}
