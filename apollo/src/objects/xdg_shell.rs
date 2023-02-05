use std::{future::Future, num::NonZeroU32, rc::Rc};

use derivative::Derivative;
use wl_protocol::stable::xdg_shell::{
    xdg_surface::v5 as xdg_surface, xdg_toplevel::v5 as xdg_toplevel,
    xdg_wm_base::v5 as xdg_wm_base,
};
use wl_server::{
    connection::{Client, State},
    error::Error,
    events::DispatchTo,
    objects::{wayland_object, Object},
};

use crate::{
    globals::xdg_shell::WmBaseState,
    shell::{xdg, HasShell, Shell, ShellOf},
    utils::geometry::{Extent, Point, Rectangle},
    objects::compositor::{SurfaceObject, get_surface_from_ctx},
};
#[derive(Debug)]
pub struct WmBase;

#[wayland_object]
impl<Ctx: Client> xdg_wm_base::RequestDispatch<Ctx> for WmBase
where
    Ctx::ServerContext: HasShell,
    Ctx::Object: From<Surface<<Ctx::ServerContext as HasShell>::Shell>>,
    <Ctx::ServerContext as HasShell>::Shell: crate::shell::xdg::XdgShell,
    Ctx: DispatchTo<crate::globals::xdg_shell::WmBase> + State<WmBaseState>,
{
    type Error = Error;

    type CreatePositionerFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetXdgSurfaceFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type PongFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn pong<'a>(ctx: &'a mut Ctx, object_id: u32, serial: u32) -> Self::PongFut<'a> {
        async move { unimplemented!() }
    }

    fn destroy<'a>(ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        async move { unimplemented!() }
    }

    fn get_xdg_surface<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
        surface: wl_types::Object,
    ) -> Self::GetXdgSurfaceFut<'a> {
        async move {
            use wl_server::connection::Objects;
            let pending_configure = ctx.state_mut().pending_configure.clone();
            let objects = ctx.objects();
            let surface_id = surface.0;
            let surface_obj = objects
                .get(surface_id)
                .ok_or_else(|| Error::UnknownObject(surface.0))?;
            let surface = surface_obj
                .cast::<crate::objects::compositor::Surface<ShellOf<Ctx::ServerContext>>>()
                .ok_or_else(|| Error::UnknownObject(surface_id))?
                .0
                .clone();
            drop(surface_obj);

            let mut shell = ctx.server_context().shell().borrow_mut();
            let inserted = objects.try_insert_with(id.0, || {
                let role = crate::shell::xdg::Surface::new(
                    id,
                    (ctx.event_handle(), Ctx::SLOT),
                    &pending_configure,
                );
                surface.set_role(role, &mut shell);
                Surface(surface).into()
            });
            if !inserted {
                Err(Error::IdExists(id.0))
            } else {
                Ok(())
            }
        }
    }

    fn create_positioner<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
    ) -> Self::CreatePositionerFut<'a> {
        async move { unimplemented!() }
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Surface<S: Shell>(pub(crate) Rc<crate::shell::surface::Surface<S>>);
impl<S: Shell> SurfaceObject<S> for Surface<S> {
    fn surface(&self) -> &Rc<crate::shell::surface::Surface<S>> {
        &self.0
    }
}

#[wayland_object]
impl<Ctx: Client, S: Shell> xdg_surface::RequestDispatch<Ctx> for Surface<S>
where
    Ctx::ServerContext: HasShell<Shell = S>,
    Ctx::Object: From<TopLevel<S>>,
    <Ctx::ServerContext as HasShell>::Shell: crate::shell::xdg::XdgShell,
{
    type Error = wl_server::error::Error;

    type AckConfigureFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetPopupFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetToplevelFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetWindowGeometryFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn destroy<'a>(ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        // TODO remove from WmBaseState::pending_configure
        async move { unimplemented!() }
    }

    fn get_popup<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
        parent: wl_types::Object,
        positioner: wl_types::Object,
    ) -> Self::GetPopupFut<'a> {
        async move { unimplemented!() }
    }

    fn get_toplevel<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
    ) -> Self::GetToplevelFut<'a> {
        let surface = get_surface_from_ctx::<_, Self>(ctx, object_id).unwrap();
        async move {
            use wl_server::connection::Objects;
            let inserted = ctx.objects().try_insert_with(id.0, || {
                let base_role = surface.role::<xdg::Surface>().unwrap().clone();
                let role = crate::shell::xdg::TopLevel::new(base_role, id.0);
                let mut shell = ctx.server_context().shell().borrow_mut();
                surface.set_role(role, &mut shell);
                TopLevel(surface).into()
            });
            if !inserted {
                Err(Error::IdExists(id.0))
            } else {
                Ok(())
            }
        }
    }

    fn ack_configure<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        serial: u32,
    ) -> Self::AckConfigureFut<'a> {
        let surface = get_surface_from_ctx::<_, Self>(ctx, object_id).unwrap();
        async move {
            let mut role = surface.role_mut::<xdg::Surface>().unwrap();
            while let Some(front) = role.pending_serial.front() {
                if front.get() > serial {
                    break
                }
                role.pending_serial.pop_front();
            }
            role.last_ack = Some(NonZeroU32::new(serial).ok_or(
                wl_server::error::Error::UnknownFatalError("ack_configure: serial is 0"),
            )?);
            Ok(())
        }
    }

    fn set_window_geometry<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Self::SetWindowGeometryFut<'a> {
        let surface = get_surface_from_ctx::<_, Self>(ctx, object_id).unwrap();
        async move {
            let mut role = surface.role_mut::<xdg::Surface>().unwrap();
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
pub struct TopLevel<S: Shell>(pub(crate) Rc<crate::shell::surface::Surface<S>>);
impl<S: Shell> SurfaceObject<S> for TopLevel<S> {
    fn surface(&self) -> &Rc<crate::shell::surface::Surface<S>> {
        &self.0
    }
}

#[wayland_object]
impl<Ctx: Client, S: Shell> xdg_toplevel::RequestDispatch<Ctx> for TopLevel<S>
where
    Ctx::ServerContext: HasShell<Shell = S>,
    <Ctx::ServerContext as HasShell>::Shell: crate::shell::xdg::XdgShell,
{
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
        ctx: &'a mut Ctx,
        object_id: u32,
        seat: wl_types::Object,
        serial: u32,
    ) -> Self::MoveFut<'a> {
        async move { unimplemented!() }
    }

    fn resize<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        seat: wl_types::Object,
        serial: u32,
        edges: xdg_toplevel::enums::ResizeEdge,
    ) -> Self::ResizeFut<'a> {
        async move { unimplemented!() }
    }

    fn destroy<'a>(ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        // TODO remove from WmBaseState::pending_configure
        async move { unimplemented!() }
    }

    fn set_title<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        title: wl_types::Str<'a>,
    ) -> Self::SetTitleFut<'a> {
        async move {
            let surface = get_surface_from_ctx::<_, Self>(ctx, object_id).unwrap();
            let mut role = surface.role_mut::<xdg::TopLevel>().unwrap();
            role.title = Some(String::from_utf8_lossy(title.0.to_bytes()).into_owned());
            Ok(())
        }
    }

    fn set_parent<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        parent: wl_types::Object,
    ) -> Self::SetParentFut<'a> {
        async move { unimplemented!() }
    }

    fn set_app_id<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        app_id: wl_types::Str<'a>,
    ) -> Self::SetAppIdFut<'a> {
        async move {
            let surface = get_surface_from_ctx::<_, Self>(ctx, object_id).unwrap();
            let mut role = surface.role_mut::<xdg::TopLevel>().unwrap();
            role.app_id = Some(String::from_utf8_lossy(app_id.0.to_bytes()).into_owned());
            Ok(())
        }
    }

    fn set_max_size<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        width: i32,
        height: i32,
    ) -> Self::SetMaxSizeFut<'a> {
        async move {
            let surface = get_surface_from_ctx::<_, Self>(ctx, object_id).unwrap();
            let mut role = surface.role_mut::<xdg::TopLevel>().unwrap();
            role.pending.max_size = Some(Extent::new(width, height));
            Ok(())
        }
    }

    fn set_min_size<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        width: i32,
        height: i32,
    ) -> Self::SetMinSizeFut<'a> {
        async move {
            let surface = get_surface_from_ctx::<_, Self>(ctx, object_id).unwrap();
            let mut role = surface.role_mut::<xdg::TopLevel>().unwrap();
            role.pending.min_size = Some(Extent::new(width, height));
            Ok(())
        }
    }

    fn set_maximized<'a>(ctx: &'a mut Ctx, object_id: u32) -> Self::SetMaximizedFut<'a> {
        async move { unimplemented!() }
    }

    fn set_minimized<'a>(ctx: &'a mut Ctx, object_id: u32) -> Self::SetMinimizedFut<'a> {
        async move { unimplemented!() }
    }

    fn set_fullscreen<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        output: wl_types::Object,
    ) -> Self::SetFullscreenFut<'a> {
        async move { unimplemented!() }
    }

    fn unset_maximized<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
    ) -> Self::UnsetMaximizedFut<'a> {
        async move { unimplemented!() }
    }

    fn show_window_menu<'a>(
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
        ctx: &'a mut Ctx,
        object_id: u32,
    ) -> Self::UnsetFullscreenFut<'a> {
        async move { unimplemented!() }
    }
}
