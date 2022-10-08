use std::{future::Future, pin::Pin};

use wl_protocol::wayland::{
    wl_compositor, wl_display, wl_registry, wl_shm::v1 as wl_shm,
    wl_subcompositor::v1 as wl_subcompositor,
};

use crate::{
    connection::{self, Connection},
    objects::InterfaceMeta,
    provide_any::Demand,
    renderer_capability::RendererCapability,
    server::Server,
};

type PinnedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

pub trait Global<S: Server + ?Sized> {
    fn interface(&self) -> &'static str;
    fn version(&self) -> u32;
    /// Called when the global is bound to a client, return the client side
    /// object, and optionally an I/O task to be completed after the object is
    /// inserted into the client's object store
    fn bind<'b, 'c>(
        &self,
        client: &'b S::Connection,
        object_id: u32,
    ) -> (
        Box<dyn InterfaceMeta>,
        Option<PinnedFuture<'c, std::io::Result<()>>>,
    )
    where
        'b: 'c;
    fn provide<'a>(&'a self, demand: &mut Demand<'a>);
}

impl<'b, S: Server> crate::provide_any::Provider for dyn Global<S> + 'b {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        self.provide(demand)
    }
}

#[derive(Debug, Default)]
pub struct Display;

impl<S> Global<S> for Display
where
    S: Server,
{
    fn interface(&self) -> &'static str {
        wl_display::v1::NAME
    }

    fn version(&self) -> u32 {
        wl_display::v1::VERSION
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
        (Box::new(crate::objects::Display), None)
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}

/// The registry singleton. This is an interface only object. The actual list of
/// globals is stored in the `Globals` implementation of the server context.
///
/// If you use this as your registry global implementation, you must also use
/// [`crate::objects::Registry`] as your client side registry proxy
/// implementation.
#[derive(Debug, Default)]
pub struct Registry;

impl<S> Global<S> for Registry
where
    S: Server + 'static,
    S::Connection: connection::Evented<S::Connection> + connection::Objects,
    crate::error::Error: From<<S::Connection as connection::Connection>::Error>,
{
    fn interface(&self) -> &'static str {
        wl_registry::v1::NAME
    }

    fn version(&self) -> u32 {
        wl_registry::v1::VERSION
    }

    fn bind<'b, 'c>(
        &self,
        client: &'b S::Connection,
        _object_id: u32,
    ) -> (
        Box<dyn InterfaceMeta>,
        Option<PinnedFuture<'c, std::io::Result<()>>>,
    )
    where
        'b: 'c,
    {
        // Registry::new would add a listener to the GlobalStore. If you want to
        // implement the Registry global yourself, you need to remember to do
        // this yourself, somewhere.
        (
            Box::new(crate::objects::Registry::<S::Connection>::new(client)),
            None,
        )
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}
