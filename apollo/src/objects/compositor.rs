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
use std::{cell::RefCell, future::Future, marker::PhantomData, rc::Rc};

use derivative::Derivative;
use wl_common::{interface_message_dispatch, utils::geometry::Point};
use wl_protocol::wayland::{
    wl_compositor::v5 as wl_compositor, wl_output::v4 as wl_output,
    wl_subcompositor::v1 as wl_subcompositor, wl_subsurface::v1 as wl_subsurface,
    wl_surface::v5 as wl_surface,
};
use wl_server::{
    connection::Connection,
    error,
    objects::InterfaceMeta,
    provide_any::{request_ref, Demand},
    Extra,
};

use crate::shell::{surface::roles, Shell};

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Surface<S: Shell, Ctx>(
    pub(crate) Rc<crate::shell::surface::Surface<S>>,
    PhantomData<Ctx>,
);

impl<S: Shell, Ctx: Connection> InterfaceMeta<Ctx> for Surface<S, Ctx>
where
    Ctx::Context: Extra<RefCell<S>>,
{
    fn interface(&self) -> &'static str {
        wl_surface::NAME
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }

    fn on_drop(&self, ctx: &Ctx) {
        let shell: &RefCell<S> = ctx.server_context().extra();
        self.0.destroy(&mut *shell.borrow_mut());
    }
}

#[interface_message_dispatch]
impl<S: Shell, Ctx: Connection> wl_surface::RequestDispatch<Ctx> for Surface<S, Ctx>
where
    Ctx::Context: Extra<RefCell<S>>,
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
        object_id: u32,
        callback: wl_types::NewId,
    ) -> Self::FrameFut<'a> {
        async move { unimplemented!() }
    }

    fn attach<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        buffer: wl_types::Object,
        x: i32,
        y: i32,
    ) -> Self::AttachFut<'a> {
        async move { unimplemented!() }
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
        async move { unimplemented!() }
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
        async move { unimplemented!() }
    }

    fn commit<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::CommitFut<'a> {
        async move {
            let shell: &RefCell<S> = ctx.server_context().extra();
            self.0.commit(&mut shell.borrow_mut());
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
        async move { unimplemented!() }
    }
}

/// The reference implementation of wl_compositor
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Compositor<S>(PhantomData<S>);

#[interface_message_dispatch]
impl<S: Shell, Ctx: Connection> wl_compositor::RequestDispatch<Ctx> for Compositor<S>
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
        use wl_server::connection::{Entry as _, Objects};

        use crate::shell::surface;
        async move {
            let mut objects = ctx.objects().borrow_mut();
            let entry = objects.entry(id.0);
            if entry.is_vacant() {
                let shell: &RefCell<S> = ctx.server_context().extra();
                let mut shell = shell.borrow_mut();
                let surface = Rc::new(surface::Surface::default());
                let current = shell.allocate(surface::SurfaceState::new(surface.clone()));
                let pending = shell.allocate(surface::SurfaceState::new(surface.clone()));
                surface.set_current(current);
                surface.set_pending(pending);
                shell.commit(None, current);
                tracing::debug!("id {} is surface {:p}", id.0, surface);
                entry.or_insert(Surface(surface, PhantomData::<Ctx>));
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
impl<S: Shell> Compositor<S> {
    pub fn new() -> Self {
        Compositor(PhantomData)
    }
}

impl<S: 'static, Ctx> InterfaceMeta<Ctx> for Compositor<S> {
    fn interface(&self) -> &'static str {
        wl_compositor::NAME
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}

pub struct Subsurface<S: Shell>(Rc<crate::shell::surface::Surface<S>>);

impl<S: Shell, Ctx> InterfaceMeta<Ctx> for Subsurface<S> {
    fn interface(&self) -> &'static str {
        wl_subsurface::NAME
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}

#[interface_message_dispatch]
impl<S: Shell, Ctx: Connection> wl_subsurface::RequestDispatch<Ctx> for Subsurface<S>
where
    Ctx::Context: Extra<RefCell<S>>,
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
            let mut shell = ctx.server_context().extra().borrow_mut();
            let role = self.0.role::<roles::Subsurface<S>>().unwrap();
            let parent = role.parent.pending_mut(&mut shell);
            let parent_antirole = parent
                .antirole_mut::<roles::SubsurfaceParent<S>>(*roles::SUBSURFACE_PARENT_SLOT)
                .unwrap();
            parent_antirole.children.get_mut(role.stack_index).unwrap().position = Point::new(x, y);
            Ok(())
        }
    }
}

pub struct Subcompositor<S: Shell>(PhantomData<S>);

impl<Sh: Shell> Subcompositor<Sh> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<S: Shell, Ctx> InterfaceMeta<Ctx> for Subcompositor<S> {
    fn interface(&self) -> &'static str {
        wl_subcompositor::NAME
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
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
impl<S: Shell, Ctx: Connection> wl_subcompositor::RequestDispatch<Ctx> for Subcompositor<S>
where
    Ctx::Context: Extra<RefCell<S>>,
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
            let shell: &RefCell<S> = ctx.server_context().extra();
            let mut objects = ctx.objects().borrow_mut();
            let mut shell = shell.borrow_mut();
            let surface_id = surface.0;
            let surface = objects
                .get(surface.0)
                .and_then(|r| request_ref(r.as_ref()))
                .map(|sur: &Surface<S, Ctx>| sur.0.clone())
                .ok_or_else(|| {
                    Self::Error::custom(Error::BadSurface {
                        bad_surface:       surface.0,
                        subsurface_object: object_id,
                    })
                })?;
            let parent = objects
                .get(parent.0)
                .and_then(|r| request_ref(r.as_ref()))
                .map(|sur: &Surface<S, Ctx>| sur.0.clone())
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
                    entry.or_insert(Subsurface(surface));
                    Ok(())
                }
            } else {
                Err(Self::Error::IdExists(id.0))
            }
        }
    }
}
