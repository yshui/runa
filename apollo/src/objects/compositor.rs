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
use std::{any::Any, cell::RefCell, future::Future, rc::Rc, collections::VecDeque};

use derivative::Derivative;
use wl_common::{interface_message_dispatch, utils::geometry::Point};
use wl_protocol::wayland::{
    wl_buffer::v1 as wl_buffer, wl_compositor::v5 as wl_compositor, wl_display::v1 as wl_display,
    wl_output::v4 as wl_output, wl_subcompositor::v1 as wl_subcompositor,
    wl_subsurface::v1 as wl_subsurface, wl_surface::v5 as wl_surface,
};
use wl_server::{
    connection::{ClientContext, State},
    error,
    objects::{Object, DISPLAY_ID},
};

use crate::{
    globals::{CompositorObject, SubcompositorObject},
    shell::{self, buffers::HasBuffer, output::Output, surface::roles, HasShell, Shell, ShellOf},
    utils::RcPtr,
};

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Surface<Shell: shell::Shell>(pub(crate) Rc<crate::shell::surface::Surface<Shell>>);

impl<Ctx, S> Object<Ctx> for Surface<S>
where
    Ctx: ClientContext,
    Ctx::Context: HasShell<Shell = S>,
    S: shell::Shell,
{
    fn interface(&self) -> &'static str {
        wl_surface::NAME
    }

    fn on_disconnect(&mut self, ctx: &mut Ctx) {
        let mut shell = ctx.server_context().shell().borrow_mut();
        self.0.destroy(&mut shell);
    }
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

#[interface_message_dispatch]
impl<Ctx: ClientContext, S: shell::Shell> wl_surface::RequestDispatch<Ctx> for Surface<S>
where
    Ctx::Context: HasShell<Shell = S> + HasBuffer<Buffer = S::Buffer>,
    Ctx: State<CompositorState>,
    Ctx::Object: From<wl_server::globals::DisplayObject>,
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
        &'a self,
        ctx: &'a mut Ctx,
        _object_id: u32,
        callback: wl_types::NewId,
    ) -> Self::FrameFut<'a> {
        use wl_server::globals::DisplayObject;
        async move {
            use wl_server::connection::{Entry, Objects};
            let mut objects = ctx.objects().borrow_mut();
            let entry = objects.entry(callback.0);
            if entry.is_vacant() {
                entry.or_insert(DisplayObject::Callback(wl_server::objects::Callback).into());
                let mut shell = ctx.server_context().shell().borrow_mut();
                let state = self.0.pending_mut(&mut shell);
                state.add_frame_callback(callback.0);
                Ok(())
            } else {
                Err(error::Error::IdExists(callback.0))
            }
        }
    }

    fn attach<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        buffer: wl_types::Object,
        x: i32,
        y: i32,
    ) -> Self::AttachFut<'a> {
        use wl_server::connection::Objects;
        async move {
            if x != 0 || y != 0 {
                return Err(error::Error::custom(SurfaceError {
                    object_id,
                    kind: wl_surface::enums::Error::InvalidOffset,
                }))
            }
            let objects = ctx.objects().borrow();
            let buffer_id = buffer.0;
            let Some(buffer) = objects.get(buffer.0).cloned() else { return Err(error::Error::UnknownObject(buffer_id)); };
            if buffer.interface() != wl_buffer::NAME {
                return Err(error::Error::InvalidObject(buffer_id))
            }
            let mut shell = ctx.server_context().shell().borrow_mut();
            let state = self.0.pending_mut(&mut shell);
            let buffer = buffer
                .cast::<crate::objects::Buffer<<Ctx::Context as HasBuffer>::Buffer>>()
                .unwrap();
            state.set_buffer(Some(buffer.buffer.clone()));
            Ok(())
        }
    }

    fn damage<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Self::DamageFut<'a> {
        async move {
            let mut shell = ctx.server_context().shell().borrow_mut();
            let state = self.0.pending_mut(&mut shell);
            state.damage_buffer();
            Ok(())
        }
    }

    fn damage_buffer<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Self::DamageBufferFut<'a> {
        async move {
            let mut shell = ctx.server_context().shell().borrow_mut();
            let state = self.0.pending_mut(&mut shell);
            state.damage_buffer();
            Ok(())
        }
    }

    fn commit<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::CommitFut<'a> {
        async move {
            use crate::shell::buffers::Buffer;
            let mut shell = ctx.server_context().shell().borrow_mut();
            let old_buffer = self.0.current(&shell).buffer().cloned();
            self.0
                .commit(&mut shell)
                .map_err(|msg| wl_server::error::Error::UnknownFatalError(msg))?;
            let new_buffer = self.0.current(&shell).buffer().cloned();
            if let Some(old_buffer) = old_buffer {
                let changed =
                    new_buffer.map_or(true, |new_buffer| !Rc::ptr_eq(&old_buffer, &new_buffer));
                if changed {
                    ctx.send(old_buffer.object_id(), wl_buffer::events::Release {})
                        .await?
                }
            }
            Ok(())
        }
    }

    fn set_buffer_scale<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        scale: i32,
    ) -> Self::SetBufferScaleFut<'a> {
        async move { unimplemented!() }
    }

    fn set_input_region<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        region: wl_types::Object,
    ) -> Self::SetInputRegionFut<'a> {
        async move { unimplemented!() }
    }

    fn set_opaque_region<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        region: wl_types::Object,
    ) -> Self::SetOpaqueRegionFut<'a> {
        async move { unimplemented!() }
    }

    fn set_buffer_transform<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        transform: wl_output::enums::Transform,
    ) -> Self::SetBufferTransformFut<'a> {
        async move { unimplemented!() }
    }

    fn offset<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        x: i32,
        y: i32,
    ) -> Self::OffsetFut<'a> {
        async move { unimplemented!() }
    }

    fn destroy<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        async move {
            if ctx
                .state_mut()
                .surfaces
                .remove(&object_id)
                .is_none()
            {
                panic!(
                    "Incosistent compositor state, surface {} not found",
                    object_id
                );
            }
            self.0
                .destroy(&mut ctx.server_context().shell().borrow_mut());
            ctx.send(DISPLAY_ID, wl_display::events::DeleteId { id: object_id })
                .await?;
            Ok(())
        }
    }
}

/// The reference implementation of wl_compositor
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Compositor;

use hashbrown::{HashMap, HashSet};
#[derive(Debug)]
pub struct CompositorState {
    /// Map of surface ids to outputs they are currently on
    pub(crate) surfaces:                  HashMap<u32, HashSet<RcPtr<Output>>>,
    pub(crate) output_changed:            Rc<RefCell<HashSet<u32>>>,
    pub(crate) buffer:                    Option<HashSet<u32>>,
    pub(crate) tmp_frame_callback_buffer: Option<VecDeque<u32>>,
}

impl Default for CompositorState {
    fn default() -> Self {
        Self {
            surfaces:                  HashMap::new(),
            output_changed:            Rc::new(RefCell::new(HashSet::new())),
            buffer:                    Some(HashSet::new()),
            tmp_frame_callback_buffer: Some(VecDeque::new()),
        }
    }
}

#[interface_message_dispatch]
impl<Ctx: ClientContext> wl_compositor::RequestDispatch<Ctx> for Compositor
where
    Ctx::Context: HasShell,
    Ctx: State<CompositorState>,
    Ctx::Object: From<CompositorObject<Ctx>>,
    CompositorObject<Ctx>: std::fmt::Debug,
{
    type Error = error::Error;

    type CreateRegionFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type CreateSurfaceFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn create_surface<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        _object_id: u32,
        id: wl_types::NewId,
    ) -> Self::CreateSurfaceFut<'a> {
        use wl_server::connection::Objects;

        use crate::shell::surface;
        async move {
            if ctx.objects().borrow().get(id.0).is_none() {
                // Add the id to the list of surfaces
                ctx.state_mut()
                    .surfaces
                    .insert(id.0, Default::default());
                let mut objects = ctx.objects().borrow_mut();
                let shell = ctx.server_context().shell();
                let mut shell = shell.borrow_mut();
                let surface = Rc::new(surface::Surface::default());
                let current = shell.allocate(surface::SurfaceState::new(surface.clone()));
                let pending = shell.allocate(surface::SurfaceState::new(surface.clone()));
                surface.set_current(current);
                surface.set_pending(pending);
                shell.commit(None, current);
                tracing::debug!("id {} is surface {:p}", id.0, surface);
                objects
                    .insert(id.0, CompositorObject::Surface(Surface(surface)))
                    .unwrap();
                Ok(())
            } else {
                Err(error::Error::IdExists(id.0))
            }
        }
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

impl<Ctx> Object<Ctx> for Compositor {
    fn interface(&self) -> &'static str {
        wl_compositor::NAME
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Subsurface<Ctx: ClientContext>(
    Rc<crate::shell::surface::Surface<<Ctx::Context as HasShell>::Shell>>,
)
where
    Ctx::Context: HasShell;

impl<Ctx> Object<Ctx> for Subsurface<Ctx>
where
    Ctx: ClientContext,
    Ctx::Context: HasShell,
{
    fn interface(&self) -> &'static str {
        wl_subsurface::NAME
    }
}

#[interface_message_dispatch]
impl<Ctx: ClientContext> wl_subsurface::RequestDispatch<Ctx> for Subsurface<Ctx>
where
    Ctx::Context: HasShell,
{
    type Error = error::Error;

    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type PlaceAboveFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type PlaceBelowFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetDesyncFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetPositionFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetSyncFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn set_sync<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::SetSyncFut<'a> {
        async move { unimplemented!() }
    }

    fn set_desync<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::SetDesyncFut<'a> {
        async move { unimplemented!() }
    }

    fn destroy<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        async move { unimplemented!() }
    }

    fn place_above<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        sibling: wl_types::Object,
    ) -> Self::PlaceAboveFut<'a> {
        async move { unimplemented!() }
    }

    fn place_below<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        sibling: wl_types::Object,
    ) -> Self::PlaceBelowFut<'a> {
        async move { unimplemented!() }
    }

    fn set_position<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        _object_id: u32,
        x: i32,
        y: i32,
    ) -> Self::SetPositionFut<'a> {
        async move {
            let mut shell = ctx.server_context().shell().borrow_mut();
            let role = self
                .0
                .role::<roles::Subsurface<<Ctx::Context as HasShell>::Shell>>()
                .unwrap();
            let parent = role.parent.pending_mut(&mut shell);
            let parent_antirole = parent
                .antirole_mut::<roles::SubsurfaceParent<<Ctx::Context as HasShell>::Shell>>(
                    *roles::SUBSURFACE_PARENT_SLOT,
                )
                .unwrap();
            parent_antirole
                .children
                .get_mut(role.stack_index)
                .unwrap()
                .position = Point::new(x, y);
            Ok(())
        }
    }
}

#[derive(Debug)]
pub struct Subcompositor;

impl<Ctx> Object<Ctx> for Subcompositor {
    fn interface(&self) -> &'static str {
        wl_subcompositor::NAME
    }
}
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

#[interface_message_dispatch]
impl<Ctx: ClientContext> wl_subcompositor::RequestDispatch<Ctx> for Subcompositor
where
    Ctx::Context: HasShell,
    Ctx::Object: From<SubcompositorObject<Ctx>>,
{
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
        use wl_server::connection::{Entry as _, Objects};
        tracing::debug!(
            "get_subsurface, id: {:?}, surface: {:?}, parent: {:?}",
            id,
            surface,
            parent
        );
        async move {
            let shell = ctx.server_context().shell();
            let mut objects = ctx.objects().borrow_mut();
            let mut shell = shell.borrow_mut();
            let surface_id = surface.0;
            let surface = objects
                .get(surface.0)
                .and_then(|r| r.as_ref().cast())
                .map(|sur: &Surface<ShellOf<Ctx::Context>>| sur.0.clone())
                .ok_or_else(|| {
                    Self::Error::custom(Error::BadSurface {
                        bad_surface:       surface.0,
                        subsurface_object: object_id,
                    })
                })?;
            let parent = objects
                .get(parent.0)
                .and_then(|r| r.as_ref().cast())
                .map(|sur: &Surface<ShellOf<Ctx::Context>>| sur.0.clone())
                .ok_or_else(|| {
                    Self::Error::custom(Error::BadSurface {
                        bad_surface:       parent.0,
                        subsurface_object: object_id,
                    })
                })?;
            let entry = objects.entry(id.0);
            if entry.is_vacant() {
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
                    entry.or_insert(
                        SubcompositorObject::Subsurface(Subsurface::<Ctx>(surface)).into(),
                    );
                    Ok(())
                }
            } else {
                Err(Self::Error::IdExists(id.0))
            }
        }
    }
}
