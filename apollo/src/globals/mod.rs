use std::{cell::RefCell, future::Future, marker::PhantomData, pin::Pin, rc::Rc};

pub mod xdg_shell;

use derivative::Derivative;
use futures_util::future::Pending;
use wl_common::Infallible;
use wl_protocol::wayland::{
    wl_compositor::v5 as wl_compositor, wl_shm::v1 as wl_shm,
    wl_subcompositor::v1 as wl_subcompositor,
};
use wl_server::{
    connection::{Connection, State},
    events::{DispatchTo, EventHandler},
    global_dispatch,
    globals::{Global, GlobalDispatch, GlobalMeta},
    objects::Object,
    renderer_capability::RendererCapability,
    server::Server,
};

use crate::shell::{buffers::HasBuffer, HasShell, Shell};

type PinnedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

#[derive(Derivative)]
#[derivative(Default(bound = ""), Debug(bound = ""))]
pub struct Compositor;

impl GlobalMeta for Compositor {
    fn interface(&self) -> &'static str {
        wl_compositor::NAME
    }

    fn version(&self) -> u32 {
        wl_compositor::VERSION
    }
}
impl<Ctx: 'static> Global<Ctx> for Compositor
where
    Ctx: State<crate::objects::compositor::CompositorState> + DispatchTo<Self> + Connection,
    Ctx::Context: HasShell,
{
    fn bind<'b, 'c>(
        &self,
        client: &'b mut Ctx,
        _object_id: u32,
    ) -> PinnedFuture<'c, std::io::Result<Box<dyn Object>>>
    where
        'b: 'c,
    {
        client.set_state(Default::default());
        client
            .server_context()
            .shell()
            .borrow()
            .add_render_listener((client.event_handle(), Ctx::SLOT));
        Box::pin(futures_util::future::ok(
            Box::new(crate::objects::compositor::Compositor) as _,
        ))
    }
}

impl<Ctx> GlobalDispatch<Ctx> for Compositor
where
    Ctx: Connection + State<crate::objects::compositor::CompositorState> + DispatchTo<Self>,
    Ctx::Context: HasShell,
{
    type Error = wl_server::error::Error;

    const INIT: Self = Self;

    global_dispatch! {
        "wl_compositor" => crate::objects::compositor::Compositor,
        "wl_surface" => crate::objects::compositor::Surface<Ctx>,
        "wl_buffer" => crate::objects::Buffer<<Ctx::Context as HasBuffer>::Buffer>,
    }
}

impl<Ctx> EventHandler<Ctx> for Compositor
where
    Ctx: DispatchTo<Self> + State<crate::objects::compositor::CompositorState> + Connection,
    Ctx::Context: HasShell,
{
    type Error = std::io::Error;

    type Fut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a
        where
            Ctx: 'a;

    fn invoke(ctx: &mut Ctx) -> Self::Fut<'_> {
        use wl_server::connection::Objects;
        async move {
            let state = ctx.state().unwrap();
            let time = crate::time::elapsed().as_millis() as u32;
            for surface in &state.surfaces {
                let surface = ctx.objects().borrow().get(*surface).unwrap().clone(); // don't hold
                                                                                     // the Ref
                let surface =
                    Rc::downcast::<crate::objects::compositor::Surface<Ctx>>(surface).unwrap();
                let mut shell = ctx.server_context().shell().borrow_mut();
                let current = surface.0.current_mut(&mut shell);
                let fired = current.frame_callback.len();
                for frame in current.frame_callback.drain(..) {
                    wl_server::objects::Callback::fire(frame, time, ctx).await?;
                }
                // Remove fired callbacks from pending state
                let pending = surface.0.pending_mut(&mut shell);
                for _ in 0..fired {
                    pending.frame_callback.pop_front();
                }
            }
            Ok(())
        }
    }
}

#[derive(Debug)]
pub struct Subcompositor;

impl<Ctx> GlobalDispatch<Ctx> for Subcompositor
where
    Ctx: Connection,
    Ctx::Context: HasShell,
{
    type Error = wl_server::error::Error;

    const INIT: Self = Self;

    global_dispatch! {
        "wl_subcompositor" => crate::objects::compositor::Subcompositor,
        "wl_subsurface" => crate::objects::compositor::Subsurface<Ctx>,
    }
}

impl GlobalMeta for Subcompositor {
    fn interface(&self) -> &'static str {
        wl_subcompositor::NAME
    }

    fn version(&self) -> u32 {
        wl_subcompositor::VERSION
    }
}
impl<Ctx> Global<Ctx> for Subcompositor {
    fn bind<'b, 'c>(
        &self,
        _client: &'b mut Ctx,
        _object_id: u32,
    ) -> PinnedFuture<'c, std::io::Result<Box<dyn Object>>>
    where
        'b: 'c,
    {
        Box::pin(futures_util::future::ok(
            Box::new(crate::objects::compositor::Subcompositor) as _,
        ))
    }
}

#[derive(Default)]
pub struct Shm;

impl<Ctx> GlobalDispatch<Ctx> for Shm
where
    Ctx: Connection,
    Ctx::Context: HasBuffer,
    <Ctx::Context as HasBuffer>::Buffer: From<crate::objects::shm::Buffer>,
{
    type Error = wl_server::error::Error;

    const INIT: Self = Self;

    global_dispatch! {
        "wl_shm" => crate::objects::shm::Shm,
        "wl_shm_pool" => crate::objects::shm::ShmPool,
    }
}

impl GlobalMeta for Shm {
    fn interface(&self) -> &'static str {
        wl_shm::NAME
    }

    fn version(&self) -> u32 {
        wl_shm::VERSION
    }
}
impl<Ctx: Connection> Global<Ctx> for Shm
where
    Ctx::Context: RendererCapability,
{
    fn bind<'b, 'c>(
        &self,
        client: &'b mut Ctx,
        object_id: u32,
    ) -> PinnedFuture<'c, std::io::Result<Box<dyn Object>>>
    where
        'b: 'c,
    {
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
            Ok(Box::new(crate::objects::shm::Shm::new()) as _)
        })
    }
}
