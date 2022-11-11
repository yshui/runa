use std::{cell::RefCell, future::Future, marker::PhantomData, pin::Pin};

pub mod xdg_shell;

use futures_util::future::Pending;
use wl_common::Infallible;
use wl_protocol::wayland::{
    wl_compositor::v5 as wl_compositor, wl_shm::v1 as wl_shm,
    wl_subcompositor::v1 as wl_subcompositor,
};
use wl_server::{
    connection::Connection,
    global_dispatch,
    globals::{Global, GlobalMeta},
    objects::Object,
    renderer_capability::RendererCapability,
    server::Server,
};
use derivative::Derivative;

use crate::shell::{Shell, HasShell, buffers::HasBuffer};

type PinnedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

#[derive(Derivative)]
#[derivative(Default(bound = ""), Debug(bound = ""))]
pub struct Compositor;

impl<S: Server + HasShell> GlobalMeta<S> for Compositor {
    fn interface(&self) -> &'static str {
        wl_compositor::NAME
    }

    fn version(&self) -> u32 {
        wl_compositor::VERSION
    }

    fn bind<'b, 'c>(
        &self,
        _client: &'b <S as Server>::Connection,
        _object_id: u32,
    ) -> (
        Box<dyn Object<S::Connection>>,
        Option<PinnedFuture<'c, std::io::Result<()>>>,
    )
    where
        'b: 'c,
    {
        (
            Box::new(crate::objects::compositor::Compositor),
            None,
        )
    }
}

impl<Ctx> Global<Ctx> for Compositor
where
    Ctx: Connection,
    Ctx::Context: HasShell,
{
    type Error = wl_server::error::Error;
    type HandleEventsError = Infallible;
    type HandleEventsFut<'a> = Pending<Result<(), Self::HandleEventsError>> where Ctx: 'a;

    const INIT: Self = Self;

    global_dispatch! {
        "wl_compositor" => crate::objects::compositor::Compositor,
        "wl_surface" => crate::objects::compositor::Surface<Ctx>,
        "wl_buffer" => crate::objects::Buffer<Ctx>,
    }

    fn handle_events<'a>(_ctx: &'a mut Ctx, _slot: usize) -> Option<Self::HandleEventsFut<'a>> {
        None
    }
}

#[derive(Debug)]
pub struct Subcompositor;

impl<Ctx> Global<Ctx> for Subcompositor
where
    Ctx: Connection,
    Ctx::Context: HasShell,
{
    type Error = wl_server::error::Error;
    type HandleEventsError = Infallible;
    type HandleEventsFut<'a> = Pending<Result<(), Self::HandleEventsError>> where Ctx: 'a;

    const INIT: Self = Self;

    global_dispatch! {
        "wl_subcompositor" => crate::objects::compositor::Subcompositor,
        "wl_subsurface" => crate::objects::compositor::Subsurface<Ctx>,
    }

    fn handle_events<'a>(_ctx: &'a mut Ctx, _slot: usize) -> Option<Self::HandleEventsFut<'a>> {
        None
    }
}

impl<S: Server> GlobalMeta<S> for Subcompositor {
    fn interface(&self) -> &'static str {
        wl_subcompositor::NAME
    }

    fn version(&self) -> u32 {
        wl_subcompositor::VERSION
    }

    fn bind<'b, 'c>(
        &self,
        _client: &'b <S as Server>::Connection,
        _object_id: u32,
    ) -> (
        Box<dyn Object<S::Connection>>,
        Option<PinnedFuture<'c, std::io::Result<()>>>,
    )
    where
        'b: 'c,
    {
        (
            Box::new(crate::objects::compositor::Subcompositor),
            None,
        )
    }
}

#[derive(Default)]
pub struct Shm;

impl<Ctx> Global<Ctx> for Shm
where
    Ctx: Connection,
    Ctx::Context: HasBuffer,
    <Ctx::Context as HasBuffer>::Buffer: From<crate::objects::shm::Buffer>,
{
    type Error = wl_server::error::Error;
    type HandleEventsError = Infallible;
    type HandleEventsFut<'a> = Pending<Result<(), Self::HandleEventsError>>;

    const INIT: Self = Self;

    global_dispatch! {
        "wl_shm" => crate::objects::shm::Shm,
        "wl_shm_pool" => crate::objects::shm::ShmPool,
    }

    fn handle_events<'a>(_ctx: &'a mut Ctx, _slot: usize) -> Option<Self::HandleEventsFut<'a>> {
        None
    }
}

impl<S: Server + RendererCapability> GlobalMeta<S> for Shm {
    fn interface(&self) -> &'static str {
        wl_shm::NAME
    }

    fn version(&self) -> u32 {
        wl_shm::VERSION
    }

    fn bind<'b, 'c>(
        &self,
        client: &'b <S as Server>::Connection,
        object_id: u32,
    ) -> (
        Box<dyn Object<S::Connection>>,
        Option<PinnedFuture<'c, std::io::Result<()>>>,
    )
    where
        'b: 'c,
    {
        let formats = client.server_context().formats();
        (
            Box::new(crate::objects::shm::Shm::new()),
            Some(Box::pin(async move {
                // Send known buffer formats
                for format in formats {
                    client
                        .send(
                            object_id,
                            wl_shm::Event::Format(wl_shm::events::Format { format }),
                        )
                        .await?;
                }
                Ok(())
            })),
        )
    }
}
