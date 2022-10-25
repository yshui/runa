use std::{future::Future, pin::Pin, marker::PhantomData};

pub mod xdg_shell;

use wl_protocol::wayland::{wl_compositor::v5 as wl_compositor, wl_shm::v1 as wl_shm, wl_subcompositor::v1 as wl_subcompositor};
use wl_server::{
    connection::Connection,
    globals::Global,
    objects::InterfaceMeta,
    server::{Globals, Server},
    provide_any::Demand,
    renderer_capability::RendererCapability,
};

use crate::shell::Shell;

type PinnedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

pub struct Compositor<S>(PhantomData<S>);
impl<S> Default for Compositor<S> {
    fn default() -> Self {
        Compositor(PhantomData)
    }
}

impl<S: Server, Sh: Shell> Global<S> for Compositor<Sh> {
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
        Box<dyn InterfaceMeta>,
        Option<PinnedFuture<'c, std::io::Result<()>>>,
    )
    where
        'b: 'c,
    {
        (Box::new(crate::objects::compositor::Compositor::<Sh>::new()), None)
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}

pub struct Subcompositor;

impl<S: Server> Global<S> for Subcompositor {
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
        Box<dyn InterfaceMeta>,
        Option<PinnedFuture<'c, std::io::Result<()>>>,
    )
    where
        'b: 'c,
    {
        (Box::new(crate::objects::compositor::Subcompositor::new()), None)
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}

#[derive(Default)]
pub struct Shm;

impl<S: Server + RendererCapability> Global<S> for Shm
{
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
        Box<dyn InterfaceMeta>,
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

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}
