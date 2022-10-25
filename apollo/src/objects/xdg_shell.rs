use std::future::Future;

use wl_common::Infallible;
use wl_protocol::stable::xdg_shell::xdg_wm_base::v5 as xdg_wm_base;
use wl_server::{objects::{interface_message_dispatch, InterfaceMeta}, server::ServerBuilder};
#[derive(Debug, Default)]
pub struct WmBase;

impl InterfaceMeta for WmBase {
    fn interface(&self) -> &'static str {
        xdg_wm_base::NAME
    }

    fn provide<'a>(&'a self, demand: &mut wl_server::provide_any::Demand<'a>) {
        demand.provide_ref(self);
    }
}

#[interface_message_dispatch]
impl<Ctx> xdg_wm_base::RequestDispatch<Ctx> for WmBase {
    type Error = wl_server::error::Error;

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
        async move { unimplemented!() }
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

impl WmBase {
    pub fn init_server<S: ServerBuilder>(builder: &mut S) -> Result<(), Infallible> {
        builder.global(crate::globals::xdg_shell::WmBase);
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
