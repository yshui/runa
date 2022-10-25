//! Proxy for the compositor global
//!
//! The compositor global is responsible for managing the sets of surfaces a
//! client has. According to the wayland spec, each surface has a set of
//! double-buffered states: updates are made to the pending state first, and
//! applied to current state when `wl_surface.commit` is called.
//!
//! Another core interface, wl_subsurface, add some persistent data structure
//! flavor to the mix. A subsurface can be in synchronized mode, and state
//! commit will create a new "version" of the surface tree. And visibility of
//! changes can be propagated from bottom up through commits on the
//! parent, grand-parent, etc.
//!
//! We deal with this requirement with COW (copy-on-write) techniques. Details
//! are documented in the types' document.
use std::{
    cell::{Cell, RefCell},
    future::Future,
    rc::Rc, marker::PhantomData,
};

use wl_common::{interface_message_dispatch, Infallible};
use wl_protocol::wayland::{wl_compositor::v5 as wl_compositor, wl_subcompositor::v1 as wl_subcompositor};
use wl_server::{
    connection::Connection,
    error,
    objects::InterfaceMeta,
    provide_any::{Demand, Provider},
    server::{Globals, Server, ServerBuilder},
    Extra,
};

use crate::shell::Shell;
/// The reference implementation of wl_compositor
#[derive(Debug)]
pub struct Compositor<S>(PhantomData<S>);

#[interface_message_dispatch]
impl<S: Shell, Ctx: Connection + 'static> wl_compositor::RequestDispatch<Ctx> for Compositor<S>
where
    Ctx::Context: Extra<RefCell<S>>,
{
    type Error = error::Error;

    type CreateRegionFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type CreateSurfaceFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn create_surface<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
    ) -> Self::CreateSurfaceFut<'a> {
        async move { Err(error::Error::IdExists(id.0)) }
    }

    fn create_region<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
    ) -> Self::CreateRegionFut<'a> {
        async { unimplemented!() }
    }
}
impl<S: Shell> Compositor<S> {
    pub fn new() -> Self {
        Compositor(PhantomData)
    }

    pub async fn handle_events<Ctx: 'static>(
        _ctx: &Ctx,
        _slot: usize,
        _event: &'static str,
    ) -> Result<(), error::Error> {
        Ok(())
    }

    pub fn init_server<Ctx: ServerBuilder>(server: &mut Ctx) -> Result<(), wl_common::Infallible> {
        server
            .global(crate::globals::Compositor::<S>::default())
            .event_slot("wl_compositor");
        Ok(())
    }
}

impl<S: 'static> InterfaceMeta for Compositor<S> {
    fn interface(&self) -> &'static str {
        wl_compositor::NAME
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}

pub struct Subcompositor;

impl Subcompositor {
    pub fn new() -> Self {
        Self
    }

    pub fn init_server<S: ServerBuilder>(server: &mut S) -> Result<(), Infallible> {
        server.global(crate::globals::Subcompositor);
        Ok(())
    }

    pub async fn handle_events<Ctx>(
        _ctx: &Ctx,
        _slot: usize,
        _event: &'static str,
    ) -> Result<(), Infallible> {
        Ok(())
    }
}

impl InterfaceMeta for Subcompositor {
    fn interface(&self) -> &'static str {
        wl_subcompositor::NAME
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}

#[interface_message_dispatch]
impl<Ctx> wl_subcompositor::RequestDispatch<Ctx> for Subcompositor {
    type Error = wl_server::error::Error;

    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetSubsurfaceFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn destroy<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        async move { unimplemented!() }
    }

    fn get_subsurface<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
        surface: wl_types::Object,
        parent: wl_types::Object,
    ) -> Self::GetSubsurfaceFut<'a> {
        async move { unimplemented!() }
    }
}
