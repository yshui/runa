use std::{cell::RefCell, future::Future, marker::PhantomData, rc::Rc};

use wl_common::Infallible;
use wl_protocol::stable::xdg_shell::xdg_wm_base::v5 as xdg_wm_base;
use wl_server::{
    connection::Connection,
    error::Error,
    objects::{interface_message_dispatch, InterfaceMeta},
    provide_any::request_ref,
    server::ServerBuilder,
    Extra,
};

use crate::shell::Shell;
#[derive(Debug)]
pub struct WmBase<S>(PhantomData<S>);

impl<S> Default for WmBase<S> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<Ctx, S: 'static> InterfaceMeta<Ctx> for WmBase<S> {
    fn interface(&self) -> &'static str {
        xdg_wm_base::NAME
    }

    fn provide<'a>(&'a self, demand: &mut wl_server::provide_any::Demand<'a>) {
        demand.provide_ref(self);
    }
}

#[interface_message_dispatch]
impl<S: Shell, Ctx: Connection> xdg_wm_base::RequestDispatch<Ctx> for WmBase<S>
where
    Ctx::Context: Extra<RefCell<S>>,
{
    type Error = Error;

    type CreatePositionerFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetXdgSurfaceFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type PongFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn pong<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32, serial: u32) -> Self::PongFut<'a> {
        async move { unimplemented!() }
    }

    fn destroy<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        async move { unimplemented!() }
    }

    fn get_xdg_surface<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
        surface: wl_types::Object,
    ) -> Self::GetXdgSurfaceFut<'a> {
        async move {
            use wl_server::connection::{Entry, Objects};
            let mut objects = ctx.objects().borrow_mut();
            let surface_id = surface.0;
            let surface = objects
                .get(surface_id)
                .ok_or_else(|| Error::UnknownObject(surface.0))?
                .clone();
            let entry = objects.entry(id.0);
            let shell: &RefCell<S> = ctx.server_context().extra();
            let surface: &crate::objects::compositor::Surface<S, Ctx> =
                request_ref(surface.as_ref()).ok_or_else(|| Error::UnknownObject(surface_id))?;
            if entry.is_vacant() {
                let role = crate::shell::xdg::Surface::default();
                surface.0.set_role(role);
                // TODO: create xdg_surface object.
                Ok(())
            } else {
                Err(Error::IdExists(id.0))
            }
        }
    }

    fn create_positioner<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
    ) -> Self::CreatePositionerFut<'a> {
        async move { unimplemented!() }
    }
}

impl<Sh: Shell> WmBase<Sh> {
    pub fn init_server<S: ServerBuilder>(builder: &mut S) -> Result<(), Infallible> {
        builder.global(crate::globals::xdg_shell::WmBase::<Sh>::default());
        Ok(())
    }

    pub async fn handle_events<Ctx: 'static>(
        ctx: &Ctx,
        slot: usize,
        event: &'static str,
    ) -> Result<(), Infallible> {
        Ok(())
    }
}
