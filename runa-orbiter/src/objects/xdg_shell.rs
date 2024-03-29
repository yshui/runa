//! Objects related to the xdg_shell protocol.

use std::{future::Future, num::NonZeroU32, pin::Pin, rc::Rc};

use derive_where::derive_where;
use runa_core::{
    client::traits::{
        Client, ClientParts, EventDispatcher, EventHandler, EventHandlerAction, Store,
    },
    error::{Error, ProtocolError},
    events::EventSource,
    objects::wayland_object,
};
use runa_io::traits::WriteMessage;
use runa_wayland_protocols::stable::xdg_shell::{
    xdg_surface::v5 as xdg_surface, xdg_toplevel::v5 as xdg_toplevel,
    xdg_wm_base::v5 as xdg_wm_base,
};
use runa_wayland_types::{NewId, Object as WaylandObject, Str};

use crate::{
    shell::{
        surface::{roles::xdg as xdg_roles, LayoutEvent},
        HasShell, Shell,
    },
    utils::geometry::{Extent, Point, Rectangle},
};

/// Implementation of the `xdg_wm_base` interface.
#[derive(Debug, Clone, Copy)]
pub struct WmBase;

#[wayland_object]
impl<S: crate::shell::xdg::XdgShell, Ctx: Client> xdg_wm_base::RequestDispatch<Ctx> for WmBase
where
    Ctx::ServerContext: HasShell<Shell = S>,
    Ctx::Object: From<Surface<S>>,
{
    type Error = Error;

    type CreatePositionerFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetXdgSurfaceFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type PongFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn pong(_ctx: &mut Ctx, object_id: u32, serial: u32) -> Self::PongFut<'_> {
        async move {
            tracing::error!(object_id, serial, "xdg_wm_base.pong not implemented");
            Err(Error::NotImplemented("xdg_wm_base.pong"))
        }
    }

    fn destroy(ctx: &mut Ctx, object_id: u32) -> Self::DestroyFut<'_> {
        async move {
            ctx.objects_mut().remove(object_id).unwrap();
            Ok(())
        }
    }

    fn get_xdg_surface(
        ctx: &mut Ctx,
        _object_id: u32,
        id: NewId,
        surface: WaylandObject,
    ) -> Self::GetXdgSurfaceFut<'_> {
        use crate::objects::compositor;
        async move {
            let ClientParts {
                objects,
                server_context,
                ..
            } = ctx.as_mut_parts();
            let surface_id = surface.0;
            let surface_obj = objects.get::<compositor::Surface<S>>(surface_id)?;
            let surface = surface_obj.inner.clone();

            let mut shell = server_context.shell().borrow_mut();
            let inserted = objects
                .try_insert_with(id.0, || {
                    let role = xdg_roles::Surface::new(id);
                    surface.set_role(role, &mut shell);
                    Surface { inner: surface }.into()
                })
                .is_some();
            if !inserted {
                Err(Error::IdExists(id.0))
            } else {
                Ok(())
            }
        }
    }

    fn create_positioner(
        _ctx: &mut Ctx,
        object_id: u32,
        id: NewId,
    ) -> Self::CreatePositionerFut<'_> {
        async move {
            tracing::error!(
                object_id,
                ?id,
                "xdg_wm_base.create_positioner not implemented"
            );
            Err(Error::NotImplemented("xdg_wm_base.create_positioner"))
        }
    }
}

struct LayoutEventHandler {
    surface_object_id: u32,
}
impl<Ctx: Client> EventHandler<Ctx> for LayoutEventHandler
where
    Ctx::ServerContext: HasShell,
    <Ctx::ServerContext as HasShell>::Shell: crate::shell::xdg::XdgShell,
{
    type Message = LayoutEvent;

    type Future<'ctx> = impl Future<
            Output = Result<EventHandlerAction, Box<dyn std::error::Error + Send + Sync + 'static>>,
        > + 'ctx;

    fn handle_event<'ctx>(
        &'ctx mut self,
        objects: &'ctx mut Ctx::ObjectStore,
        connection: &'ctx mut Ctx::Connection,
        _server_context: &'ctx Ctx::ServerContext,
        message: &'ctx mut Self::Message,
    ) -> Self::Future<'ctx> {
        tracing::debug!(?message, "LayoutEventHandler::poll_handle_event");
        use crate::shell::surface::roles::Role;
        let surface = objects
            .get::<crate::objects::compositor::Surface<<Ctx::ServerContext as HasShell>::Shell>>(
                self.surface_object_id,
            )
            .unwrap();
        let mut connection = Pin::new(connection);
        async move {
            if let Some(size) = message.0.extent {
                if let Some(role_object_id) =
                    surface.inner.role::<xdg_roles::TopLevel>().map(|r| {
                        assert!(
                            <xdg_roles::TopLevel as Role<
                                <Ctx::ServerContext as HasShell>::Shell,
                            >>::is_active(&r),
                            "TopLevel role no longer active"
                        );
                        r.object_id
                    })
                {
                    connection
                        .as_mut()
                        .send(role_object_id, xdg_toplevel::events::Configure {
                            height: size.h as i32,
                            width:  size.w as i32,
                            states: &[],
                        })
                        .await?;
                } else {
                    // TODO: handle PopUp role
                    unimplemented!()
                }
            }
            // Send xdg_surface.configure event
            let (serial, role_object_id) = {
                let mut role = surface.inner.role_mut::<xdg_roles::Surface>().unwrap();
                let serial = role.serial;
                role.serial = role.serial.checked_add(1).unwrap_or(1.try_into().unwrap());
                role.pending_serial.push_back(serial);
                (serial, role.object_id)
            };
            connection
                .send(role_object_id, xdg_surface::events::Configure {
                    serial: serial.get(),
                })
                .await?;
            Ok(EventHandlerAction::Keep)
        }
    }
}

/// Implementation of the `xdg_surface` interface.
#[derive_where(Debug)]
pub struct Surface<S: Shell> {
    pub(crate) inner: Rc<crate::shell::surface::Surface<S>>,
}

struct SurfaceError {
    kind:      xdg_surface::enums::Error,
    object_id: u32,
}

impl std::fmt::Debug for SurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use xdg_surface::enums::Error::*;
        match self.kind {
            DefunctRoleObject => {
                write!(
                    f,
                    "Surface {} was destroyed before its role object",
                    self.object_id
                )
            },
            NotConstructed => {
                write!(f, "Surface {} was not fully constructed", self.object_id)
            },
            AlreadyConstructed => {
                write!(f, "Surface {} was already constructed", self.object_id)
            },
            UnconfiguredBuffer => {
                write!(
                    f,
                    "Attaching a buffer to an unconfigured surface {}",
                    self.object_id
                )
            },
            InvalidSerial => {
                write!(
                    f,
                    "Invalid serial number when acking a configure event, xdg_surface: {}",
                    self.object_id
                )
            },
            InvalidSize => {
                write!(
                    f,
                    "Width or height was zero or negative, xdg_surface: {}",
                    self.object_id
                )
            },
        }
    }
}

impl std::fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        <Self as std::fmt::Debug>::fmt(self, f)
    }
}

impl std::error::Error for SurfaceError {}

impl ProtocolError for SurfaceError {
    fn fatal(&self) -> bool {
        true
    }

    fn wayland_error(&self) -> Option<(u32, u32)> {
        Some((self.object_id, self.kind as u32))
    }
}

#[wayland_object]
impl<Ctx: Client, S: Shell> xdg_surface::RequestDispatch<Ctx> for Surface<S>
where
    Ctx::ServerContext: HasShell<Shell = S>,
    Ctx::Object: From<TopLevel<S>>,
    <Ctx::ServerContext as HasShell>::Shell: crate::shell::xdg::XdgShell,
{
    type Error = runa_core::error::Error;

    type AckConfigureFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetPopupFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetToplevelFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetWindowGeometryFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn destroy(ctx: &mut Ctx, object_id: u32) -> Self::DestroyFut<'_> {
        let object = ctx.objects().get::<Self>(object_id).unwrap();
        if object.inner.role_is_active() {
            return futures_util::future::err(Error::custom(SurfaceError {
                object_id,
                kind: xdg_surface::enums::Error::DefunctRoleObject,
            }))
        }
        ctx.objects_mut().remove(object_id);
        futures_util::future::ok(())
    }

    fn get_popup(
        _ctx: &mut Ctx,
        object_id: u32,
        id: NewId,
        parent: WaylandObject,
        positioner: WaylandObject,
    ) -> Self::GetPopupFut<'_> {
        async move {
            tracing::error!(
                object_id,
                ?id,
                ?parent,
                ?positioner,
                "get_popup not implemented"
            );
            Err(Error::NotImplemented("xdg_wm_base.get_popup"))
        }
    }

    fn get_toplevel(ctx: &mut Ctx, object_id: u32, id: NewId) -> Self::GetToplevelFut<'_> {
        async move {
            let ClientParts {
                objects,
                server_context,
                event_dispatcher,
                ..
            } = ctx.as_mut_parts();
            let surface = objects.get::<Self>(object_id).unwrap().inner.clone();
            let inserted = objects
                .try_insert_with(id.0, || {
                    let base_role = surface.role::<xdg_roles::Surface>().unwrap().clone();
                    let role = xdg_roles::TopLevel::new(base_role, id.0);
                    surface.set_role(role, &mut server_context.shell().borrow_mut());

                    // Start listening to layout events
                    let rx = <_ as EventSource<LayoutEvent>>::subscribe(&*surface);
                    let (rx, abort_handle) = futures_util::stream::abortable(rx);
                    event_dispatcher.add_event_handler(rx, LayoutEventHandler {
                        surface_object_id: surface.object_id(),
                    });
                    // `abort_handle.into()` creates a handle which will terminate the event stream
                    // when the TopLevel object is destroyed.
                    TopLevel(surface, abort_handle.into()).into()
                })
                .is_some();
            if !inserted {
                Err(Error::IdExists(id.0))
            } else {
                Ok(())
            }
        }
    }

    fn ack_configure(ctx: &mut Ctx, object_id: u32, serial: u32) -> Self::AckConfigureFut<'_> {
        async move {
            let this = ctx.objects().get::<Self>(object_id).unwrap();
            let mut role = this.inner.role_mut::<xdg_roles::Surface>().unwrap();
            while let Some(front) = role.pending_serial.front() {
                if front.get() > serial {
                    break
                }
                role.pending_serial.pop_front();
            }
            role.last_ack = Some(NonZeroU32::new(serial).ok_or(
                runa_core::error::Error::UnknownFatalError("ack_configure: serial is 0"),
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
            let this = ctx.objects().get::<Self>(object_id).unwrap();
            let mut role = this.inner.role_mut::<xdg_roles::Surface>().unwrap();
            role.pending_geometry = Some(Rectangle {
                loc:  Point::new(x, y),
                size: Extent::new(width, height),
            });
            Ok(())
        }
    }
}

/// Implements the `xdg_toplevel` interface.
#[derive_where(Debug)]
pub struct TopLevel<S: Shell>(
    pub(crate) Rc<crate::shell::surface::Surface<S>>,
    // Intentionally not used, it's here so when the TopLevel object is dropped, the event stream
    // will be terminated automatically.
    #[allow(dead_code)] crate::utils::AutoAbort,
);

#[wayland_object]
impl<Ctx: Client, S: Shell> xdg_toplevel::RequestDispatch<Ctx> for TopLevel<S>
where
    Ctx::ServerContext: HasShell<Shell = S>,
    S: crate::shell::xdg::XdgShell + Shell,
{
    type Error = runa_core::error::Error;

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
        _ctx: &mut Ctx,
        object_id: u32,
        seat: WaylandObject,
        serial: u32,
    ) -> Self::MoveFut<'_> {
        async move {
            tracing::error!(object_id, ?seat, serial, "move is not implemented");
            Err(Error::NotImplemented("xdg_toplevel.move"))
        }
    }

    fn resize(
        _ctx: &mut Ctx,
        object_id: u32,
        seat: WaylandObject,
        serial: u32,
        edges: xdg_toplevel::enums::ResizeEdge,
    ) -> Self::ResizeFut<'_> {
        async move {
            tracing::error!(
                object_id,
                ?seat,
                serial,
                ?edges,
                "resize is not implemented"
            );
            Err(Error::NotImplemented("xdg_toplevel.resize"))
        }
    }

    fn destroy(ctx: &mut Ctx, object_id: u32) -> Self::DestroyFut<'_> {
        use runa_core::objects::AnyObject;
        let object = ctx.objects_mut().remove(object_id).unwrap();
        let object = object.cast::<Self>().unwrap();
        object
            .0
            .deactivate_role(&mut ctx.server_context().shell().borrow_mut());
        futures_util::future::ok(())
    }

    fn set_title<'a>(ctx: &'a mut Ctx, object_id: u32, title: Str<'a>) -> Self::SetTitleFut<'a> {
        async move {
            let this = ctx.objects().get::<Self>(object_id).unwrap();
            let mut role = this.0.role_mut::<xdg_roles::TopLevel>().unwrap();
            role.title = Some(String::from_utf8_lossy(title.0).into_owned());
            Ok(())
        }
    }

    fn set_parent(_ctx: &mut Ctx, object_id: u32, parent: WaylandObject) -> Self::SetParentFut<'_> {
        async move {
            tracing::error!(object_id, ?parent, "set_parent is not implemented");
            Err(Error::NotImplemented("xdg_toplevel.set_parent"))
        }
    }

    fn set_app_id<'a>(ctx: &'a mut Ctx, object_id: u32, app_id: Str<'a>) -> Self::SetAppIdFut<'a> {
        async move {
            let this = ctx.objects().get::<Self>(object_id).unwrap();
            let mut role = this.0.role_mut::<xdg_roles::TopLevel>().unwrap();
            role.app_id = Some(String::from_utf8_lossy(app_id.0).into_owned());
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
            let this = ctx.objects().get::<Self>(object_id).unwrap();
            let mut role = this.0.role_mut::<xdg_roles::TopLevel>().unwrap();
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
            let this = ctx.objects().get::<Self>(object_id).unwrap();
            let mut role = this.0.role_mut::<xdg_roles::TopLevel>().unwrap();
            role.pending.min_size = Some(Extent::new(width, height));
            Ok(())
        }
    }

    fn set_maximized(_ctx: &mut Ctx, object_id: u32) -> Self::SetMaximizedFut<'_> {
        async move {
            tracing::error!(object_id, "set_maximized is not implemented");
            Err(Error::NotImplemented("xdg_toplevel.set_maximized"))
        }
    }

    fn set_minimized(_ctx: &mut Ctx, object_id: u32) -> Self::SetMinimizedFut<'_> {
        async move {
            tracing::error!(object_id, "set_minimized is not implemented");
            Err(Error::NotImplemented("xdg_toplevel.set_minimized"))
        }
    }

    fn set_fullscreen(
        _ctx: &mut Ctx,
        object_id: u32,
        output: WaylandObject,
    ) -> Self::SetFullscreenFut<'_> {
        async move {
            tracing::error!(object_id, ?output, "set_fullscreen is not implemented");
            Err(Error::NotImplemented("xdg_toplevel.set_fullscreen"))
        }
    }

    fn unset_maximized(_ctx: &mut Ctx, object_id: u32) -> Self::UnsetMaximizedFut<'_> {
        async move {
            tracing::error!(object_id, "unset_maximized is not implemented");
            Err(Error::NotImplemented("xdg_toplevel.unset_maximized"))
        }
    }

    fn show_window_menu(
        _ctx: &mut Ctx,
        object_id: u32,
        seat: WaylandObject,
        serial: u32,
        x: i32,
        y: i32,
    ) -> Self::ShowWindowMenuFut<'_> {
        async move {
            tracing::error!(
                object_id,
                ?seat,
                serial,
                x,
                y,
                "show_window_menu is not implemented"
            );
            Err(Error::NotImplemented("xdg_toplevel.show_window_menu"))
        }
    }

    fn unset_fullscreen(_ctx: &mut Ctx, object_id: u32) -> Self::UnsetFullscreenFut<'_> {
        async move {
            tracing::error!(object_id, "unset_fullscreen is not implemented");
            Err(Error::NotImplemented("xdg_toplevel.unset_fullscreen"))
        }
    }
}
