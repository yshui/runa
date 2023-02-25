use std::{
    future::Future,
    num::NonZeroU32,
    pin::Pin,
    rc::Rc,
    task::{ready, Context, Poll},
};

use derivative::Derivative;
use wl_protocol::stable::xdg_shell::{
    xdg_surface::v5 as xdg_surface, xdg_toplevel::v5 as xdg_toplevel,
    xdg_wm_base::v5 as xdg_wm_base,
};
use wl_server::{
    connection::{
        event_handler::{Abortable, AutoAbortHandle},
        traits::{Client, EventHandlerAction, LockableStore, Store, WriteMessage},
    },
    error::Error,
    events::EventSource,
    objects::{wayland_object, ObjectMeta},
};

use crate::{
    shell::{surface::LayoutEvent, xdg, HasShell, Shell, ShellOf},
    utils::geometry::{Extent, Point, Rectangle},
};
#[derive(Debug)]
pub struct WmBase;

#[wayland_object]
impl<Ctx: Client> xdg_wm_base::RequestDispatch<Ctx> for WmBase
where
    Ctx::ServerContext: HasShell,
    Ctx::Object: From<Surface<<Ctx::ServerContext as HasShell>::Shell>>,
    <Ctx::ServerContext as HasShell>::Shell: crate::shell::xdg::XdgShell,
{
    type Error = Error;

    type CreatePositionerFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetXdgSurfaceFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type PongFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn pong(ctx: &mut Ctx, object_id: u32, serial: u32) -> Self::PongFut<'_> {
        async move { unimplemented!() }
    }

    fn destroy(ctx: &mut Ctx, object_id: u32) -> Self::DestroyFut<'_> {
        async move { unimplemented!() }
    }

    fn get_xdg_surface(
        ctx: &mut Ctx,
        _object_id: u32,
        id: wl_types::NewId,
        surface: wl_types::Object,
    ) -> Self::GetXdgSurfaceFut<'_> {
        async move {
            let objects = ctx.objects().clone();
            let mut objects = objects.lock().await;
            let surface_id = surface.0;
            let surface_obj = objects
                .get(surface_id)
                .ok_or_else(|| Error::UnknownObject(surface.0))?;
            let surface = surface_obj
                .cast::<crate::objects::compositor::Surface<ShellOf<Ctx::ServerContext>>>()
                .ok_or_else(|| Error::UnknownObject(surface_id))?
                .inner
                .clone();

            let server_ctx = ctx.server_context().clone();
            let mut shell = server_ctx.shell().borrow_mut();
            let inserted = objects.try_insert_with(id.0, || {
                let role = crate::shell::xdg::Surface::new(id);
                surface.set_role(role, &mut shell);
                Surface { inner: surface }.into()
            });
            if !inserted {
                Err(Error::IdExists(id.0))
            } else {
                Ok(())
            }
        }
    }

    fn create_positioner(
        ctx: &mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
    ) -> Self::CreatePositionerFut<'_> {
        async move { unimplemented!() }
    }
}

struct LayoutEventHandler {
    surface_object_id: u32,
}
use wl_server::connection::traits::EventHandler;
impl<Ctx: Client> EventHandler<Ctx> for LayoutEventHandler
where
    Ctx::ServerContext: HasShell,
    <Ctx::ServerContext as HasShell>::Shell: crate::shell::xdg::XdgShell,
{
    type Message = LayoutEvent;

    fn poll_handle_event(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        objects: &mut Ctx::ObjectStore,
        connection: &mut Ctx::Connection,
        server_context: &Ctx::ServerContext,
        message: &mut Self::Message,
    ) -> Poll<Result<EventHandlerAction, Box<dyn std::error::Error + Send + Sync + 'static>>> {
        tracing::debug!(?message, "LayoutEventHandler::poll_handle_event");
        let objects = objects.try_lock().unwrap();
        use crate::shell::{
            surface::Role,
            xdg::{Surface as XdgSurface, TopLevel},
        };
        let surface = objects.get(self.surface_object_id).unwrap();
        let surface = surface
            .cast::<crate::objects::compositor::Surface<<Ctx::ServerContext as HasShell>::Shell>>()
            .unwrap();
        let mut connection = Pin::new(connection);
        if let Some(size) = message.0.extent {
            if let Some(role_object_id) = surface.inner.role::<TopLevel>().map(|r| {
                assert!(
                    <TopLevel as Role<<Ctx::ServerContext as HasShell>::Shell>>::is_active(&r),
                    "TopLevel role no longer active"
                );
                r.object_id
            }) {
                let message = xdg_toplevel::events::Configure {
                    height: size.h as i32,
                    width:  size.w as i32,
                    states: &[],
                };
                ready!(connection.as_mut().poll_reserve(cx, &message))?;
                connection.as_mut().start_send(role_object_id, message);
            } else {
                // TODO: handle PopUp role
                unimplemented!()
            }

            // Remove the extent so we don't send the same event twice
            message.0.extent = None;
        }
        // Send xdg_surface.configure event
        let (serial, role_object_id) = {
            let mut role = surface.inner.role_mut::<XdgSurface>().unwrap();
            let serial = role.serial;
            role.serial = role.serial.checked_add(1).unwrap_or(1.try_into().unwrap());
            role.pending_serial.push_back(serial);
            (serial, role.object_id)
        };
        let message = xdg_surface::events::Configure {
            serial: serial.get(),
        };
        ready!(connection.as_mut().poll_reserve(cx, &message))?;
        connection.start_send(role_object_id, message);
        Poll::Ready(Ok(EventHandlerAction::Keep))
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Surface<S: Shell> {
    pub(crate) inner: Rc<crate::shell::surface::Surface<S>>,
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

    fn destroy(ctx: &mut Ctx, object_id: u32) -> Self::DestroyFut<'_> {
        // TODO remove from WmBaseState::pending_configure
        async move { unimplemented!() }
    }

    fn get_popup(
        ctx: &mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
        parent: wl_types::Object,
        positioner: wl_types::Object,
    ) -> Self::GetPopupFut<'_> {
        async move { unimplemented!() }
    }

    fn get_toplevel(
        ctx: &mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
    ) -> Self::GetToplevelFut<'_> {
        async move {
            let objects = ctx.objects().clone();
            let mut objects = objects.lock().await;
            let surface = objects
                .get(object_id)
                .unwrap()
                .cast::<Self>()
                .unwrap()
                .inner
                .clone();
            let inserted = objects.try_insert_with(id.0, || {
                let base_role = surface.role::<xdg::Surface>().unwrap().clone();
                let role = crate::shell::xdg::TopLevel::new(base_role, id.0);
                {
                    let mut shell = ctx.server_context().shell().borrow_mut();
                    surface.set_role(role, &mut shell);
                }

                // Start listening to layout events
                let rx = <_ as EventSource<LayoutEvent>>::subscribe(&*surface);
                let (handler, abort) = Abortable::new(LayoutEventHandler {
                    surface_object_id: surface.object_id(),
                });
                ctx.add_event_handler(rx, handler);
                // `abort.auto_abort()` creates a handle which will stop the event handler when
                // the TopLevel object is destroyed.
                TopLevel(surface, abort.auto_abort()).into()
            });
            if !inserted {
                Err(Error::IdExists(id.0))
            } else {
                Ok(())
            }
        }
    }

    fn ack_configure(ctx: &mut Ctx, object_id: u32, serial: u32) -> Self::AckConfigureFut<'_> {
        async move {
            let objects = ctx.objects().lock().await;
            let this = objects.get(object_id).unwrap().cast::<Self>().unwrap();
            let mut role = this.inner.role_mut::<xdg::Surface>().unwrap();
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

    fn set_window_geometry(
        ctx: &mut Ctx,
        object_id: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Self::SetWindowGeometryFut<'_> {
        async move {
            let objects = ctx.objects().lock().await;
            let this = objects.get(object_id).unwrap().cast::<Self>().unwrap();
            let mut role = this.inner.role_mut::<xdg::Surface>().unwrap();
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
pub struct TopLevel<S: Shell>(
    pub(crate) Rc<crate::shell::surface::Surface<S>>,
    AutoAbortHandle,
);

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

    fn move_(
        ctx: &mut Ctx,
        object_id: u32,
        seat: wl_types::Object,
        serial: u32,
    ) -> Self::MoveFut<'_> {
        async move { unimplemented!() }
    }

    fn resize(
        ctx: &mut Ctx,
        object_id: u32,
        seat: wl_types::Object,
        serial: u32,
        edges: xdg_toplevel::enums::ResizeEdge,
    ) -> Self::ResizeFut<'_> {
        async move { unimplemented!() }
    }

    fn destroy(ctx: &mut Ctx, object_id: u32) -> Self::DestroyFut<'_> {
        // TODO remove from WmBaseState::pending_configure
        async move { unimplemented!() }
    }

    fn set_title<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        title: wl_types::Str<'a>,
    ) -> Self::SetTitleFut<'a> {
        async move {
            let objects = ctx.objects().lock().await;
            let this = objects.get(object_id).unwrap().cast::<Self>().unwrap();
            let mut role = this.0.role_mut::<xdg::TopLevel>().unwrap();
            role.title = Some(String::from_utf8_lossy(title.0.to_bytes()).into_owned());
            Ok(())
        }
    }

    fn set_parent(
        ctx: &mut Ctx,
        object_id: u32,
        parent: wl_types::Object,
    ) -> Self::SetParentFut<'_> {
        async move { unimplemented!() }
    }

    fn set_app_id<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        app_id: wl_types::Str<'a>,
    ) -> Self::SetAppIdFut<'a> {
        async move {
            let objects = ctx.objects().lock().await;
            let this = objects.get(object_id).unwrap().cast::<Self>().unwrap();
            let mut role = this.0.role_mut::<xdg::TopLevel>().unwrap();
            role.app_id = Some(String::from_utf8_lossy(app_id.0.to_bytes()).into_owned());
            Ok(())
        }
    }

    fn set_max_size(
        ctx: &mut Ctx,
        object_id: u32,
        width: i32,
        height: i32,
    ) -> Self::SetMaxSizeFut<'_> {
        async move {
            let objects = ctx.objects().lock().await;
            let this = objects.get(object_id).unwrap().cast::<Self>().unwrap();
            let mut role = this.0.role_mut::<xdg::TopLevel>().unwrap();
            role.pending.max_size = Some(Extent::new(width, height));
            Ok(())
        }
    }

    fn set_min_size(
        ctx: &mut Ctx,
        object_id: u32,
        width: i32,
        height: i32,
    ) -> Self::SetMinSizeFut<'_> {
        async move {
            let objects = ctx.objects().lock().await;
            let this = objects.get(object_id).unwrap().cast::<Self>().unwrap();
            let mut role = this.0.role_mut::<xdg::TopLevel>().unwrap();
            role.pending.min_size = Some(Extent::new(width, height));
            Ok(())
        }
    }

    fn set_maximized(ctx: &mut Ctx, object_id: u32) -> Self::SetMaximizedFut<'_> {
        async move { unimplemented!() }
    }

    fn set_minimized(ctx: &mut Ctx, object_id: u32) -> Self::SetMinimizedFut<'_> {
        async move { unimplemented!() }
    }

    fn set_fullscreen(
        ctx: &mut Ctx,
        object_id: u32,
        output: wl_types::Object,
    ) -> Self::SetFullscreenFut<'_> {
        async move { unimplemented!() }
    }

    fn unset_maximized(ctx: &mut Ctx, object_id: u32) -> Self::UnsetMaximizedFut<'_> {
        async move { unimplemented!() }
    }

    fn show_window_menu(
        ctx: &mut Ctx,
        object_id: u32,
        seat: wl_types::Object,
        serial: u32,
        x: i32,
        y: i32,
    ) -> Self::ShowWindowMenuFut<'_> {
        async move { unimplemented!() }
    }

    fn unset_fullscreen(ctx: &mut Ctx, object_id: u32) -> Self::UnsetFullscreenFut<'_> {
        async move { unimplemented!() }
    }
}
