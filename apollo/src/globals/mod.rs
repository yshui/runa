use std::{
    collections::VecDeque,
    future::Future,
    rc::{Rc, Weak},
};

pub mod xdg_shell;

use derivative::Derivative;
use hashbrown::{HashMap, HashSet};
use wl_protocol::wayland::{
    wl_compositor::v5 as wl_compositor, wl_output::v4 as wl_output, wl_shm::v1 as wl_shm,
    wl_subcompositor::v1 as wl_subcompositor, wl_surface::v5 as wl_surface,
};
use wl_server::{
    connection::{Client, State},
    events::{DispatchTo, EventHandler},
    globals::{Bind, MaybeConstInit},
    impl_global_for,
    objects::Object,
    renderer_capability::RendererCapability,
};

use crate::{
    shell::{output::OutputChange, surface::OutputSet, HasShell, Shell, ShellOf},
    utils::{Double, WeakPtr},
};

#[derive(Derivative)]
#[derivative(Default(bound = ""), Debug(bound = ""))]
pub struct Compositor;
impl_global_for!(Compositor);

impl MaybeConstInit for Compositor {
    const INIT: Option<Self> = Some(Self);
}
impl<Ctx: Client> Bind<Ctx> for Compositor
where
    Ctx:
        State<OutputAndCompositorState<<Ctx::ServerContext as HasShell>::Shell>> + DispatchTo<Self>,
    Ctx::ServerContext: HasShell,
    Ctx::Object: From<crate::objects::compositor::Compositor>,
{
    type BindFut<'a> = impl Future<Output = std::io::Result<Ctx::Object>> + 'a;

    fn interface(&self) -> &'static str {
        wl_compositor::NAME
    }

    fn version(&self) -> u32 {
        wl_compositor::VERSION
    }

    fn bind<'a>(&'a self, client: &'a mut Ctx, _object_id: u32) -> Self::BindFut<'a> {
        client
            .server_context()
            .shell()
            .borrow()
            .add_render_listener((client.event_handle(), Ctx::SLOT));
        futures_util::future::ok(crate::objects::compositor::Compositor.into())
    }
}

impl<Ctx> EventHandler<Ctx> for Compositor
where
    Ctx: DispatchTo<Self>
        + State<OutputAndCompositorState<<Ctx::ServerContext as HasShell>::Shell>>
        + Client,
    Ctx::ServerContext: HasShell,
{
    type Error = std::io::Error;

    type Fut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a
        where
            Ctx: 'a;

    #[tracing::instrument(level = "debug", skip_all)]
    fn invoke(ctx: &mut Ctx) -> Self::Fut<'_> {
        use wl_server::connection::Objects;

        use crate::objects::compositor::Surface as SurfaceObject;
        async move {
            // Use a tmp buffer so we don't need to hold RefMut of `current` acorss await.
            let mut tmp_frame_callback_buffer =
                ctx.state_mut().tmp_frame_callback_buffer.take().unwrap();
            let (ro_ctx, state) = ctx.state();
            let surfaces = &state.surfaces;
            let time = crate::time::elapsed().as_millis() as u32;
            for (surface, _) in surfaces {
                let surface = ro_ctx.objects().borrow().get(*surface).unwrap().clone(); // don't hold
                                                                                        // the Ref
                let surface: &SurfaceObject<ShellOf<Ctx::ServerContext>> = surface.cast().unwrap();
                {
                    let mut shell = ro_ctx.server_context().shell().borrow_mut();
                    let current = surface.0.current_mut(&mut shell);
                    std::mem::swap(&mut current.frame_callback, &mut tmp_frame_callback_buffer);
                }
                let fired = tmp_frame_callback_buffer.len();
                for frame in tmp_frame_callback_buffer.drain(..) {
                    wl_server::objects::Callback::fire(frame, time, ro_ctx).await?;
                }
                // Remove fired callbacks from pending state
                let mut shell = ro_ctx.server_context().shell().borrow_mut();
                let pending = surface.0.pending_mut(&mut shell);
                for _ in 0..fired {
                    pending.frame_callback.pop_front();
                }
            }

            let state = ctx.state_mut();
            // Put the buffer back so we can reuse it next time.
            state.tmp_frame_callback_buffer = Some(tmp_frame_callback_buffer);
            let output_changed = state.output_changed.read_exclusive().unwrap();
            let mut output_changed_buffer = state.output_changed_buffer.take().unwrap();

            for (surface_object_id, new_outputs) in output_changed.as_ref() {
                tracing::debug!("output changed for surface object {surface_object_id}");
                let old_outputs = state.surfaces.get(surface_object_id).unwrap();
                let new_outputs = new_outputs.borrow();
                // Calculate symmetric difference of old and new outputs.
                for output in &*new_outputs {
                    if old_outputs.contains(output) {
                        continue
                    }
                    if let Some(output_object_ids) = state.output_bindings.get(output) {
                        for output_object_id in output_object_ids {
                            output_changed_buffer.push((
                                *output_object_id,
                                *surface_object_id,
                                true,
                            ));
                        }
                    }
                }
                for output in old_outputs {
                    if new_outputs.contains(output) {
                        continue
                    }
                    if let Some(output_object_ids) = state.output_bindings.get(output) {
                        for output_object_id in output_object_ids {
                            output_changed_buffer.push((
                                *output_object_id,
                                *surface_object_id,
                                false,
                            ));
                        }
                    }
                }

                // Update the recorded output set.
                let outputs_mut = state.surfaces.get_mut(surface_object_id).unwrap();
                outputs_mut.retain(|output| new_outputs.contains(output));
                for output in &*new_outputs {
                    outputs_mut.insert(output.clone());
                }
            }

            // Send enter/leave events.
            for (output_object_id, surface_object_id, added) in output_changed_buffer.drain(..) {
                use wl_server::connection::WriteMessage;
                ctx.connection()
                    .send(
                        surface_object_id,
                        if added {
                            wl_surface::Event::Enter(wl_surface::events::Enter {
                                output: wl_types::Object(output_object_id),
                            })
                        } else {
                            wl_surface::Event::Leave(wl_surface::events::Leave {
                                output: wl_types::Object(output_object_id),
                            })
                        },
                    )
                    .await?;
            }

            // Put the buffer back so we can reuse it next time.
            let state = ctx.state_mut();
            state.output_changed_buffer = Some(output_changed_buffer);
            Ok(())
        }
    }
}

#[derive(Debug)]
pub struct Subcompositor;
impl_global_for!(Subcompositor);

impl MaybeConstInit for Subcompositor {
    const INIT: Option<Self> = Some(Self);
}
impl<Ctx: Client> Bind<Ctx> for Subcompositor
where
    Ctx::ServerContext: HasShell,
    Ctx::Object: From<crate::objects::compositor::Subcompositor>,
{
    type BindFut<'a> = impl Future<Output = std::io::Result<Ctx::Object>> + 'a;

    fn interface(&self) -> &'static str {
        wl_subcompositor::NAME
    }

    fn version(&self) -> u32 {
        wl_subcompositor::VERSION
    }

    fn bind<'a>(&'a self, _client: &'a mut Ctx, _object_id: u32) -> Self::BindFut<'a> {
        futures_util::future::ok(crate::objects::compositor::Subcompositor.into())
    }
}

#[derive(Default)]
pub struct Shm;
impl_global_for!(Shm);

impl MaybeConstInit for Shm {
    const INIT: Option<Self> = Some(Self);
}
impl<Ctx: Client> Bind<Ctx> for Shm
where
    Ctx::ServerContext: RendererCapability,
    Ctx::Object: From<crate::objects::shm::Shm>,
{
    type BindFut<'a> = impl Future<Output = std::io::Result<Ctx::Object>> + 'a;

    fn interface(&self) -> &'static str {
        wl_shm::NAME
    }

    fn version(&self) -> u32 {
        wl_shm::VERSION
    }

    fn bind<'a>(&'a self, client: &'a mut Ctx, object_id: u32) -> Self::BindFut<'a> {
        let formats = client.server_context().formats();
        async move {
            use wl_server::connection::WriteMessage;
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
            Ok(crate::objects::shm::Shm.into())
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

impl<Ctx> Bind<Ctx> for Output
where
    Ctx::Object: From<crate::objects::Output>,
    Ctx::ServerContext: HasShell,
    Ctx: Client
        + DispatchTo<Self>
        + State<OutputAndCompositorState<<Ctx::ServerContext as HasShell>::Shell>>,
{
    type BindFut<'a> = impl Future<Output = std::io::Result<<Ctx>::Object>> + 'a
        where
            Ctx: Client + 'a,
            Self: 'a;

    fn interface(&self) -> &'static str {
        wl_output::NAME
    }

    fn version(&self) -> u32 {
        wl_output::VERSION
    }

    fn bind<'a>(&'a self, client: &'a mut Ctx, object_id: u32) -> Self::BindFut<'a>
    where
        Ctx: Client,
    {
        let slot = <Ctx as DispatchTo<Self>>::SLOT;
        let handle = (client.event_handle(), slot);
        let state = client.state_mut();
        // Add this binding to the new bindings list, so initial events will be sent for
        // it
        state
            .new_bindings
            .push((object_id, Rc::downgrade(&self.0).into()));
        // Invoke the event handler to send events for the new binding
        handle.0.set(slot);
        // Listen for output changes
        self.0
            .add_change_listener(handle, state.changed_outputs.write_end());
        futures_util::future::ok(crate::objects::Output(Rc::downgrade(&self.0)).into())
    }
}

use crate::shell::output::Output as ShellOutput;
#[derive(Derivative)]
#[derivative(Default)]
pub struct OutputAndCompositorState<S: crate::shell::Shell> {
    changed_outputs:                  Double<HashMap<WeakPtr<ShellOutput>, OutputChange>>,
    /// Map from output's global id to the output object ids in client's
    /// context.
    pub(crate) output_bindings:       HashMap<WeakPtr<ShellOutput>, HashSet<u32>>,
    /// New bindings of outputs, to which we haven't sent the initial events.
    /// A pair of (object_id, weak ref to the output)
    pub(crate) new_bindings:          Vec<(u32, WeakPtr<ShellOutput>)>,
    /// Map of surface ids to outputs they are currently on
    pub(crate) surfaces:              HashMap<u32, HashSet<WeakPtr<ShellOutput>>>,
    /// List of surfaces that have changed output
    pub(crate) output_changed:        Double<HashMap<u32, OutputSet>>,
    /// A buffer to store add/removed outputs, a tuple of
    /// (output object id, surface object id, whether it was added)
    #[derivative(Default(value = "Some(Default::default())"))]
    output_changed_buffer:            Option<Vec<(u32, u32, bool)>>,
    #[derivative(Default(value = "Some(Default::default())"))]
    buffer:                           Option<HashSet<u32>>,
    /// A buffer for holding fram callback list, so we don't need to hold borrow
    /// or surface states across awaits.
    #[derivative(Default(value = "Some(Default::default())"))]
    tmp_frame_callback_buffer:        Option<VecDeque<u32>>,
    pub(crate) commit_scratch_buffer: Vec<S::Token>,
}

impl<Ctx> EventHandler<Ctx> for Output
where
    Ctx::ServerContext: HasShell,
    Ctx: DispatchTo<Output>
        + State<OutputAndCompositorState<<Ctx::ServerContext as HasShell>::Shell>>
        + Client,
{
    type Error = std::io::Error;

    type Fut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a
        where
            Ctx: 'a;

    #[tracing::instrument(skip(ctx))]
    fn invoke(ctx: &mut Ctx) -> Self::Fut<'_> {
        tracing::trace!("Output event handler invoked");
        async move {
            // Send events for changed outputs
            let changed_outputs = ctx.state_mut().changed_outputs.read_exclusive().unwrap();
            let (ro_ctx, state) = ctx.state();
            for (weak_output, change) in changed_outputs.iter() {
                let Some(output_global) = weak_output.upgrade() else { continue };
                for output_object_id in state.output_bindings.get(weak_output).unwrap() {
                    if change.contains(OutputChange::GEOMETRY) {
                        output_global
                            .send_geometry(ro_ctx, *output_object_id)
                            .await?;
                    }
                    if change.contains(OutputChange::NAME) {
                        output_global.send_name(ro_ctx, *output_object_id).await?;
                    }
                    if change.contains(OutputChange::SCALE) {
                        output_global.send_scale(ro_ctx, *output_object_id).await?;
                    }
                    ShellOutput::send_done(ro_ctx, *output_object_id).await?;
                }
            }

            // Send initial events for the new bindings
            for (object_id, weak_output) in &state.new_bindings {
                let Some(output_global) = weak_output.upgrade() else { continue };
                output_global.send_all(ro_ctx, *object_id).await?;
                // Send enter for surfaces already on the output
                for (surface_id, surface_outputs) in &state.surfaces {
                    use wl_server::connection::WriteMessage;
                    if surface_outputs.contains(weak_output) {
                        ro_ctx
                            .connection()
                            .send(*surface_id, wl_surface::events::Enter {
                                output: wl_types::Object(*object_id),
                            })
                            .await?;
                    }
                }
            }

            // Insert the new bindings into the output_bindings map
            let state = ctx.state_mut();
            for (object_id, weak_output) in state.new_bindings.drain(..) {
                state
                    .output_bindings
                    .entry(weak_output)
                    .or_default()
                    .insert(object_id);
            }
            Ok(())
        }
    }
}
