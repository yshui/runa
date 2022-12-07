use std::{future::Future, rc::Rc};

pub mod xdg_shell;

use derivative::Derivative;
use hashbrown::{HashMap, HashSet};
use wl_protocol::wayland::{
    wl_compositor::v5 as wl_compositor, wl_output::v4 as wl_output, wl_shm::v1 as wl_shm,
    wl_subcompositor::v1 as wl_subcompositor,
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
    objects::compositor::CompositorState,
    shell::{output::OutputChange, HasShell, Shell, ShellOf},
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
    Ctx: State<CompositorState> + DispatchTo<Self>,
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
    Ctx: DispatchTo<Self> + State<CompositorState> + Client,
    Ctx::ServerContext: HasShell,
{
    type Error = std::io::Error;

    type Fut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a
        where
            Ctx: 'a;

    fn invoke(ctx: &mut Ctx) -> Self::Fut<'_> {
        use wl_server::connection::Objects;
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
                let surface: &crate::objects::compositor::Surface<ShellOf<Ctx::ServerContext>> =
                    surface.cast().unwrap();
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
            //let output_buffer = state.output_buffer.take().unwrap();
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
            // Send known buffer formats
            for format in formats {
                client
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
impl_global_for!(Output);
impl MaybeConstInit for Output {
    const INIT: Option<Self> = None;
}

impl<Ctx: Client + DispatchTo<Self> + State<OutputState>> Bind<Ctx> for Output
where
    Ctx::Object: From<crate::objects::Output>,
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
        let handle = (client.event_handle(), <Ctx as DispatchTo<Self>>::SLOT);
        let state = client.state_mut();
        // Add this binding to the new bindings list, so initial events will be sent for
        // it
        state
            .new_bindings
            .push((object_id, Rc::downgrade(&self.0).into()));
        // Listen for output changes
        self.0
            .add_change_listener(handle, state.changed_outputs.write_end());
        futures_util::future::ok(crate::objects::Output(Rc::downgrade(&self.0)).into())
    }
}

use crate::shell::output::Output as ShellOutput;
#[derive(Default)]
pub struct OutputState {
    changed_outputs: Double<HashMap<WeakPtr<ShellOutput>, OutputChange>>,
    /// Map from output's global id to the output object ids in client's
    /// context.
    output_bindings: HashMap<WeakPtr<ShellOutput>, HashSet<u32>>,
    /// New bindings of outputs, to which we haven't sent the initial events.
    /// A pair of (object_id, weak ref to the output)
    new_bindings:    Vec<(u32, WeakPtr<ShellOutput>)>,
}

impl<Ctx> EventHandler<Ctx> for Output
where
    Ctx: DispatchTo<Output> + State<OutputState> + Client,
{
    type Error = std::io::Error;

    type Fut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a
        where
            Ctx: 'a;

    fn invoke(ctx: &mut Ctx) -> Self::Fut<'_> {
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
