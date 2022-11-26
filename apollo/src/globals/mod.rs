use std::{future::Future, pin::Pin};

pub mod xdg_shell;

use derivative::Derivative;
use wl_common::InterfaceMessageDispatch;
use wl_protocol::wayland::{
    wl_compositor::v5 as wl_compositor, wl_shm::v1 as wl_shm,
    wl_subcompositor::v1 as wl_subcompositor,
};
use wl_server::{
    connection::{ClientContext, State},
    events::{DispatchTo, EventHandler},
    globals::{Bind, ConstInit},
    renderer_capability::RendererCapability,
};

use crate::{
    objects::compositor::CompositorState,
    shell::{buffers::HasBuffer, HasShell, Shell, ShellOf},
};

type PinnedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

#[derive(Derivative)]
#[derivative(Default(bound = ""), Debug(bound = ""))]
pub struct Compositor;

impl ConstInit for Compositor {
    const INIT: Self = Self;
}
impl<Ctx: ClientContext> Bind<Ctx> for Compositor
where
    Ctx: State<CompositorState> + DispatchTo<Self>,
    Ctx::Context: HasShell,
{
    type Objects = CompositorObject<Ctx>;

    fn interface(&self) -> &'static str {
        wl_compositor::NAME
    }

    fn version(&self) -> u32 {
        wl_compositor::VERSION
    }

    fn bind<'a>(
        &'a self,
        client: &'a mut Ctx,
        _object_id: u32,
    ) -> PinnedFuture<'a, std::io::Result<Self::Objects>> {
        client
            .server_context()
            .shell()
            .borrow()
            .add_render_listener((client.event_handle(), Ctx::SLOT));
        Box::pin(futures_util::future::ok(CompositorObject::Compositor(
            crate::objects::compositor::Compositor,
        )))
    }
}

#[derive(InterfaceMessageDispatch, Derivative)]
#[derivative(Debug(bound = "<Ctx::Context as HasBuffer>::Buffer: std::fmt::Debug"))]
pub enum CompositorObject<Ctx>
where
    Ctx: ClientContext,
    Ctx::Context: HasShell,
{
    Compositor(crate::objects::compositor::Compositor),
    Surface(crate::objects::compositor::Surface<<Ctx::Context as HasShell>::Shell>),
    Buffer(crate::objects::Buffer<<Ctx::Context as HasBuffer>::Buffer>),
}

impl<Ctx> EventHandler<Ctx> for Compositor
where
    Ctx: DispatchTo<Self> + State<CompositorState> + ClientContext,
    Ctx::Context: HasShell,
{
    type Error = std::io::Error;

    type Fut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a
        where
            Ctx: 'a;

    fn invoke(ctx: &mut Ctx) -> Self::Fut<'_> {
        use wl_server::{connection::Objects, objects::Object};
        async move {
            // Use a tmp buffer so we don't need to hold RefMut of `current` acorss await.
            let mut tmp_frame_callback_buffer = ctx
                .state_mut()
                .tmp_frame_callback_buffer
                .take()
                .unwrap();
            let state = ctx.state();
            let empty = Default::default();
            let surfaces = state.map(|s| &s.surfaces).unwrap_or(&empty);
            let time = crate::time::elapsed().as_millis() as u32;
            for (surface, _) in surfaces {
                let surface = ctx.objects().borrow().get(*surface).unwrap().clone(); // don't hold
                                                                                     // the Ref
                let surface: &crate::objects::compositor::Surface<ShellOf<Ctx::Context>> =
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

#[derive(InterfaceMessageDispatch, Derivative)]
#[derivative(Debug(bound = ""))]
pub enum SubcompositorObject<Ctx>
where
    Ctx: ClientContext,
    Ctx::Context: HasShell,
{
    Subcompositor(crate::objects::compositor::Subcompositor),
    Subsurface(crate::objects::compositor::Subsurface<Ctx>),
}

impl ConstInit for Subcompositor {
    const INIT: Self = Self;
}
impl<Ctx: ClientContext> Bind<Ctx> for Subcompositor
where
    Ctx::Context: HasShell,
{
    type Objects = SubcompositorObject<Ctx>;

    fn interface(&self) -> &'static str {
        wl_subcompositor::NAME
    }

    fn version(&self) -> u32 {
        wl_subcompositor::VERSION
    }

    fn bind<'a>(
        &'a self,
        _client: &'a mut Ctx,
        _object_id: u32,
    ) -> PinnedFuture<'a, std::io::Result<Self::Objects>> {
        Box::pin(futures_util::future::ok(
            SubcompositorObject::Subcompositor(crate::objects::compositor::Subcompositor),
        ))
    }
}

#[derive(Default)]
pub struct Shm;

#[derive(InterfaceMessageDispatch, Debug)]
pub enum ShmObject {
    Shm(crate::objects::shm::Shm),
    ShmPool(crate::objects::shm::ShmPool),
}

impl ConstInit for Shm {
    const INIT: Self = Self;
}
impl<Ctx: ClientContext> Bind<Ctx> for Shm
where
    Ctx::Context: RendererCapability,
{
    type Objects = ShmObject;

    fn interface(&self) -> &'static str {
        wl_shm::NAME
    }

    fn version(&self) -> u32 {
        wl_shm::VERSION
    }

    fn bind<'a>(
        &'a self,
        client: &'a mut Ctx,
        object_id: u32,
    ) -> PinnedFuture<'a, std::io::Result<Self::Objects>> {
        let formats = client.server_context().formats();
        Box::pin(async move {
            // Send known buffer formats
            for format in formats {
                client
                    .send(
                        object_id,
                        wl_shm::Event::Format(wl_shm::events::Format { format }),
                    )
                    .await?;
            }
            Ok(ShmObject::Shm(crate::objects::shm::Shm::new()))
        })
    }
}
