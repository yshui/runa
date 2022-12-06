use std::future::Future;

pub mod xdg_shell;

use derivative::Derivative;
use wl_protocol::wayland::{
    wl_compositor::v5 as wl_compositor, wl_shm::v1 as wl_shm,
    wl_subcompositor::v1 as wl_subcompositor,
};
use wl_server::{
    connection::{Client, State},
    events::{DispatchTo, EventHandler},
    globals::{Bind, MaybeConstInit, Global},
    objects::Object,
    renderer_capability::RendererCapability, server::{Server, Globals}, impl_global_for,
};

use crate::{
    objects::compositor::CompositorState,
    shell::{HasShell, Shell, ShellOf},
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
            let state = ctx.state();
            let empty = Default::default();
            let surfaces = state.map(|s| &s.surfaces).unwrap_or(&empty);
            let time = crate::time::elapsed().as_millis() as u32;
            for (surface, _) in surfaces {
                let surface = ctx.objects().borrow().get(*surface).unwrap().clone(); // don't hold
                                                                                     // the Ref
                let surface: &crate::objects::compositor::Surface<ShellOf<Ctx::ServerContext>> =
                    surface.cast().unwrap();
                {
                    let mut shell = ctx.server_context().shell().borrow_mut();
                    let current = surface.0.current_mut(&mut shell);
                    std::mem::swap(&mut current.frame_callback, &mut tmp_frame_callback_buffer);
                }
                let fired = tmp_frame_callback_buffer.len();
                for frame in tmp_frame_callback_buffer.drain(..) {
                    wl_server::objects::Callback::fire(frame, time, ctx).await?;
                }
                // Remove fired callbacks from pending state
                let mut shell = ctx.server_context().shell().borrow_mut();
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
