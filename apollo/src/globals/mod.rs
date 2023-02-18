use std::{
    future::Future,
    rc::{Rc, Weak},
};

pub mod xdg_shell;

use derivative::Derivative;
use futures_util::{pin_mut, Stream};
use hashbrown::HashSet;
use wl_protocol::wayland::{
    wl_compositor::v5 as wl_compositor, wl_output::v4 as wl_output, wl_shm::v1 as wl_shm,
    wl_subcompositor::v1 as wl_subcompositor, wl_surface::v5 as wl_surface,
};
use wl_server::{
    connection::{
        traits::{LockableStore, Store, StoreExt},
        Client, WriteMessage,
    },
    events::EventSource,
    globals::{Bind, GlobalMeta, MaybeConstInit},
    impl_global_for,
    objects::ObjectMeta,
    renderer_capability::RendererCapability,
    server::Server,
};

use crate::{
    shell::{
        output::{OutputChange, OutputChangeEvent},
        HasShell, Shell, ShellEvent,
    },
    utils::WeakPtr,
};

#[derive(Derivative)]
#[derivative(Default(bound = ""), Debug(bound = ""))]
pub struct Compositor;
impl_global_for!(Compositor);

impl MaybeConstInit for Compositor {
    const INIT: Option<Self> = Some(Self);
}
impl GlobalMeta for Compositor {
    type Object = crate::objects::compositor::Compositor;

    fn interface(&self) -> &'static str {
        wl_compositor::NAME
    }

    fn version(&self) -> u32 {
        wl_compositor::VERSION
    }

    fn new_object(&self) -> Self::Object {
        crate::objects::compositor::Compositor::default()
    }
}

impl<Ctx: Client> Bind<Ctx> for Compositor
where
    Ctx::ServerContext: HasShell,
{
    type BindFut<'a> = impl Future<Output = std::io::Result<()>> + 'a where Ctx: 'a, Self: 'a;

    fn bind<'a>(&'a self, client: &'a mut Ctx, object_id: u32) -> Self::BindFut<'a> {
        async move {
            let mut objects = client.objects().lock().await;
            // Check if we already have a compositor bound, and clone their render event
            // handler abort handle if so; otherwise start the event handler task.
            let (fut, inner) = if let Some(compositor_inner) = objects
                .by_type::<Self::Object>()
                .filter_map(|(_, obj)| obj.is_complete().then_some(obj)) // skip incomplete compositor objects
                .next()
                .cloned()
            {
                (None, compositor_inner)
            } else {
                let rx = client.server_context().shell().borrow().subscribe();
                let (objects, server, conn) = (
                    client.objects().clone(),
                    client.server_context().clone(),
                    client.connection().clone(),
                );
                let (fut, handle) = futures_util::future::abortable(async move {
                    handle_render_event(objects, server, conn, rx).await;
                });
                (Some(fut), Self::Object::new(handle))
            };
            let this = objects
                .get_mut(object_id)
                .unwrap()
                .cast_mut::<Self::Object>()
                .unwrap();
            *this = inner;
            if let Some(fut) = fut {
                use futures_util::FutureExt;
                client.spawn(fut.map(|_| ()));
            }
            Ok(())
        }
    }
}

async fn handle_render_event<S: Shell, Obj: ObjectMeta + 'static>(
    objects: impl LockableStore<Obj>,
    server_context: impl Server + HasShell<Shell = S>,
    conn: impl WriteMessage,
    rx: impl Stream<Item = ShellEvent>,
) {
    use futures_util::StreamExt;
    let mut callbacks_to_fire: HashSet<u32> = HashSet::new();
    pin_mut!(rx);
    while let Some(event) = rx.next().await {
        assert!(matches!(event, ShellEvent::Render));
        let time = crate::time::elapsed().as_millis() as u32;
        let mut objects = objects.lock().await;
        {
            let shell = server_context.shell().borrow();
            // Send frame callback for all current surface states.
            for (id, surface) in objects.by_type::<crate::objects::compositor::Surface<S>>() {
                // Skip subsurfaces. Only iterate surface trees from the root surface, so we
                // only gets the surface states that are current.
                // TODO: handle desync'd subsurfaces
                let role = surface
                    .inner
                    .role::<crate::shell::surface::roles::Subsurface<S>>();
                if role.is_some() {
                    continue
                }
                for (key, _) in crate::shell::surface::roles::subsurface_iter(
                    surface.inner.current_key(),
                    &*shell,
                ) {
                    let state = shell.get(key);
                    let surface = state.surface().upgrade().unwrap();
                    let first_frame_callback_index = surface.first_frame_callback_index();
                    if state.frame_callback_end == first_frame_callback_index {
                        continue
                    }

                    tracing::debug!("Firing frame callback for surface {}", surface.object_id());
                    tracing::debug!(
                        "frame_callback_end: {}, surface.first_frame_callback_index: {}",
                        state.frame_callback_end,
                        first_frame_callback_index
                    );
                    callbacks_to_fire.extend(
                        surface.frame_callbacks().borrow_mut().drain(
                            ..(state.frame_callback_end - first_frame_callback_index) as usize,
                        ),
                    );
                    surface.set_first_frame_callback_index(state.frame_callback_end);
                }
            }
        }
        for &callback in &callbacks_to_fire {
            use std::ops::DerefMut;
            wl_server::objects::Callback::fire(callback, time, objects.deref_mut(), &conn)
                .await
                .unwrap();
        }
        conn.flush().await.unwrap();
        callbacks_to_fire.clear();
    }
}

#[derive(Debug)]
pub struct Subcompositor;
impl_global_for!(Subcompositor);

impl MaybeConstInit for Subcompositor {
    const INIT: Option<Self> = Some(Self);
}
impl GlobalMeta for Subcompositor {
    type Object = crate::objects::compositor::Subcompositor;

    fn interface(&self) -> &'static str {
        wl_subcompositor::NAME
    }

    fn new_object(&self) -> Self::Object {
        crate::objects::compositor::Subcompositor
    }

    fn version(&self) -> u32 {
        wl_subcompositor::VERSION
    }
}

impl<Ctx> Bind<Ctx> for Subcompositor {
    type BindFut<'a> = impl Future<Output = std::io::Result<()>> + 'a where Ctx: 'a, Self: 'a;

    fn bind<'a>(&'a self, _client: &'a mut Ctx, _object_id: u32) -> Self::BindFut<'a> {
        futures_util::future::ok(())
    }
}

#[derive(Default)]
pub struct Shm;
impl_global_for!(Shm);

impl MaybeConstInit for Shm {
    const INIT: Option<Self> = Some(Self);
}
impl GlobalMeta for Shm {
    type Object = crate::objects::shm::Shm;

    fn interface(&self) -> &'static str {
        wl_shm::NAME
    }

    fn version(&self) -> u32 {
        wl_shm::VERSION
    }

    fn new_object(&self) -> Self::Object {
        crate::objects::shm::Shm
    }
}

impl<Ctx: Client> Bind<Ctx> for Shm
where
    Ctx::ServerContext: RendererCapability,
{
    type BindFut<'a> = impl Future<Output = std::io::Result<()>> + 'a where Ctx: 'a, Self: 'a;

    fn bind<'a>(&'a self, client: &'a mut Ctx, object_id: u32) -> Self::BindFut<'a> {
        let formats = client.server_context().formats();
        async move {
            // Send known buffer formats
            for format in formats {
                client
                    .connection()
                    .send(
                        object_id,
                        wl_shm::Event::Format(wl_shm::events::Format { format }),
                    )
                    .await?;
            }
            Ok(())
        }
    }
}

#[derive(Debug)]
pub struct Output(pub(crate) Rc<ShellOutput>);
impl Output {
    pub fn new(output: Rc<ShellOutput>) -> Self {
        Self(output)
    }
}
impl_global_for!(Output);
impl MaybeConstInit for Output {
    const INIT: Option<Self> = None;
}

impl GlobalMeta for Output {
    type Object = crate::objects::Output;

    fn new_object(&self) -> Self::Object {
        crate::objects::Output {
            output:     None,
            event_task: None,
        }
    }

    fn interface(&self) -> &'static str {
        wl_output::NAME
    }

    fn version(&self) -> u32 {
        wl_output::VERSION
    }
}

impl<Ctx: Client> Bind<Ctx> for Output
where
    Ctx::ServerContext: HasShell,
{
    type BindFut<'a> = impl Future<Output = std::io::Result<()>> + 'a
        where
            Ctx: 'a, Self: 'a;

    fn bind<'a>(&'a self, client: &'a mut Ctx, object_id: u32) -> Self::BindFut<'a> {
        async move {
            use futures_util::FutureExt;
            let rx = self.0.subscribe();
            let objects = client.objects();
            let mut objects = objects.lock().await;
            let output: WeakPtr<_> = Rc::downgrade(&self.0).into();
            // Send properties of this output
            self.0.send_all(client.connection(), object_id).await?;
            // Send enter events for surfaces already on this output
            let messages: Vec<_> = {
                let surfaces =
                    objects.by_type::<crate::objects::compositor::Surface<
                        <Ctx::ServerContext as HasShell>::Shell,
                    >>();
                surfaces
                    .filter_map(|(id, surface)| {
                        surface.inner.outputs().contains(&output).then_some((
                            id,
                            wl_surface::events::Enter {
                                output: wl_types::Object(object_id),
                            },
                        ))
                    })
                    .collect()
            };
            for (id, message) in messages {
                client.connection().send(id, message).await.unwrap();
            }
            // Sent all the enter events before finailzing the object, this is to
            // avoid race conditions. For example, if we send the enter events
            // _after_ we finalized the object creation, surface event handler
            // could receive surface output change events and sent more
            // up-to-date enter events before we do, and after that we would send
            // the outdated enter events, and confuse the client.
            let this = objects
                .get_mut(object_id)
                .unwrap()
                .cast_mut::<Self::Object>()
                .unwrap();
            let output2 = output.clone();
            let connection = client.connection().clone();
            let (fut, abort) = futures_util::future::abortable(async move {
                handle_output_change_event(object_id, output2, connection, rx).await
            });
            this.event_task = Some(abort);
            this.output = Some(output.clone());
            client.spawn(fut.map(|_| ()));

            Ok(())
        }
    }
}

use crate::shell::output::Output as ShellOutput;

async fn handle_output_change_event(
    object_id: u32,
    output: WeakPtr<crate::shell::output::Output>,
    connection: impl WriteMessage,
    mut rx: impl futures_util::Stream<Item = OutputChangeEvent> + Unpin,
) {
    use futures_util::StreamExt;
    // Send events for changes on this output
    while let Some(event) = rx.next().await {
        let Some(output_global) = Weak::upgrade(&event.output) else { continue };
        if event.change.contains(OutputChange::GEOMETRY) {
            output_global
                .send_geometry(&connection, object_id)
                .await
                .unwrap();
        }
        if event.change.contains(OutputChange::NAME) {
            output_global
                .send_name(&connection, object_id)
                .await
                .unwrap();
        }
        if event.change.contains(OutputChange::SCALE) {
            output_global
                .send_scale(&connection, object_id)
                .await
                .unwrap();
        }
        ShellOutput::send_done(&connection, object_id)
            .await
            .unwrap();
        connection.flush().await.unwrap();
    }
}
