use wl_protocol::wayland::{wl_registry, wl_display};

use crate::{
    connection::{self, InterfaceMeta},
    provide_any::Demand,
    server::Server,
};

pub trait Global<S: Server + ?Sized> {
    fn interface(&self) -> &'static str;
    fn version(&self) -> u32;
    fn bind(&self, client: &S::Connection) -> Box<dyn InterfaceMeta>;
    fn provide<'a>(&'a self, demand: &mut Demand<'a>);
}

impl<'b, S: Server> crate::provide_any::Provider for dyn Global<S> + 'b {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        self.provide(demand)
    }
}

#[derive(Debug, Default)]
pub struct Display;

impl<S> Global<S> for Display where S: Server {
    fn interface(&self) -> &'static str {
        wl_display::v1::NAME
    }
    fn version(&self) -> u32 {
        wl_display::v1::VERSION
    }
    fn bind(&self, _client: &<S as Server>::Connection) -> Box<dyn InterfaceMeta> {
        Box::new(crate::objects::Display)
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
    S::Globals: Clone,
    S::Connection: connection::Evented<S::Connection> + connection::Objects,
    crate::error::Error: From<<S::Connection as connection::Connection>::Error>,
{
    fn interface(&self) -> &'static str {
        wl_registry::v1::NAME
    }

    fn version(&self) -> u32 {
        wl_registry::v1::VERSION
    }

    fn bind(&self, client: &S::Connection) -> Box<dyn InterfaceMeta> {
        // Registry::new would add a listener to the GlobalStore. If you want to
        // implement the Registry global yourself, you need to remember to do
        // this yourself, somewhere.
        Box::new(crate::objects::Registry::<S::Connection>::new(client))
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}
