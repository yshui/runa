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
use std::{future::Future, rc::Rc};

use derivative::Derivative;
use wl_protocol::wayland::{
    wl_buffer::v1 as wl_buffer, wl_compositor::v5 as wl_compositor, wl_display::v1 as wl_display,
    wl_output::v4 as wl_output, wl_subcompositor::v1 as wl_subcompositor,
    wl_subsurface::v1 as wl_subsurface, wl_surface::v5 as wl_surface,
};
use wl_server::{
    connection::{Client, State},
    error,
    events::DispatchTo,
    objects::{wayland_object, Object, DISPLAY_ID},
};

use crate::{
    globals::OutputAndCompositorState,
    shell::{self, buffers::HasBuffer, surface::roles, HasShell, Shell, ShellOf},
    utils::geometry::Point,
};

/// Trait for objects that contains a surface reference
pub(crate) trait SurfaceObject<S: Shell> {
    fn surface(&self) -> &Rc<crate::shell::surface::Surface<S>>;
}

pub(crate) fn get_surface_from_ctx<Ctx: Client, Obj: SurfaceObject<ShellOf<Ctx::ServerContext>> + 'static>(
    ctx: &Ctx,
    id: u32,
) -> Option<Rc<crate::shell::surface::Surface<<Ctx::ServerContext as HasShell>::Shell>>>
where
    Ctx::ServerContext: HasShell,
{
    use wl_server::{connection::Objects, objects::Object};
    let obj = ctx.objects().get(id)?;
    let obj: &Obj = obj.cast()?;
    Some(obj.surface().clone())
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Surface<Shell: shell::Shell>(pub(crate) Rc<crate::shell::surface::Surface<Shell>>);

impl<S: Shell> SurfaceObject<S> for Surface<S> {
    fn surface(&self) -> &Rc<crate::shell::surface::Surface<S>> {
        &self.0
    }
}

fn deallocate_surface<Ctx: Client>(
    this: &mut Surface<<Ctx::ServerContext as HasShell>::Shell>,
    ctx: &mut Ctx,
) where
    Ctx::ServerContext: HasShell,
    Ctx: State<OutputAndCompositorState<<Ctx::ServerContext as HasShell>::Shell>>,
{
    let server_context = ctx.server_context().clone();
    let mut shell = server_context.shell().borrow_mut();
    this.0
        .destroy(&mut shell, &mut ctx.state_mut().commit_scratch_buffer);
}

#[derive(Debug)]
enum SurfaceErrorKind {
    InvalidOffset,
}

impl std::fmt::Display for SurfaceErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOffset => write!(f, "invalid offset"),
        }
    }
}

#[derive(Debug)]
struct SurfaceError {
    kind:      wl_surface::enums::Error,
    object_id: u32,
}

impl std::fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use wl_surface::enums::Error::*;
        match self.kind {
            InvalidScale => write!(f, "invalid scale"),
            InvalidTransform => {
                write!(f, "invalid transform")
            },
            InvalidOffset => write!(f, "invalid offset"),
            InvalidSize => write!(f, "invalid size"),
        }
    }
}

impl std::error::Error for SurfaceError {}

impl wl_protocol::ProtocolError for SurfaceError {
    fn wayland_error(&self) -> Option<(u32, u32)> {
        Some((self.object_id, self.kind as u32))
    }

    fn fatal(&self) -> bool {
        true
    }
}

#[wayland_object(on_disconnect = "deallocate_surface")]
impl<Ctx: Client, S: shell::Shell> wl_surface::RequestDispatch<Ctx> for Surface<S>
where
    Ctx::ServerContext: HasShell<Shell = S> + HasBuffer<Buffer = S::Buffer>,
    Ctx: State<OutputAndCompositorState<S>>,
    Ctx::Object: From<wl_server::objects::Callback>,
{
    type Error = error::Error;

    type AttachFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type CommitFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type DamageBufferFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type DamageFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type FrameFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type OffsetFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type SetBufferScaleFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type SetBufferTransformFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type SetInputRegionFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type SetOpaqueRegionFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn frame<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        callback: wl_types::NewId,
    ) -> Self::FrameFut<'a> {
        let surface = get_surface_from_ctx::<_, Self>(ctx, object_id).unwrap();
        async move {
            use wl_server::connection::Objects;
            let objects = ctx.objects();
            let inserted = objects.try_insert_with(callback.0, || {
                let mut shell = ctx.server_context().shell().borrow_mut();
                let state = surface.pending_mut(&mut shell);
                state.add_frame_callback(callback.0);
                wl_server::objects::Callback.into()
            });
            if !inserted {
                Err(error::Error::IdExists(callback.0))
            } else {
                Ok(())
            }
        }
    }

    fn attach<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        buffer: wl_types::Object,
        x: i32,
        y: i32,
    ) -> Self::AttachFut<'a> {
        use wl_server::connection::Objects;
        let surface = get_surface_from_ctx::<_, Self>(ctx, object_id).unwrap();
        async move {
            if x != 0 || y != 0 {
                return Err(error::Error::custom(SurfaceError {
                    object_id,
                    kind: wl_surface::enums::Error::InvalidOffset,
                }))
            }
            let objects = ctx.objects();
            let buffer_id = buffer.0;
            let Some(buffer) = objects.get(buffer.0) else { return Err(error::Error::UnknownObject(buffer_id)); };
            if buffer.interface() != wl_buffer::NAME {
                return Err(error::Error::InvalidObject(buffer_id))
            }
            let mut shell = ctx.server_context().shell().borrow_mut();
            let state = surface.pending_mut(&mut shell);
            let buffer = buffer
                .cast::<crate::objects::Buffer<<Ctx::ServerContext as HasBuffer>::Buffer>>()
                .unwrap();
            state.set_buffer(Some(buffer.buffer.clone()));
            Ok(())
        }
    }

    fn damage<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Self::DamageFut<'a> {
        let surface = get_surface_from_ctx::<_, Self>(ctx, object_id).unwrap();
        async move {
            let mut shell = ctx.server_context().shell().borrow_mut();
            let state = surface.pending_mut(&mut shell);
            state.damage_buffer();
            Ok(())
        }
    }

    fn damage_buffer<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Self::DamageBufferFut<'a> {
        let surface = get_surface_from_ctx::<_, Self>(ctx, object_id).unwrap();
        async move {
            let mut shell = ctx.server_context().shell().borrow_mut();
            let state = surface.pending_mut(&mut shell);
            state.damage_buffer();
            Ok(())
        }
    }

    fn commit<'a>(ctx: &'a mut Ctx, object_id: u32) -> Self::CommitFut<'a> {
        let surface = get_surface_from_ctx::<_, Self>(ctx, object_id).unwrap();
        async move {
            use wl_server::connection::WriteMessage;

            use crate::shell::buffers::Buffer;
            let server_context = ctx.server_context().clone();
            let released_buffer = {
                let mut shell = server_context.shell().borrow_mut();
                let old_buffer = surface.current(&shell).buffer().cloned();
                surface
                    .commit(&mut shell, &mut ctx.state_mut().commit_scratch_buffer)
                    .map_err(wl_server::error::Error::UnknownFatalError)?;

                let new_buffer = surface.current(&shell).buffer().cloned();
                drop(shell);

                // TODO: this released_buffer calculation is wrong.
                if let Some(old_buffer) = old_buffer {
                    if new_buffer.map_or(true, |new_buffer| !Rc::ptr_eq(&old_buffer, &new_buffer)) {
                        Some(old_buffer.object_id())
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(released_buffer) = released_buffer {
                ctx.connection()
                    .send(released_buffer, wl_buffer::events::Release {})
                    .await?;
            }
            Ok(())
        }
    }

    fn set_buffer_scale<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        scale: i32,
    ) -> Self::SetBufferScaleFut<'a> {
        let surface = get_surface_from_ctx::<_, Self>(ctx, object_id).unwrap();
        let mut shell = ctx.server_context().shell().borrow_mut();
        let pending_mut = surface.pending_mut(&mut shell);

        pending_mut.set_buffer_scale(scale as u32 * 120);
        futures_util::future::ok(())
    }

    fn set_input_region<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        region: wl_types::Object,
    ) -> Self::SetInputRegionFut<'a> {
        async move { unimplemented!() }
    }

    fn set_opaque_region<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        region: wl_types::Object,
    ) -> Self::SetOpaqueRegionFut<'a> {
        async move { unimplemented!() }
    }

    fn set_buffer_transform<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        transform: wl_output::enums::Transform,
    ) -> Self::SetBufferTransformFut<'a> {
        async move { unimplemented!() }
    }

    fn offset<'a>(ctx: &'a mut Ctx, object_id: u32, x: i32, y: i32) -> Self::OffsetFut<'a> {
        async move { unimplemented!() }
    }

    fn destroy<'a>(ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        async move {
            use wl_server::connection::{WriteMessage, Objects};
            let server_context = ctx.server_context().clone();
            let this = ctx.objects().remove(object_id).unwrap();
            let this: &Self = this.cast().unwrap();
            let state = ctx.state_mut();
            if state.surfaces.remove(&object_id).is_none() {
                panic!("Incosistent compositor state, surface {object_id} not found");
            }
            // Remove the surface from surface with output changed.
            state
                .output_changed
                .write_end()
                .borrow_mut()
                .remove(&object_id);

            this.0.destroy(
                &mut server_context.shell().borrow_mut(),
                &mut state.commit_scratch_buffer,
            );

            ctx.connection()
                .send(DISPLAY_ID, wl_display::events::DeleteId { id: object_id })
                .await?;
            Ok(())
        }
    }
}

/// The reference implementation of wl_compositor
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Compositor;

#[wayland_object]
impl<Ctx: Client> wl_compositor::RequestDispatch<Ctx> for Compositor
where
    Ctx::ServerContext: HasShell,
    Ctx: State<OutputAndCompositorState<<Ctx::ServerContext as HasShell>::Shell>>
        + DispatchTo<crate::globals::Compositor>,
    Ctx::Object: From<Surface<<Ctx::ServerContext as HasShell>::Shell>>,
{
    type Error = error::Error;

    type CreateRegionFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type CreateSurfaceFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn create_surface<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
    ) -> Self::CreateSurfaceFut<'a> {
        use futures_util::future::ready;
        use wl_server::connection::Objects;

        use crate::shell::surface;
        if ctx.objects().get(id.0).is_none() {
            // Add the id to the list of surfaces
            ctx.state_mut().surfaces.insert(id.0, Default::default());
            let (ctx, state) = ctx.state();
            let objects = ctx.objects();
            let shell = ctx.server_context().shell();
            let mut shell = shell.borrow_mut();
            let surface = Rc::new(surface::Surface::new(id));
            let current = shell.allocate(surface::SurfaceState::new(surface.clone()));
            let current_state = shell.get_mut(current);
            current_state.stack_index = Some(current_state.stack_mut().push_back(
                crate::shell::surface::SurfaceStackEntry {
                    token:    current,
                    position: Point::new(0, 0),
                },
            ));
            surface.set_current(current);
            surface.set_pending(current);
            shell.post_commit(None, current);

            // Listen for output change events
            let handle = ctx.event_handle();
            let slot = <Ctx as DispatchTo<crate::globals::Compositor>>::SLOT;
            surface.add_output_change_listener((handle, slot), state.output_changed.write_end());

            tracing::debug!("id {} is surface {:p}", id.0, surface);
            objects.insert(id.0, Surface(surface)).unwrap();
            ready(Ok(()))
        } else {
            ready(Err(error::Error::IdExists(id.0)))
        }
    }

    fn create_region<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
    ) -> Self::CreateRegionFut<'a> {
        async { unimplemented!() }
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Subsurface<S: Shell>(Rc<crate::shell::surface::Surface<S>>);
impl<S: Shell> SurfaceObject<S> for Subsurface<S> {
    fn surface(&self) -> &Rc<crate::shell::surface::Surface<S>> {
        &self.0
    }
}

#[wayland_object]
impl<Ctx: Client, S: Shell> wl_subsurface::RequestDispatch<Ctx> for Subsurface<S>
where
    Ctx::ServerContext: HasShell<Shell = S>,
{
    type Error = error::Error;

    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type PlaceAboveFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type PlaceBelowFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetDesyncFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetPositionFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetSyncFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn set_sync<'a>(ctx: &'a mut Ctx, object_id: u32) -> Self::SetSyncFut<'a> {
        async move { unimplemented!() }
    }

    fn set_desync<'a>(ctx: &'a mut Ctx, object_id: u32) -> Self::SetDesyncFut<'a> {
        async move { unimplemented!() }
    }

    fn destroy<'a>(ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        async move { unimplemented!() }
    }

    fn place_above<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        sibling: wl_types::Object,
    ) -> Self::PlaceAboveFut<'a> {
        async move { unimplemented!() }
    }

    fn place_below<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        sibling: wl_types::Object,
    ) -> Self::PlaceBelowFut<'a> {
        async move { unimplemented!() }
    }

    fn set_position<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        x: i32,
        y: i32,
    ) -> Self::SetPositionFut<'a> {
        async move {
            let mut shell = ctx.server_context().shell().borrow_mut();
            let surface = get_surface_from_ctx::<_, Self>(ctx, object_id).unwrap();
            let role = surface
                .role::<roles::Subsurface<<Ctx::ServerContext as HasShell>::Shell>>()
                .unwrap();
            let parent = role.parent().upgrade().unwrap().pending_mut(&mut shell);
            parent
                .stack_mut()
                .get_mut(role.stack_index)
                .unwrap()
                .position = Point::new(x, y);
            Ok(())
        }
    }
}

#[derive(Debug)]
pub struct Subcompositor;

#[derive(Debug)]
pub enum Error {
    BadSurface {
        bad_surface:       u32,
        subsurface_object: u32,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::BadSurface { bad_surface, .. } => write!(f, "Bad surface id {bad_surface}"),
        }
    }
}

impl std::error::Error for Error {}

impl wl_protocol::ProtocolError for Error {
    fn wayland_error(&self) -> Option<(u32, u32)> {
        match self {
            Error::BadSurface {
                bad_surface,
                subsurface_object,
            } => Some((
                *subsurface_object,
                wl_subcompositor::enums::Error::BadSurface as u32,
            )),
        }
    }

    fn fatal(&self) -> bool {
        true
    }
}

#[wayland_object]
impl<Ctx: Client> wl_subcompositor::RequestDispatch<Ctx> for Subcompositor
where
    Ctx::ServerContext: HasShell,
    Ctx::Object: From<Subsurface<<Ctx::ServerContext as HasShell>::Shell>>,
{
    type Error = wl_server::error::Error;

    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetSubsurfaceFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn destroy<'a>(ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        async move { unimplemented!() }
    }

    fn get_subsurface<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
        surface: wl_types::Object,
        parent: wl_types::Object,
    ) -> Self::GetSubsurfaceFut<'a> {
        use wl_server::connection::Objects;
        tracing::debug!(
            "get_subsurface, id: {:?}, surface: {:?}, parent: {:?}",
            id,
            surface,
            parent
        );
        async move {
            let shell = ctx.server_context().shell();
            let objects = ctx.objects();
            let mut shell = shell.borrow_mut();
            let surface_id = surface.0;
            let surface = objects
                .get(surface.0)
                .and_then(|r| {
                    r.cast()
                        .map(|sur: &Surface<ShellOf<Ctx::ServerContext>>| sur.0.clone())
                })
                .ok_or_else(|| {
                    Self::Error::custom(Error::BadSurface {
                        bad_surface:       surface.0,
                        subsurface_object: object_id,
                    })
                })?;
            let parent = objects
                .get(parent.0)
                .and_then(|r| {
                    r.cast()
                        .map(|sur: &Surface<ShellOf<Ctx::ServerContext>>| sur.0.clone())
                })
                .ok_or_else(|| {
                    Self::Error::custom(Error::BadSurface {
                        bad_surface:       parent.0,
                        subsurface_object: object_id,
                    })
                })?;
            if objects.get(id.0).is_none() {
                if !crate::shell::surface::roles::Subsurface::attach(
                    parent,
                    surface.clone(),
                    &mut shell,
                ) {
                    Err(Self::Error::custom(Error::BadSurface {
                        bad_surface:       surface_id,
                        subsurface_object: object_id,
                    }))
                } else {
                    objects.insert(id.0, Subsurface(surface));
                    Ok(())
                }
            } else {
                Err(Self::Error::IdExists(id.0))
            }
        }
    }
}
