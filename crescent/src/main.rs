#![feature(type_alias_impl_trait)]
use std::{cell::RefCell, future::Future, os::unix::net::UnixStream, pin::Pin, rc::Rc};

use anyhow::Result;
use apollo::shell::{
    buffers::{HasBuffer, RendererBuffer},
    HasShell,
};
use futures_util::{FutureExt, TryStreamExt};
use smol::{LocalExecutor, Task};
use wl_io::buf::{BufReaderWithFd, BufWriterWithFd};
use wl_server::{
    __private::AsyncBufReadWithFdExt,
    connection::{self, ClientContext as _, Store},
    events::EventMux,
    objects::Object,
    renderer_capability::RendererCapability,
};
mod render;
mod shell;
use shell::DefaultShell;

#[derive(Debug)]
pub struct CrescentState {
    globals:  RefCell<wl_server::server::GlobalStore<CrescentClient>>,
    shell:    Rc<RefCell<<Crescent as HasShell>::Shell>>,
    executor: LocalExecutor<'static>,
}

#[derive(Debug, Clone)]
pub struct Crescent(Rc<CrescentState>);

wl_server::globals! {
    type ClientContext = CrescentClient;
    pub enum Globals {
        Display(wl_server::globals::Display),
        Registry(wl_server::globals::Registry),
        Compositor(apollo::globals::Compositor),
        Subcompositor(apollo::globals::Subcompositor),
        Shm(apollo::globals::Shm),
        WmBase(apollo::globals::xdg_shell::WmBase),
    }
}

type Shell = <Crescent as HasShell>::Shell;
#[derive(Object, Debug)]
#[wayland(context = "CrescentClient")]
pub enum Objects {
    // === core objects ===
    Display(wl_server::objects::Display),
    Registry(wl_server::objects::Registry),
    Callback(wl_server::objects::Callback),

    // === compositor objects ===
    Compositor(apollo::objects::compositor::Compositor),
    Surface(apollo::objects::compositor::Surface<Shell>),
    Subcompositor(apollo::objects::compositor::Subcompositor),
    Subsurface(apollo::objects::compositor::Subsurface<Shell>),

    // === xdg_shell objects ===
    WmBase(apollo::objects::xdg_shell::WmBase),
    XdgSurface(apollo::objects::xdg_shell::Surface<Shell>),
    XdgTopLevel(apollo::objects::xdg_shell::TopLevel<Shell>),

    // === shm objects ===
    Shm(apollo::objects::shm::Shm),
    ShmPool(apollo::objects::shm::ShmPool),

    // === buffer ===
    Buffer(apollo::objects::Buffer<RendererBuffer<render::BufferData>>),
}

impl wl_server::server::Server for Crescent {
    type ClientContext = CrescentClient;
    type Conn = UnixStream;
    type Error = ();
    type Global = Globals;
    type Globals = wl_server::server::GlobalStore<Self::ClientContext>;

    fn globals(&self) -> &RefCell<Self::Globals> {
        &self.0.globals
    }

    fn new_connection(&self, conn: UnixStream) -> Result<(), Self::Error> {
        tracing::debug!("New connection");
        let state = self.clone();
        self.0
            .executor
            .spawn(async move {
                use wl_server::connection::Objects as _;
                let (rx, tx) = ::wl_io::split_unixstream(conn)?;
                let mut client_ctx = CrescentClient {
                    store: RefCell::new(Store::default()),
                    per_client: Default::default(),
                    event_flags: Default::default(),
                    state,
                    tasks: Default::default(),
                    tx: Rc::new(RefCell::new(BufWriterWithFd::new(tx))),
                };
                client_ctx
                    .objects()
                    .borrow_mut()
                    .insert(
                        1,
                        wl_server::objects::Display,
                    )
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
                        flags = client_ctx.event_flags.listen().fuse() => {
                            tracing::trace!("got events");
                            client_ctx.dispatch_events(flags).await?;
                        }
                    }
                }
                client_ctx.flush().await?;
                Ok::<(), wl_server::error::Error>(())
            })
            .detach();
        Ok(())
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
    store:       RefCell<connection::Store<Objects>>,
    per_client:  wl_server::utils::UnboundedAggregate,
    event_flags: wl_server::events::EventFlags,
    state:       Crescent,
    tasks:       RefCell<Vec<Task<()>>>,
    tx:          Rc<RefCell<BufWriterWithFd<wl_io::WriteWithFd>>>,
}

impl Drop for CrescentClient {
    fn drop(&mut self) {
        let empty_store = Default::default();
        let mut store = self.store.replace(empty_store);
        store.clear_for_disconnect(self)
    }
}

impl<T: std::any::Any + Default> connection::State<T> for CrescentClient {
    fn state(&self) -> Option<&T> {
        self.per_client.get::<T>()
    }

    fn state_mut(&mut self) -> &mut T {
        self.per_client.get_or_default::<T>()
    }
}

wl_server::event_multiplexer! {
    ctx: CrescentClient,
    error: wl_server::error::Error,
    receivers: [
        wl_server::globals::Registry,
        apollo::globals::Compositor,
        apollo::globals::xdg_shell::WmBase,
    ],
}
impl EventMux for CrescentClient {
    fn event_handle(&self) -> wl_server::events::EventHandle {
        self.event_flags.as_handle()
    }
}

impl connection::ClientContext for CrescentClient {
    type Context = Crescent;
    type Object = Objects;
    type Objects = Store<Self::Object>;

    type Flush<'a> = impl Future<Output = Result<(), std::io::Error>> + 'a;
    type Send<'a, M> = impl Future<Output = Result<(), std::io::Error>> + 'a
    where
        M: 'a + wl_io::traits::ser::Serialize + Unpin + std::fmt::Debug;

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

    fn spawn(&self, fut: impl Future<Output = ()> + 'static) {
        let task = self.state.0.executor.spawn(fut);
        self.tasks.borrow_mut().push(task);
    }
    wl_server::impl_dispatch!();
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
    let server = Crescent(Rc::new(CrescentState {
        globals:  RefCell::new(Globals::globals().collect()),
        shell:    Default::default(),
        executor: LocalExecutor::new(),
    }));
    let shell2 = server.0.shell.clone();
    tracing::debug!("Size: {:?}", window.inner_size());
    let _render = server.0.executor.spawn(async move {
        let renderer = render::Renderer::new(&window, window.inner_size(), shell2).await;
        renderer.render_loop(event_rx).await;
    });

    let (listener, _guard) = wl_server::wayland_listener_auto()?;
    let server2 = server.clone();
    let cm_task = server.0.executor.spawn(async move {
        let listener = smol::Async::new(listener)?;
        let incoming = Box::pin(
            listener
                .incoming()
                .and_then(|conn| future::ready(conn.into_inner())),
        );
        let mut cm = wl_server::ConnectionManager::new(incoming, server2);
        cm.run().await
    });
    let () = smol::block_on(server.0.executor.run(cm_task)).unwrap();
    Ok(())
}
