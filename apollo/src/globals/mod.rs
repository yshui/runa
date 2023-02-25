use std::{
    future::Future,
    pin::Pin,
    rc::{Rc, Weak},
    task::{ready, Poll},
};

pub mod xdg_shell;

use derivative::Derivative;
use futures_util::pin_mut;
use wl_protocol::wayland::{
    wl_compositor::v5 as wl_compositor, wl_output::v4 as wl_output, wl_shm::v1 as wl_shm,
    wl_subcompositor::v1 as wl_subcompositor, wl_surface::v5 as wl_surface,
};
use wl_server::{
    connection::{
        event_handler::Abortable,
        traits::{
            Client, ClientParts, EventDispatcher, EventHandler, EventHandlerAction, Store,
            WriteMessage,
        },
    },
    events::EventSource,
    globals::{Bind, GlobalMeta, MaybeConstInit},
    impl_global_for,
    objects::ObjectMeta,
    renderer_capability::RendererCapability,
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
            let objects = client.objects_mut();
            // Check if we already have a compositor bound, and clone their render event
            // handler abort handle if so; otherwise start the event handler task.
            let other_compositor = objects
                .by_type::<Self::Object>()
                .filter_map(|(_, obj)| obj.is_complete().then_some(obj)) // skip incomplete compositor objects
                .next()
                .cloned();
            let this = objects
                .get_mut(object_id)
                .unwrap()
                .cast_mut::<Self::Object>()
                .unwrap();
            if let Some(compositor) = other_compositor {
                *this = compositor;
            } else {
                *this = Self::Object::new();
                drop(objects);

                // Only start the event handler for the first completed compositor object bound.
                let rx = client.server_context().shell().borrow().subscribe();
                client
                    .event_dispatcher_mut()
                    .add_event_handler(rx, RenderEventHandler {
                        callbacks_to_fire: Vec::new(),
                    });
            };

            Ok(())
        }
    }
}

struct RenderEventHandler {
    callbacks_to_fire: Vec<u32>,
}

impl<S: Shell, Ctx: Client> EventHandler<Ctx> for RenderEventHandler
where
    Ctx::ServerContext: HasShell<Shell = S>,
{
    type Message = ShellEvent;

    fn poll_handle_event(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        objects: &mut <Ctx as Client>::ObjectStore,
        connection: &mut <Ctx as Client>::Connection,
        server_context: &<Ctx as Client>::ServerContext,
        message: &mut Self::Message,
    ) -> Poll<
        Result<EventHandlerAction, Box<dyn std::error::Error + std::marker::Send + Sync + 'static>>,
    > {
        assert!(matches!(message, ShellEvent::Render));
        let time = crate::time::elapsed().as_millis() as u32;
        if self.callbacks_to_fire.is_empty() {
            // First collect all callbacks we need to fire
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
                    self.callbacks_to_fire.extend(
                        surface.frame_callbacks().borrow_mut().drain(
                            ..(state.frame_callback_end - first_frame_callback_index) as usize,
                        ),
                    );
                    surface.set_first_frame_callback_index(state.frame_callback_end);
                }
            }
        }
        while let Some(&callback) = self.callbacks_to_fire.last() {
            ready!(wl_server::objects::Callback::poll_fire(
                cx,
                callback,
                time,
                objects,
                Pin::new(&mut *connection)
            ))?;
            self.callbacks_to_fire.pop();
        }

        // We can only ever return Ready iff callbacks_to_fire is empty.
        Poll::Ready(Ok(EventHandlerAction::Keep))
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
        let conn = client.connection_mut();
        async move {
            // Send known buffer formats
            for format in formats {
                conn.send(
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
pub struct Output {
    pub(crate) shell_output: Rc<ShellOutput>,
}

impl Output {
    pub fn new(output: Rc<ShellOutput>) -> Self {
        Self {
            shell_output: output,
        }
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
            output:              Rc::downgrade(&self.shell_output).into(),
            all_outputs:         None,
            event_handler_abort: None,
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
            let ClientParts {
                connection,
                objects,
                event_dispatcher,
                ..
            } = client.as_mut_parts();
            let output: WeakPtr<_> = Rc::downgrade(&self.shell_output).into();
            // Send properties of this output
            self.shell_output.send_all(connection, object_id).await?;
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
                connection.send(id, message).await.unwrap();
            }

            // If there is an output object already bound (of any shell output), we can get
            // the set of all bound outputs from it.
            let all_outputs = objects
                .by_type::<Self::Object>()
                .filter_map(|(id, o)| (id != object_id).then(|| o.all_outputs.clone().unwrap()))
                .next()
                .unwrap_or_default();

            // Start a new event handler for this shell output
            let rx = self.shell_output.subscribe();
            let handler = OutputChangeEventHandler { object_id };
            let (handler, abort) = Abortable::new(handler);
            let auto_abort = abort.auto_abort();
            event_dispatcher.add_event_handler(rx, handler);

            // Add this output to the set of all outputs
            all_outputs
                .borrow_mut()
                .entry(output)
                .or_default()
                .insert(object_id);
            let this = objects
                .get_mut(object_id)
                .unwrap()
                .cast_mut::<Self::Object>()
                .unwrap();
            // This will automatically stop the event handler when the output is destroyed
            this.event_handler_abort = Some(auto_abort);
            this.all_outputs = Some(all_outputs);

            Ok(())
        }
    }
}

use crate::shell::output::Output as ShellOutput;

struct OutputChangeEventHandler {
    object_id: u32,
}

impl<Ctx: Client> EventHandler<Ctx> for OutputChangeEventHandler {
    type Message = OutputChangeEvent;

    fn poll_handle_event(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        objects: &mut Ctx::ObjectStore,
        connection: &mut Ctx::Connection,
        server_context: &Ctx::ServerContext,
        message: &mut Self::Message,
    ) -> Poll<
        Result<EventHandlerAction, Box<dyn std::error::Error + std::marker::Send + Sync + 'static>>,
    > {
        // Send events for changes on this output
        // If the shell output object is gone, its event stream should have stopped and
        // we should not have received this event.
        let shell_output = Weak::upgrade(&message.output).unwrap();
        if message.change.contains(OutputChange::GEOMETRY) {
            let fut = shell_output.send_geometry(&mut *connection, self.object_id);
            pin_mut!(fut);
            ready!(fut.poll(cx))?;
            message.change.remove(OutputChange::GEOMETRY);
        }
        if message.change.contains(OutputChange::NAME) {
            let fut = shell_output.send_name(&mut *connection, self.object_id);
            pin_mut!(fut);
            ready!(fut.poll(cx))?;
            message.change.remove(OutputChange::NAME);
        }
        if message.change.contains(OutputChange::SCALE) {
            let fut = shell_output.send_scale(&mut *connection, self.object_id);
            pin_mut!(fut);
            ready!(fut.poll(cx))?;
            message.change.remove(OutputChange::SCALE);
        }
        let fut = ShellOutput::send_done(connection, self.object_id);
        pin_mut!(fut);
        ready!(fut.poll(cx))?;
        Poll::Ready(Ok(EventHandlerAction::Keep))
    }
}
