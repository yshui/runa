#![feature(impl_trait_in_assoc_type)]
use std::{
    cell::RefCell,
    os::{fd::OwnedFd, unix::net::UnixStream},
    pin::Pin,
    rc::Rc,
    sync::Arc,
};

use anyhow::Result;
use futures_util::{select, TryStreamExt};
use runa_core::{
    client::{
        traits::{Client, ClientParts, Store as _},
        EventDispatcher, Store,
    },
    events::broadcast::Broadcast,
    globals::Bind,
    objects::{Object, DISPLAY_ID},
    server::traits::GlobalStore,
};
use runa_io::{
    buf::BufReaderWithFd,
    traits::{ReadMessage, WriteMessage as _},
    Connection,
};
use runa_orbiter::{
    globals, objects,
    renderer_capability::RendererCapability,
    shell::{buffers::HasBuffer, HasShell, Keymap, RepeatInfo, SeatEvent},
    utils::geometry::{Extent, Point},
};
use runa_wayland_protocols::wayland::{wl_keyboard::v9 as wl_keyboard, wl_seat::v9 as wl_seat};
use smol::{block_on, LocalExecutor};
mod render;
mod shell;
use shell::DefaultShell;

#[derive(Debug)]
pub struct CrescentState {
    globals:     RefCell<runa_core::server::GlobalStore<AnyGlobal>>,
    shell:       Rc<RefCell<<Crescent as HasShell>::Shell>>,
    executor:    LocalExecutor<'static>,
    seat_events: Broadcast<SeatEvent>,
    keymap:      Keymap,
}

impl runa_orbiter::shell::Seat for Crescent {
    fn capabilities(&self) -> wl_seat::enums::Capability {
        use wl_seat::enums::Capability;
        Capability::POINTER | Capability::KEYBOARD
    }

    fn name(&self) -> &str {
        "crescent"
    }

    fn keymap(&self) -> &runa_orbiter::shell::Keymap {
        &self.0.keymap
    }

    fn repeat_info(&self) -> runa_orbiter::shell::RepeatInfo {
        RepeatInfo {
            rate:  25,
            delay: 600,
        }
    }
}

impl runa_core::events::EventSource<SeatEvent> for Crescent {
    type Source = <Broadcast<SeatEvent> as runa_core::events::EventSource<SeatEvent>>::Source;

    fn subscribe(&self) -> Self::Source {
        self.0.seat_events.subscribe()
    }
}

#[derive(Debug, Clone)]
pub struct Crescent(Rc<CrescentState>);

runa_core::globals! {
    type ClientContext = CrescentClient;
    pub enum AnyGlobal {
        // Display must be the first one
        Display(runa_core::globals::Display),
        Registry(runa_core::globals::Registry),
        Compositor(globals::Compositor),
        Output(globals::Output),
        Subcompositor(globals::Subcompositor),
        Shm(globals::Shm),
        WmBase(globals::xdg_shell::WmBase),
        Seat(globals::Seat),
    }
}

type Shell = <Crescent as HasShell>::Shell;
#[derive(Object, Debug)]
#[wayland(context = "CrescentClient")]
pub enum AnyObject {
    // === core objects ===
    Display(runa_core::objects::Display),
    Registry(runa_core::objects::Registry),
    Callback(runa_core::objects::Callback),

    // === output ===
    Output(objects::Output),

    // === compositor objects ===
    Compositor(objects::compositor::Compositor),
    Surface(objects::compositor::Surface<Shell>),
    Subcompositor(objects::compositor::Subcompositor),
    Subsurface(objects::compositor::Subsurface<Shell>),
    Region(objects::compositor::Region),

    // === seat ===
    Seat(objects::input::Seat),
    Pointer(objects::input::Pointer),
    Keyboard(objects::input::Keyboard),

    // === xdg_shell objects ===
    WmBase(objects::xdg_shell::WmBase),
    XdgSurface(objects::xdg_shell::Surface<Shell>),
    XdgTopLevel(objects::xdg_shell::TopLevel<Shell>),

    // === shm objects ===
    Shm(objects::shm::Shm),
    ShmPool(objects::shm::ShmPool),

    // === buffer ===
    Buffer(objects::Buffer<render::Buffer>),
}

impl runa_core::server::traits::Server for Crescent {
    type ClientContext = CrescentClient;
    type Conn = UnixStream;
    type Error = ();
    type Global = AnyGlobal;
    type GlobalStore = runa_core::server::GlobalStore<AnyGlobal>;

    fn globals(&self) -> &RefCell<Self::GlobalStore> {
        &self.0.globals
    }

    fn new_connection(&self, conn: UnixStream) -> Result<(), Self::Error> {
        tracing::debug!("New connection");
        let state = self.clone();
        self.0
            .executor
            .spawn(async move {
                let (rx, tx) = ::runa_io::split_unixstream(conn)?;
                let mut client_ctx = CrescentClient {
                    store: Default::default(),
                    state,
                    tx: Connection::new(tx, 256),
                    event_dispatcher: EventDispatcher::new(),
                };
                // Insert and bind the display object
                client_ctx
                    .objects_mut()
                    .insert(DISPLAY_ID, runa_core::objects::Display::default())
                    .unwrap();

                let display_global = client_ctx
                    .server_context()
                    .clone()
                    .globals()
                    .borrow()
                    .get(DISPLAY_ID)
                    .unwrap()
                    .clone();

                display_global.bind(&mut client_ctx, DISPLAY_ID).await?;
                let mut read = BufReaderWithFd::new(rx);
                let _span = tracing::debug_span!("main loop").entered();
                loop {
                    let CrescentClient {
                        store,
                        state,
                        tx,
                        event_dispatcher,
                        ..
                    } = &mut client_ctx;
                    // Flush output before we start waiting.
                    if let Err(e) = tx.flush().await {
                        if e.kind() != std::io::ErrorKind::BrokenPipe {
                            tracing::warn!("Error while flushing connection {e}");
                            break
                        }
                        // Broken pipe means the client disconnected, but there
                        // could still be more requests we can read.
                        while Pin::new(&mut read).next_message().await.is_ok() {
                            if client_ctx.dispatch(Pin::new(&mut read)).await {
                                break
                            }
                        }
                        break
                    }
                    use futures_util::FutureExt;
                    select! {
                        msg = Pin::new(&mut read).next_message().fuse() => {
                            match msg {
                                Ok(_) =>
                                    if client_ctx.dispatch(Pin::new(&mut read)).await {
                                        break
                                    },
                                Err(e) => {
                                    if e.kind() != std::io::ErrorKind::UnexpectedEof {
                                        tracing::warn!("Error while reading message: {e}");
                                    }
                                    break
                                },
                            }
                        },
                        event = event_dispatcher.next() => {
                            event.handle(store, tx, state).await?;
                        },
                    }
                    // Dispatch all pending events before we start handling new requests.
                    client_ctx
                        .event_dispatcher
                        .handle_queued_events(
                            &mut client_ctx.store,
                            &mut client_ctx.tx,
                            &client_ctx.state,
                        )
                        .await?;
                }
                // Try to flush the connection, ok if it fails, as the connection could have
                // been broken at this point.
                client_ctx.tx.flush().await.ok();
                client_ctx.disconnect().await;
                Ok::<(), runa_core::error::Error>(())
            })
            .detach();
        Ok(())
    }
}
impl HasBuffer for Crescent {
    type Buffer = render::Buffer;
}

impl HasShell for Crescent {
    type Shell = DefaultShell<<Self as HasBuffer>::Buffer>;

    fn shell(&self) -> &RefCell<Self::Shell> {
        &self.0.shell
    }
}

impl RendererCapability for Crescent {
    fn formats(&self) -> Vec<runa_orbiter::renderer_capability::Format> {
        use runa_orbiter::renderer_capability::Format;
        vec![Format::Argb8888]
    }
}

#[derive(Debug)]
pub struct CrescentClient {
    store:            Store<AnyObject>,
    state:            Crescent,
    tx:               Connection<runa_io::WriteWithFd>,
    event_dispatcher: EventDispatcher<Self>,
}

impl CrescentClient {
    /// Finalize the client context after the client has disconnected.
    async fn disconnect(&mut self) {
        let Self { store, state, .. } = self;
        store.clear_for_disconnect(state);
    }
}

impl Client for CrescentClient {
    type Connection = Connection<runa_io::WriteWithFd>;
    type EventDispatcher = EventDispatcher<Self>;
    type Object = AnyObject;
    type ObjectStore = Store<Self::Object>;
    type ServerContext = Crescent;

    type DispatchFut<'a, R> = impl std::future::Future<Output = bool> + 'a where R: ReadMessage + 'a;

    fn dispatch<'a, R>(&'a mut self, reader: Pin<&'a mut R>) -> Self::DispatchFut<'a, R>
    where
        R: ReadMessage,
    {
        runa_core::client::dispatch_to(self, reader)
    }

    fn server_context(&self) -> &Self::ServerContext {
        &self.state
    }

    fn objects(&self) -> &Self::ObjectStore {
        &self.store
    }

    fn as_mut_parts(&mut self) -> ClientParts<'_, Self> {
        ClientParts::new(
            &self.state,
            &mut self.store,
            &mut self.tx,
            &mut self.event_dispatcher,
        )
    }
}

fn from_bytes_until_nul(bytes: &[u8]) -> Result<(&str, &[u8])> {
    let nul = bytes
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| anyhow::anyhow!("No nul byte found"))?;
    let (head, rest) = bytes.split_at(nul);
    Ok((
        std::str::from_utf8(head).map_err(|_| anyhow::anyhow!("Invalid utf8"))?,
        &rest[1..],
    ))
}

fn get_keymap_from_xserver() -> Result<xkbcommon::xkb::Keymap> {
    use x11rb::{atom_manager, connection::Connection, protocol::xproto::ConnectionExt};
    atom_manager! {
        pub Atoms:
        AtomsCookie {
            _XKB_RULES_NAMES,
            STRING,
        }
    }
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let atoms = Atoms::new(&conn)?.reply()?;
    let props = conn
        .get_property(false, root, atoms._XKB_RULES_NAMES, atoms.STRING, 0, 0)?
        .reply()?;
    if props.format != 8 {
        return Err(anyhow::anyhow!(
            "Invalid format for __XKB_RULES_NAMES: {}",
            props.format
        ))
    }
    let props = conn
        .get_property(
            false,
            root,
            atoms._XKB_RULES_NAMES,
            atoms.STRING,
            0,
            (props.bytes_after + 3) / 4,
        )?
        .reply()?;

    let (rules, rest) = from_bytes_until_nul(&props.value)?;
    let (model, rest) = from_bytes_until_nul(rest)?;
    let (layout, rest) = from_bytes_until_nul(rest)?;
    let (variant, rest) = from_bytes_until_nul(rest)?;
    let (options, _) = from_bytes_until_nul(rest)?;
    tracing::debug!(
        "Got keymap from X server: rules={rules}, model={model}, layout={layout}, \
         variant={variant}, options={options}",
    );
    xkbcommon::xkb::Keymap::new_from_names(
        &xkbcommon::xkb::Context::new(0),
        rules,
        model,
        layout,
        variant,
        Some(options.to_owned()),
        0,
    )
    .ok_or_else(|| anyhow::anyhow!("Failed to create keymap"))
}

fn get_keymap_as_fd(keymap: &xkbcommon::xkb::Keymap) -> Result<(OwnedFd, u64)> {
    use std::io::Write;
    let fd = memfd::MemfdOptions::new()
        .allow_sealing(true)
        .create("crescent-keymap")?;
    let mut file = fd.into_file();
    write!(
        file,
        "{}",
        keymap.get_as_string(xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1)
    )?;
    file.flush()?;

    let fd = memfd::Memfd::try_from_file(file)
        .map_err(|_| anyhow::anyhow!("Failed to convert file to memfd"))?;
    fd.add_seals(&[
        memfd::FileSeal::SealShrink,
        memfd::FileSeal::SealGrow,
        memfd::FileSeal::SealFutureWrite, /* SealWrite has this problem on Linux where the
                                           * recipient can't create a shared, read-only mapping
                                           * of this file. this could break some clients */
        memfd::FileSeal::SealSeal,
    ])?;
    let size = fd.as_file().metadata()?.len();
    Ok((fd.into_file().into(), size))
}

fn main() -> Result<()> {
    use futures_util::future;
    tracing_subscriber::fmt::init();
    let keymap = get_keymap_from_xserver()?;
    let (keymap_fd, keymap_size) = get_keymap_as_fd(&keymap)?;

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let (event_tx, event_rx) = smol::channel::unbounded();
    let _el = std::thread::spawn(move || {
        use winit::platform::x11::EventLoopBuilderExtX11;
        let el = winit::event_loop::EventLoopBuilder::new()
            .with_any_thread(true)
            .build()
            .unwrap();
        let window = Arc::new(
            winit::window::WindowBuilder::new()
                .with_title("Crescent")
                .with_maximized(true)
                .build(&el)
                .unwrap(),
        );
        tx.send(window).unwrap();
        let event_tx = event_tx;
        el.run(move |event, el| {
            el.set_control_flow(winit::event_loop::ControlFlow::Wait);
            smol::block_on(event_tx.send(event)).unwrap();
        })
    });
    let output_global_id = AnyGlobal::globals().count() as u32 + 1;
    let output = runa_orbiter::shell::output::Output::new(
        Extent::new(0, 0),
        "Crescent",
        "Crescent",
        "virtual-output-0",
        output_global_id,
    );
    output.set_size(Extent::new(1920, 1080));
    output.set_position(Point::new(0, 0));
    output.set_logical_position(Point::new(0, 0));
    output.set_scale(3 * 120);
    let output = output.into();
    let window = rx.recv().unwrap();
    let server = Crescent(Rc::new(CrescentState {
        globals:     RefCell::new(AnyGlobal::globals().collect()),
        shell:       Rc::new(RefCell::new(DefaultShell::new(&output, &keymap))),
        executor:    LocalExecutor::new(),
        seat_events: Default::default(),
        keymap:      Keymap {
            format: wl_keyboard::enums::KeymapFormat::XkbV1,
            fd:     keymap_fd,
            size:   keymap_size as u32,
        },
    }));
    // Add output global and make sure its id is what we expect.
    tracing::debug!("globals {:?}", server.0.globals.borrow());
    assert_eq!(
        block_on(
            server
                .0
                .globals
                .borrow_mut()
                .insert(runa_orbiter::globals::Output::new(output))
        ),
        output_global_id
    );
    let shell2 = server.0.shell.clone();
    tracing::debug!("Starting renderer");
    let renderer = smol::block_on(render::Renderer::new(
        window.clone(),
        window.inner_size(),
        shell2,
    ));
    tracing::debug!("Size: {:?}", window.inner_size());
    let _render = server.0.executor.spawn(async move {
        tracing::debug!("Starting render loop");
        renderer.render_loop(event_rx).await;
    });

    let (listener, _guard) = runa_core::wayland_listener_auto()?;
    let server2 = server.clone();
    let cm_task = server.0.executor.spawn(async move {
        tracing::debug!("Starting connection manager");
        let listener = smol::Async::new(listener)?;
        let incoming = Box::pin(
            listener
                .incoming()
                .and_then(|conn| future::ready(conn.into_inner())),
        );
        let mut cm = runa_core::ConnectionManager::new(incoming, server2);
        cm.run().await
    });
    let () = smol::block_on(server.0.executor.run(cm_task)).unwrap();
    Ok(())
}
