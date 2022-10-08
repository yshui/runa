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
    rc::Rc,
};

use wl_common::{interface_message_dispatch, Infallible};
use wl_protocol::wayland::{wl_compositor, wl_subcompositor::v1 as wl_subcompositor};
use wl_server::{
    connection::Connection,
    error,
    objects::InterfaceMeta,
    provide_any::{Demand, Provider},
    server::{Globals, Server, ServerBuilder},
};
/// The reference implementation of wl_compositor
#[derive(Debug)]
pub struct Compositor;

#[interface_message_dispatch]
impl<Ctx: 'static> wl_compositor::v5::RequestDispatch<Ctx> for Compositor {
    type Error = error::Error;

    type CreateRegionFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type CreateSurfaceFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn create_surface<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
    ) -> Self::CreateSurfaceFut<'a> {
        async { unimplemented!() }
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
impl Compositor {
    pub fn new() -> Self {
        Compositor
    }

    pub async fn handle_events<Ctx: 'static>(
        _ctx: &Ctx,
        _slot: usize,
        _event: &'static str,
    ) -> Result<(), error::Error> {
        Ok(())
    }

    pub fn init_server<Ctx: ServerBuilder>(
        server: &mut Ctx,
    ) -> Result<(), wl_common::Infallible> {
        server
            .global(crate::globals::Compositor::default())
            .event_slot("wl_compositor");
        Ok(())
    }
}

pub trait Role {
    fn name(&self) -> &'static str;
    // As specified by the wayland protocol, a surface can be assigned a role, then
    // have the role object destroyed. This makes the role "inactive", but the
    // surface cannot be assigned a different role. So we keep the role object
    // but "deactivate" it.
    fn is_active(&self) -> bool;
    fn clone_boxed(&self) -> Box<dyn Role>;
    fn deactivate(&mut self);
    /// Copy from another role state on the same surface. Used to
    /// opportunisticly commit pending state in-place when the current state
    /// is not used by anyone else.
    ///
    /// # Panic
    ///
    /// Since surfaces cannot change roles, failure to request a `&Self` from
    /// `other` is a program error, and is thus fatal.
    fn copy_from(&mut self, other: &(dyn Role + 'static));
    fn provide<'a>(&'a self, demand: &mut Demand<'a>);
}

impl Provider for dyn Role {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        self.provide(demand);
    }
}

impl Clone for Box<dyn Role> {
    fn clone(&self) -> Self {
        self.clone_boxed()
    }
}

/// An anti-role is a counter part of a role in another surface. Some roles on a
/// surface require additional state to be stored on another surface that
/// doesn't have the role. For example, a subsurface has a parent surface, so
/// the parent surface, although not necessarily having a role, need to store
/// which surfaces it has as its children. This information is stored in its
/// surface anti-role. An anti-role is still part of the surface's state, so its
/// update has to go through pending -> commit process, like any other updates.
///
/// A surface can have multiple anti-roles, they are divided by the
/// corresponding role, and only one anti-role per role can be stored in a
/// surface at a time.
pub trait AntiRole {}

pub mod roles {
    use std::rc::{Rc, Weak};

    use wl_server::provide_any::{self, request_ref};

    /// The wl_subsurface role.
    ///
    /// # Note about cache and pending states
    ///
    /// A surface normally has a pending and a current state. Changes are
    /// applied to the pending state first, then commited to the current
    /// state when wl_surface.commit is called.
    ///
    /// Subsurfaces has one more state - the cached state. This state exists if
    /// the subsurface is in synchronized mode. In sync mode, commit applies
    /// pending state to a cached state, and the cached state is applied to
    /// current state when the parent calls commit. Another way of
    /// looking at it, is the child has a "invisible" current state, and the
    /// parent has a whole copy of the states of its subtree (IOW a shadow
    /// subtree). On commit, a surface will copy its children's "invisible"
    /// current state into its own current state. This way we can keep the
    /// semantic of "commit", i.e. the commit code doesn't have to know
    /// anything about roles. And also localize/encapsulate where the state need
    /// to be stored, so we don't need a global cache queue or transactions,
    /// etc.
    ///
    /// To avoid excessive copies, this shadow tree can be implemented with COW.
    /// The parent holds a reference of its children's states, when a
    /// child's state is updated, it's not done in place.
    ///
    /// A complication is when a child is destroyed, either by destroying the
    /// surface or deactivating its role, it's immediately removed, without
    /// going through the pending or the cached state. We could do this by
    /// holding an indirect reference to the child's state, and allow it to
    /// "dangle" when the child is removed.
    pub struct Subsurface {
        sync:           bool,
        inherited_sync: bool,
        parent:         Option<Weak<super::Surface>>,
        active:         bool,
        /// Index of this surface in the parent's stack
        stack_index:    dlv_list::Index<ChildSurface>,
    }
    impl super::Role for Subsurface {
        fn name(&self) -> &'static str {
            "wl_subsurface"
        }

        fn is_active(&self) -> bool {
            self.active
        }

        fn deactivate(&mut self) {
            self.active = false;
        }

        fn clone_boxed(&self) -> Box<dyn super::Role + 'static> {
            Box::new(Self {
                sync:           self.sync,
                inherited_sync: self.inherited_sync,
                parent:         self.parent.clone(),
                active:         self.active,
                stack_index:    self.stack_index,
            })
        }

        fn copy_from(&mut self, other: &(dyn super::Role + 'static)) {
            let other = request_ref::<Self, _>(other).unwrap();
            self.sync = other.sync;
            self.inherited_sync = other.inherited_sync;
            self.parent = other.parent.clone();
            self.active = other.active;
            self.stack_index = other.stack_index;
        }

        fn provide<'a>(&'a self, demand: &mut provide_any::Demand<'a>) {
            demand.provide_ref(self);
        }
    }
    /// The anti-role of the wl_subsurface role.
    pub struct SubsurfaceParent {
        children: dlv_list::VecList<ChildSurface>,
    }

    pub enum ChildSurface {
        /// Update is synced, we keep a reference to the state, which is COW
        /// updated, so when the child commits, we still have the old state.
        /// A reference to the Surface is kept too, so we can move from Synced
        /// state to Desynced state.
        Synced(Rc<super::SurfaceState>, Rc<super::Surface>),
        /// Update is desynced, we keep a reference to the surface, so we can
        /// always access the latest state.
        Desynced(Rc<super::Surface>),
    }
    impl ChildSurface {
        pub fn to_synced(&mut self) {
            match self {
                ChildSurface::Synced(_, _) => {},
                ChildSurface::Desynced(surface) => {
                    *self =
                        ChildSurface::Synced(surface.current.clone().into_inner(), surface.clone());
                },
            }
        }

        pub fn to_desynced(&mut self) {
            match self {
                ChildSurface::Synced(_, surface) => {
                    *self = ChildSurface::Desynced(surface.clone());
                },
                ChildSurface::Desynced(_) => {},
            }
        }
    }
}

pub struct SurfaceFlags {
    /// Whether the surface is destroyed, pointing to the same `Cell` in the
    /// corresponding pending state. The only bit of the current state that is
    /// changeable.
    ///
    /// This is needed, because wayland spec demand a surface to be removed
    /// immediately from the surface tree when it is destroyed, however by
    /// our rules we cannot update the tree until commit, and in sync mode
    /// subsurface's case it's even more complicated, since all of the
    /// surface's anscesters must commit.
    ///
    /// So we keep a flag here, which is checked to skip destroyed surfaces
    /// while traversing the tree.
    destroyed: bool,
    /// Whether this surface has been rendered since its commit. We use this to
    /// avoid mutate the damage region stored in the state.
    painted:   bool,
}

/// To support the current state and the pending state double buffering, the
/// surface state must be deep cloneable.
pub struct SurfaceState {
    /// A set of flags that can be mutate even when the state is the current
    /// state of a surface.
    flags:          Cell<SurfaceFlags>,
    frame_callback: u32,
    role:           Option<Box<RefCell<dyn Role>>>,
    antiroles:      Vec<Box<dyn AntiRole>>,
}

pub struct Surface {
    /// The current state of the surface
    ///
    /// Yes, `Cell<Rc<Surface>>` is right. This is to enble COW update of the
    /// state. Once state change is commited, it's wrapped in Rc and swapped
    /// into the Cell. The old state will still be kept alive, if it's in
    /// use by some surface. Once the state is commited to current, it
    /// can no longer be modified, besides a small number of boolean flags.
    /// EXCEPT, when `current` is the only holder of the reference. In this
    /// case, pending state is copied into it instead, to avoid allocation.
    current: RefCell<Rc<SurfaceState>>,
    /// The pending state of the surface, this will be moved to [`current`] when
    /// commit is called
    pending: RefCell<SurfaceState>,
}

impl Surface {
    fn commit(&mut self) {}
}

impl InterfaceMeta for Compositor {
    fn interface(&self) -> &'static str {
        wl_compositor::v5::NAME
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
