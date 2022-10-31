use std::{future::Future, pin::Pin};

use wl_protocol::wayland::{
   wl_display, wl_registry,
};

use crate::{
    connection::{Connection, Evented, EventStates},
    objects::InterfaceMeta,
    provide_any::Demand,
    server::{EventSource, Server},
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
        Box<dyn InterfaceMeta<S::Connection>>,
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
        Box<dyn InterfaceMeta<S::Connection>>,
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
    S: Server + EventSource + 'static,
    S::Connection: Evented<S::Connection>,
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
        object_id: u32,
    ) -> (
        Box<dyn InterfaceMeta<S::Connection>>,
        Option<PinnedFuture<'c, std::io::Result<()>>>,
    )
    where
        'b: 'c,
    {
        use crate::objects::RegistryState;
        let slot = client
            .server_context()
            .slots()
            .iter()
            .position(|s| *s == wl_registry::v1::NAME)
            .unwrap() as u8;
        // Registry::new would add a listener into the Server EventSource. If you want
        // to implement the Registry global yourself, you need to remember to do
        // this yourself, somewhere.
        (
            Box::new(crate::objects::Registry::<S::Connection>::new(client)),
            // Send the client a registry event if the proxy is inserted, the event handler will
            // send the list of existing globals to the client.
            // Also set up the registry state in the client context.
            Some(Box::pin(async move {
                // Add this object_id to the list of registry objects bound.
                let state = client.event_states().with_mut(slot as u8, |state: &mut RegistryState<S::Connection>| {
                    state.add_registry_object(object_id);
                }).expect("Registry slot doesn't contain a RegistryState");
                if state.is_none() {
                    // This could be the first registry object, so the state might not be set.
                    tracing::debug!("Creating new registry state");
                    client.event_states().set(
                        slot as u8,
                        RegistryState::<S::Connection>::new(object_id),
                    )
                    .unwrap();
                }
                client.event_handle().set(slot);
                Ok(())
            })),
        )
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}