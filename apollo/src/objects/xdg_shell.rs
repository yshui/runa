use std::{cell::RefCell, future::Future, marker::PhantomData, rc::Rc};

use derivative::Derivative;
use wl_common::utils::geometry::{Extent, Point, Rectangle};
use wl_protocol::stable::xdg_shell::{
    xdg_surface::v5 as xdg_surface, xdg_toplevel::v5 as xdg_toplevel,
    xdg_wm_base::v5 as xdg_wm_base,
};
use wl_server::{
    connection::Connection,
    error::Error,
    objects::{interface_message_dispatch, InterfaceMeta},
    provide_any::{request_mut, request_ref, Demand},
    Extra,
};

use crate::shell::{Shell, xdg};
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

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
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
                entry.or_insert(Surface(surface.0.clone()));
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

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Surface<S: Shell>(Rc<crate::shell::surface::Surface<S>>);

impl<Ctx, S: Shell> InterfaceMeta<Ctx> for Surface<S> {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }

    fn interface(&self) -> &'static str {
        xdg_surface::NAME
    }
}

#[interface_message_dispatch]
impl<Ctx: Connection, S: Shell> xdg_surface::RequestDispatch<Ctx> for Surface<S> {
    type Error = wl_server::error::Error;

    type AckConfigureFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetPopupFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetToplevelFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetWindowGeometryFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn destroy<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        async move { unimplemented!() }
    }

    fn get_popup<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
        parent: wl_types::Object,
        positioner: wl_types::Object,
    ) -> Self::GetPopupFut<'a> {
        async move { unimplemented!() }
    }

    fn get_toplevel<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
    ) -> Self::GetToplevelFut<'a> {
        async move {
            use wl_server::connection::{Entry, Objects};
            let mut objects = ctx.objects().borrow_mut();
            let entry = objects.entry(id.0);
            if entry.is_vacant() {
                let role = crate::shell::xdg::TopLevel::default();
                assert!(
                    self.0
                        .role::<xdg::Surface>()
                        .is_some(),
                    "xdg_surface object pointing to a surface without the xdg_surface role"
                );
                self.0.set_role(role);
                entry.or_insert(TopLevel(self.0.clone()));
                Ok(())
            } else {
                Err(Error::IdExists(id.0))
            }
        }
    }

    fn ack_configure<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        serial: u32,
    ) -> Self::AckConfigureFut<'a> {
        async move { unimplemented!() }
    }

    fn set_window_geometry<'a>(
        &'a self,
        _ctx: &'a mut Ctx,
        _object_id: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Self::SetWindowGeometryFut<'a> {
        async move {
            let mut role = self.0.role_mut::<xdg::Surface>().unwrap();
            role.pending_geometry = Some(Rectangle {
                loc:  Point::new(x, y),
                size: Extent::new(width, height),
            });
            Ok(())
        }
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct TopLevel<S: Shell>(Rc<crate::shell::surface::Surface<S>>);

impl<Ctx, S: Shell> InterfaceMeta<Ctx> for TopLevel<S> {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }

    fn interface(&self) -> &'static str {
        xdg_toplevel::NAME
    }
}

#[interface_message_dispatch]
impl<Ctx, S: Shell> xdg_toplevel::RequestDispatch<Ctx> for TopLevel<S> {
    type Error = wl_server::error::Error;

    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type MoveFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type ResizeFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetAppIdFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetFullscreenFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetMaxSizeFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetMaximizedFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetMinSizeFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetMinimizedFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetParentFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetTitleFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type ShowWindowMenuFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type UnsetFullscreenFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type UnsetMaximizedFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn move_<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        seat: wl_types::Object,
        serial: u32,
    ) -> Self::MoveFut<'a> {
        async move { unimplemented!() }
    }

    fn resize<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        seat: wl_types::Object,
        serial: u32,
        edges: xdg_toplevel::enums::ResizeEdge,
    ) -> Self::ResizeFut<'a> {
        async move { unimplemented!() }
    }

    fn destroy<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        async move { unimplemented!() }
    }

    fn set_title<'a>(
        &'a self,
        _ctx: &'a mut Ctx,
        _object_id: u32,
        title: wl_types::Str<'a>,
    ) -> Self::SetTitleFut<'a> {
        async move {
            let mut role = self.0.role_mut::<xdg::TopLevel>().unwrap();
            role.title = Some(String::from_utf8_lossy(title.0.to_bytes()).into_owned());
            Ok(())
        }
    }

    fn set_parent<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        parent: wl_types::Object,
    ) -> Self::SetParentFut<'a> {
        async move { unimplemented!() }
    }

    fn set_app_id<'a>(
        &'a self,
        _ctx: &'a mut Ctx,
        _object_id: u32,
        app_id: wl_types::Str<'a>,
    ) -> Self::SetAppIdFut<'a> {
        async move {
            let mut role = self.0.role_mut::<xdg::TopLevel>().unwrap();
            role.app_id = Some(String::from_utf8_lossy(app_id.0.to_bytes()).into_owned());
            Ok(())
        }
    }

    fn set_max_size<'a>(
        &'a self,
        _ctx: &'a mut Ctx,
        _object_id: u32,
        width: i32,
        height: i32,
    ) -> Self::SetMaxSizeFut<'a> {
        async move {
            let mut role = self.0.role_mut::<xdg::TopLevel>().unwrap();
            role.pending.max_size = Some(Extent::new(width, height));
            Ok(())
        }
    }

    fn set_min_size<'a>(
        &'a self,
        _ctx: &'a mut Ctx,
        _object_id: u32,
        width: i32,
        height: i32,
    ) -> Self::SetMinSizeFut<'a> {
        async move {
            let mut role = self.0.role_mut::<xdg::TopLevel>().unwrap();
            role.pending.min_size = Some(Extent::new(width, height));
            Ok(())
        }
    }

    fn set_maximized<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::SetMaximizedFut<'a> {
        async move { unimplemented!() }
    }

    fn set_minimized<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::SetMinimizedFut<'a> {
        async move { unimplemented!() }
    }

    fn set_fullscreen<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        output: wl_types::Object,
    ) -> Self::SetFullscreenFut<'a> {
        async move { unimplemented!() }
    }

    fn unset_maximized<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
    ) -> Self::UnsetMaximizedFut<'a> {
        async move { unimplemented!() }
    }

    fn show_window_menu<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        seat: wl_types::Object,
        serial: u32,
        x: i32,
        y: i32,
    ) -> Self::ShowWindowMenuFut<'a> {
        async move { unimplemented!() }
    }

    fn unset_fullscreen<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
    ) -> Self::UnsetFullscreenFut<'a> {
        async move { unimplemented!() }
    }
}
