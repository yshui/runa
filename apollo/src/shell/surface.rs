use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use dyn_clone::DynClone;
use wl_server::provide_any::{request_mut, request_ref, Demand, Provider};

use super::Shell;

pub fn allocate_antirole_slot() -> u8 {
    use std::sync::atomic::AtomicU8;
    static NEXT_SLOT: AtomicU8 = AtomicU8::new(0);
    let ret = NEXT_SLOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    assert!(ret < 255);
    ret
}

pub trait Role<S: Shell>: 'static {
    fn name(&self) -> &'static str;
    // As specified by the wayland protocol, a surface can be assigned a role, then
    // have the role object destroyed. This makes the role "inactive", but the
    // surface cannot be assigned a different role. So we keep the role object
    // but "deactivate" it.
    fn is_active(&self) -> bool;
    /// Deactivate the role.
    fn deactivate(&mut self, shell: &mut S);
    /// Override how the surface the role is attached to is committed.
    fn commit_fn(&self) -> Option<fn(&mut S, &Surface<S>)> {
        None
    }
    fn provide<'a>(&'a self, _demand: &mut Demand<'a>) {}
    fn provide_mut<'a>(&'a mut self, _demand: &mut Demand<'a>) {}
}

impl<S: Shell> Provider for dyn Role<S> {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        self.provide(demand);
    }

    fn provide_mut<'a>(&'a mut self, demand: &mut Demand<'a>) {
        self.provide_mut(demand);
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
pub trait Antirole<S: Shell>: DynClone + std::fmt::Debug + 'static {
    /// The role this antirole is for.
    fn name(&self) -> &'static str;
    /// Called before the antirole is dropped during SurfaceState's destruction.
    fn destroy(&mut self, _shell: &mut S) {}
    fn provide<'a>(&'a self, demand: &mut Demand<'a>);
    fn provide_mut<'a>(&'a mut self, _demand: &mut Demand<'a>) {}
    /// Copy the incoming Antirole state to `self`. `self` is the old current
    /// antirole, and after this call, will become the new pending state.
    /// `surface` is the surface state that owns `self`.
    fn rotate_from(&mut self, incoming: &dyn Antirole<S>);
}

impl<S: Shell> Provider for dyn Antirole<S> {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        self.provide(demand);
    }

    fn provide_mut<'a>(&'a mut self, demand: &mut Demand<'a>) {
        self.provide_mut(demand);
    }
}

pub mod roles {
    use std::rc::Rc;

    use dlv_list::{Index, VecList};
    use wl_protocol::wayland::wl_subsurface;
    use wl_server::provide_any::{self, request_ref};

    use super::Antirole;
    use crate::shell::Shell;

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
    ///
    /// The role itself doesn't have any double-buffered state defined by the
    /// protocol. set_sync/set_desync/deactivate takes effect immediately,
    /// parent cannot be changed
    pub struct Subsurface<S: Shell> {
        sync:           bool,
        inherited_sync: bool,
        parent:         Rc<super::Surface<S>>,
        active:         bool,
        /// Index of this surface in the parent's states
        stack_index:    Index<S::Key>,
        /// The state used by the parent surface when child surfaces is
        /// committed but the parent surface hasn't.
        stashed_state:  S::Key,
    }
    impl<S: Shell> Subsurface<S> {
        pub fn attach(
            parent: Rc<super::Surface<S>>,
            surface: Rc<super::Surface<S>>,
            shell: &mut S,
        ) -> bool {
            if surface.role_info.borrow().is_some() {
                // already has a role
                tracing::debug!("surface {:p} already has a role", Rc::as_ptr(&surface));
                return false
            }
            // Preventing cycle creation
            if Rc::ptr_eq(&subsurface_get_root(parent.clone()), &surface) {
                tracing::debug!("cycle detected");
                return false
            }
            let parent_pending = parent.pending_mut(shell);
            let parent_antirole: &mut SubsurfaceParent<S> =
                if let Some(antirole) = parent_pending.antirole_mut(*SUBSURFACE_PARENT_SLOT) {
                    antirole
                } else {
                    tracing::debug!("add antirole to {:?}", parent.pending.get());
                    parent_pending.add_antirole(
                        *SUBSURFACE_PARENT_SLOT,
                        SubsurfaceParent::new(parent.pending.get()),
                    );
                    parent_pending
                        .antirole_mut(*SUBSURFACE_PARENT_SLOT)
                        .unwrap()
                };
            let stack_index = parent_antirole.children.push_back(surface.pending.get());
            let role = Self {
                sync: true,
                inherited_sync: true,
                parent: parent.clone(),
                active: true,
                stack_index,
                stashed_state: shell.allocate(super::SurfaceState::new(surface.clone())),
            };
            tracing::debug!(
                "attach {:p} to {:p}",
                Rc::as_ptr(&surface),
                Rc::as_ptr(&parent)
            );
            surface.set_role(role);
            true
        }
    }
    impl<S: Shell> Subsurface<S> {
        fn free_state(&mut self, shell: &mut S) {
            super::SurfaceState::clear_antiroles(self.stashed_state, shell);
            shell.deallocate(self.stashed_state);
            self.stashed_state = Default::default();
        }
    }
    impl<S: Shell> super::Role<S> for Subsurface<S> {
        fn name(&self) -> &'static str {
            wl_subsurface::v1::NAME
        }

        fn is_active(&self) -> bool {
            self.active
        }

        fn deactivate(&mut self, shell: &mut S) {
            if !self.active {
                return
            }
            self.free_state(shell);
            self.active = false;
            // Remove self from parent children list
            let parent_current_antirole: Option<&mut SubsurfaceParent<S>> = self
                .parent
                .current_mut(shell)
                .antirole_mut(*SUBSURFACE_PARENT_SLOT);
            // parent_current_antirole could be None if the role is deactivated before
            // commit is called on parent.
            if let Some(antirole) = parent_current_antirole {
                antirole.children.remove(self.stack_index);
            }
            tracing::debug!("pending state {:?}", self.parent.pending.get());
            let parent_pending_antirole: &mut SubsurfaceParent<S> = self
                .parent
                .pending_mut(shell)
                .antirole_mut(*SUBSURFACE_PARENT_SLOT)
                .expect("Subsurface parent must have a SubsurfaceParent antirole");
            parent_pending_antirole.children.remove(self.stack_index);
            // Remove self from parent's children list in stashed state
            self.parent.with_role_mut(|role: &mut Subsurface<S>| {
                if let Some(state) = shell.get_mut(role.stashed_state) {
                    let stashed_antirole: Option<&mut SubsurfaceParent<S>> =
                        state.antirole_mut(*SUBSURFACE_PARENT_SLOT);
                    if let Some(antirole) = stashed_antirole {
                        antirole.children.remove(self.stack_index);
                    }
                }
            });
        }

        fn provide<'a>(&'a self, demand: &mut provide_any::Demand<'a>) {
            demand.provide_ref(self);
        }

        fn provide_mut<'a>(&'a mut self, demand: &mut provide_any::Demand<'a>) {
            demand.provide_mut(self);
        }

        fn commit_fn(&self) -> Option<fn(&mut S, &super::Surface<S>)> {
            Some(subsurface_commit::<S>)
        }
    }
    pub fn subsurface_commit<S: Shell>(s: &mut S, surface: &super::Surface<S>) {
        let new = surface.pending.get();
        let old = surface.current.get();
        let (stack_index, stashed_state, parent) = surface
            .with_role(|r: &Subsurface<S>| (r.stack_index, r.stashed_state, r.parent.clone()))
            .expect(
                "surface has subsurface role, but subsurface role data not found in pending state",
            );
        let parent_antirole = parent
            .current(s)
            .antirole::<SubsurfaceParent<S>>(*SUBSURFACE_PARENT_SLOT)
            .unwrap();
        if *parent_antirole.children.get(stack_index).unwrap() == stashed_state {
            // Parent is using the stashed state. We are free to modify the old
            // state.
            s.commit(Some(old), new);
            s.rotate(old, new);
            surface.current.swap(&surface.pending);
        } else {
            // If parent is not using the stashed state, it must be using the current state.
            debug_assert_eq!(*parent_antirole.children.get(stack_index).unwrap(), old);
            // We need to stash away the current `current`, and use our stashed state as new
            // pending, because that one isn't being used.
            s.commit(Some(old), new);
            s.rotate(stashed_state, new);
            surface
                .with_role_mut(|r: &mut Subsurface<S>| r.stashed_state = old)
                .unwrap();
            surface.set_current(new);
            surface.set_pending(stashed_state);
        }
        // Update the state in parent's pending children list to point to the new
        // current state.
        let parent_antirole = parent
            .pending_mut(s)
            .antirole_mut::<SubsurfaceParent<S>>(*SUBSURFACE_PARENT_SLOT)
            .unwrap();
        *parent_antirole.children.get_mut(stack_index).unwrap() = new;
    }
    impl<S: Shell> Clone for Subsurface<S> {
        fn clone(&self) -> Self {
            Self {
                sync:           self.sync,
                inherited_sync: self.inherited_sync,
                parent:         self.parent.clone(),
                active:         self.active.clone(),
                stack_index:    self.stack_index,
                stashed_state:  self.stashed_state.clone(),
            }
        }
    }
    /// The anti-role of the wl_subsurface role.
    pub struct SubsurfaceParent<S: Shell> {
        /// Position of this surface, in this surface's
        /// stack. i.e. self.children[self.index] == self.
        pub(crate) index:    Index<S::Key>,
        pub(crate) children: VecList<S::Key>,
        pub(crate) changed:  bool,
        self_:               S::Key,
    }
    impl<S: Shell> std::fmt::Debug for SubsurfaceParent<S> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SubsurfaceParent")
                .field("index", &self.index)
                .field("children", &self.children)
                .field("changed", &self.changed)
                .field("self_", &self.self_)
                .finish()
        }
    }

    lazy_static::lazy_static! {
        static ref SUBSURFACE_PARENT_SLOT: u8 = super::allocate_antirole_slot();
    }
    impl<S: Shell> SubsurfaceParent<S> {
        pub fn new(surface: S::Key) -> Self {
            let mut children = VecList::new();
            let index = children.push_back(surface);
            Self {
                index,
                children,
                changed: false,
                self_: surface,
            }
        }

        /// Get the next sibling of `index` in the stack. Skip over deallocated
        /// surface states
        pub fn next_of(&self, index: Index<S::Key>, shell: &S) -> Option<S::Key> {
            let mut curr = index;
            loop {
                let next = self.children.get_next_index(curr);
                if let Some(next) = next {
                    // Safety: get_next_index returns a valid index
                    let surface = unsafe { self.children.get_unchecked(next) };
                    if shell.get(*self.children.get(next).unwrap()).is_some() {
                        break Some(*surface)
                    }
                    curr = next;
                } else {
                    break None
                }
            }
        }

        /// Get the previous sibling of `index` in the stack. Skip over
        /// deallocated surface states
        pub fn previous_of(&self, index: Index<S::Key>, shell: &S) -> Option<S::Key> {
            let mut curr = index;
            loop {
                let next = self.children.get_previous_index(curr);
                if let Some(next) = next {
                    // Safety: get_previous_index returns a valid index
                    let surface = unsafe { self.children.get_unchecked(next) };
                    if shell.get(*self.children.get(next).unwrap()).is_some() {
                        break Some(*surface)
                    }
                    curr = next;
                } else {
                    break None
                }
            }
        }

        /// Get the last child of this surface. Skip over deallocated surface
        pub fn back(&self, shell: &S) -> Option<S::Key> {
            let back_index = self.children.indices().next_back();
            back_index.and_then(|index| {
                // Safety: back_index returned by the indices iterator is a valid index
                let surface_key = unsafe { self.children.get_unchecked(index) };
                if shell.get(*surface_key).is_some() {
                    Some(*surface_key)
                } else {
                    self.previous_of(index, shell)
                }
            })
        }

        /// Get the last child of this surface. Skip over deallocated surface
        pub fn front(&self, shell: &S) -> Option<S::Key> {
            let back_index = self.children.indices().next();
            back_index.and_then(|index| {
                // Safety: back_index returned by the indices iterator is a valid index
                let surface_key = unsafe { self.children.get_unchecked(index) };
                if shell.get(*surface_key).is_some() {
                    Some(*surface_key)
                } else {
                    self.next_of(index, shell)
                }
            })
        }
    }
    impl<S: Shell> Clone for SubsurfaceParent<S> {
        fn clone(&self) -> Self {
            Self {
                children: self.children.clone(),
                index:    self.index,
                self_:    self.self_,
                changed:  false,
            }
        }
    }
    impl<S: Shell> Antirole<S> for SubsurfaceParent<S> {
        fn provide<'a>(&'a self, demand: &mut provide_any::Demand<'a>) {
            demand.provide_ref(self);
        }

        fn provide_mut<'a>(&'a mut self, demand: &mut provide_any::Demand<'a>) {
            demand.provide_mut(self);
        }

        fn rotate_from(&mut self, other: &dyn Antirole<S>) {
            let other = request_ref::<Self, _>(other).expect("mismatched antirole type");
            if other.changed {
                self.children.clone_from(&other.children);
                assert_eq!(self.index, other.index);
                *self.children.get_mut(self.index).unwrap() = self.self_;
            }
            self.changed = false;
        }

        fn name(&self) -> &'static str {
            wl_subsurface::v1::NAME
        }

        fn destroy(&mut self, shell: &mut S) {
            // Deactivate subsurface roles on all children. If the parent is
            // destroyed before the children, we need to make sure the children
            // don't try to access the parent's state during their
            // destruction.
            for child in self.children.drain() {
                let surface = shell.get_mut(child).unwrap().surface.clone();
                surface.with_role_mut(|r: &mut Subsurface<S>| {
                    // We don't call `Subsurface::deactivate` here, because that function will try
                    // to remove the subsurface from the parent's list of children, but the
                    // parent's antirole would have already be removed.
                    // We only need to free the state used by the role.
                    if r.active {
                        r.free_state(shell);
                        r.active = false;
                    }
                });
            }
        }
    }
    pub fn subsurface_get_root<S: Shell>(
        mut surface: Rc<super::Surface<S>>,
    ) -> Rc<super::Surface<S>> {
        loop {
            let parent = surface.with_role(|r: &Subsurface<S>| r.parent.clone());
            if let Some(parent) = parent {
                surface = parent
            } else {
                break surface
            }
        }
    }

    /// Double ended iterator for iterating over a surface and its subsurfaces.
    /// The forward order is bottom to top. This uses the committed states of
    /// the surfaces.
    ///
    /// You need to be careful to not call surface commit while iterating. It
    /// won't cause any memory unsafety, but it could cause non-sensical
    /// results, panics, or infinite loops. Since commit calls
    /// [`Shell::commit`], you could check for this case there.
    pub fn subsurface_iter<'a, S: Shell>(
        root: S::Key,
        s: &'a S,
    ) -> impl Iterator<Item = S::Key> + 'a {
        struct SubsurfaceIter<'a, S: Shell> {
            shell:    &'a S,
            head:     [S::Key; 2],
            is_empty: bool,
        }
        #[repr(usize)]
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Direction {
            Forward  = 0,
            Backward = 1,
        }
        impl<'a, S: Shell> SubsurfaceIter<'a, S> {
            /// Advance the front or back pointer to the next surface in the
            /// stack. The resulting surface state might or might
            /// not have been destroyed.
            fn advance(&mut self, direction: Direction) {
                if self.is_empty {
                    return
                }
                if self.head[0] == self.head[1] {
                    self.is_empty = true;
                    return
                }
                let ret = self.shell.get(self.head[direction as usize]).unwrap();
                if let Some(p) = ret.antirole::<SubsurfaceParent<S>>(*SUBSURFACE_PARENT_SLOT) {
                    // inner node
                    let next_child = if direction == Direction::Forward {
                        p.next_of(p.index, self.shell)
                    } else {
                        p.previous_of(p.index, self.shell)
                    };
                    if let Some(next_child) = next_child {
                        // find the bottom/top most surface in `next_child`'s descendants, depends
                        // on direction
                        self.head[direction as usize] = find_end(next_child, self.shell, direction);
                        return
                    }
                }
                // current surface is the end of its stack. this includes leaf nodes.
                let mut curr = ret;
                self.head[direction as usize] = loop {
                    let (stack_index, parent_key) = curr
                        .surface
                        .with_role(|r: &Subsurface<S>| (r.stack_index, r.parent.current.get()))
                        .unwrap();
                    let parent = self.shell.get(parent_key).unwrap();
                    let parent_antirole = parent
                        .antirole::<SubsurfaceParent<S>>(*SUBSURFACE_PARENT_SLOT)
                        .unwrap();
                    let next_child = if direction == Direction::Forward {
                        parent_antirole.next_of(stack_index, self.shell)
                    } else {
                        parent_antirole.previous_of(stack_index, self.shell)
                    };
                    if let Some(next_child) = next_child {
                        if next_child != parent_key {
                            // the next surface in the parent's stack is not the parent itself. we
                            // need to find the bottom most surface in `next`'s descendants.
                            break find_end(next_child, self.shell, direction)
                        } else {
                            // the next surface in the parent's stack is the parent itself.
                            break next_child
                        }
                    }
                    curr = parent;
                };
            }

            fn next_impl(&mut self, direction: Direction) -> Option<S::Key> {
                if self.is_empty {
                    return None
                }
                let ret = self.head[direction as usize];
                self.advance(direction);
                Some(ret)
            }
        }
        impl<'a, S: Shell> Iterator for SubsurfaceIter<'a, S> {
            type Item = S::Key;

            fn next(&mut self) -> Option<Self::Item> {
                self.next_impl(Direction::Forward)
            }
        }
        impl<'a, S: Shell> DoubleEndedIterator for SubsurfaceIter<'a, S> {
            fn next_back(&mut self) -> Option<Self::Item> {
                self.next_impl(Direction::Backward)
            }
        }
        /// Find the bottom most surface in `root`'s stack. The returned surface
        /// might or might not have been destroyed.
        fn find_end<S: Shell>(root: S::Key, s: &S, direction: Direction) -> S::Key {
            let mut curr = root;
            while let Some(p) = s
                .get(curr)
                .and_then(|p| p.antirole::<SubsurfaceParent<S>>(*SUBSURFACE_PARENT_SLOT))
            {
                let end = if direction == Direction::Forward {
                    p.front(s)
                } else {
                    p.back(s)
                };
                if let Some(end) = end {
                    if end == curr {
                        break
                    }
                    curr = end;
                } else {
                    break
                }
            }
            curr
        }
        let ret = SubsurfaceIter {
            shell:    s,
            head:     [
                find_end(root, s, Direction::Forward),
                find_end(root, s, Direction::Backward),
            ],
            is_empty: false,
        };
        ret
    }
}

#[derive(Copy, Clone, Debug)]
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
    pub destroyed: bool,
}

pub struct SurfaceVTable<S: Shell> {
    commit: Option<fn(&mut S, &Surface<S>)>,
}

impl<S: Shell> std::fmt::Debug for SurfaceVTable<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("SurfaceVTable")
            .field("commit", &self.commit.map(|f| f as *const ()))
            .finish()
    }
}
impl<S: Shell> Default for SurfaceVTable<S> {
    fn default() -> Self {
        Self { commit: None }
    }
}
impl<S: Shell> Clone for SurfaceVTable<S> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S: Shell> Copy for SurfaceVTable<S> {}

/// To support the current state and the pending state double buffering, the
/// surface state must be deep cloneable.
pub struct SurfaceState<S: Shell> {
    /// A set of flags that can be mutate even when the state is the current
    /// state of a surface.
    flags:          Cell<SurfaceFlags>,
    surface:        Rc<Surface<S>>,
    frame_callback: Vec<u32>,
    /// A list of antiroles, ordered by their names.
    antiroles:      Vec<Option<Box<dyn Antirole<S>>>>,
    buffer:         Option<Rc<dyn super::buffers::Buffer>>,
    pub data:       S::Data,
}

impl<S: Shell> std::fmt::Debug for SurfaceState<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurfaceState")
            .field("flags", &self.flags)
            .field("surface", &self.surface)
            .field("frame_callback", &self.frame_callback)
            .field("antiroles", &"…")
            .field("buffer", &"…")
            .field("data", &"…")
            .finish()
    }
}

impl<S: Shell> SurfaceState<S> {
    pub fn new(surface: Rc<Surface<S>>) -> Self {
        Self {
            flags: Cell::new(SurfaceFlags { destroyed: false }),
            surface,
            frame_callback: Vec::new(),
            antiroles: Vec::new(),
            buffer: None,
            data: S::Data::default(),
        }
    }
}

impl<S: Shell> SurfaceState<S> {
    #[inline]
    pub fn antirole<T: Antirole<S>>(&self, slot: u8) -> Option<&T> {
        if slot as usize >= self.antiroles.len() {
            return None
        }
        // Safety: we checked slot is in range
        unsafe { self.antiroles.get_unchecked(slot as usize) }
            .as_ref()
            .and_then(|a| request_ref(a.as_ref()))
    }

    #[inline]
    pub fn antirole_mut<T: Antirole<S>>(&mut self, slot: u8) -> Option<&mut T> {
        if slot as usize >= self.antiroles.len() {
            tracing::debug!(
                "antirole_mut: slot {} out of range {}",
                slot,
                self.antiroles.len()
            );
            return None
        }
        // Safety: we checked slot is in range
        unsafe { self.antiroles.get_unchecked_mut(slot as usize) }
            .as_mut()
            .and_then(|a| request_mut(a.as_mut()))
    }

    #[inline]
    pub fn add_antirole<T: Antirole<S>>(&mut self, slot: u8, antirole: T) {
        if self.antirole::<T>(slot).is_some() {
            panic!("antirole already exists")
        }
        tracing::debug!(
            "add_antirole: slot {}, type: {}",
            slot,
            std::any::type_name::<T>()
        );
        while self.antiroles.len() <= slot as usize {
            self.antiroles.push(None)
        }
        // Safety: we made sure self.antiroles.len() > slot
        *unsafe { self.antiroles.get_unchecked_mut(slot as usize) } = Some(Box::new(antirole));
        tracing::debug!(
            "add_antirole: done {:?}, len {}",
            self.antiroles[slot as usize],
            self.antiroles.len()
        );
    }

    #[inline]
    pub fn remove_antirole(&mut self, slot: u8) -> Option<Box<dyn Antirole<S>>> {
        if slot >= self.antiroles.len() as u8 {
            return None
        }
        // Safety: we checked slot is in range
        unsafe { self.antiroles.get_unchecked_mut(slot as usize) }.take()
    }

    /// Clear the antiroles on surface `self_`. Calls `Antirole::destroy` on the
    /// antiroles. This takes a key instead of a `&mut self`, because
    /// `&mut self` would borrow `&mut S`, which we also need.
    #[inline]
    fn clear_antiroles(self_: S::Key, shell: &mut S) {
        while let Some(entry) = shell.get_mut(self_).unwrap().antiroles.pop() {
            if let Some(mut antirole) = entry {
                antirole.destroy(shell)
            }
        }
    }

    #[inline]
    pub fn flags(&self) -> SurfaceFlags {
        self.flags.get()
    }

    #[inline]
    pub fn set_flags(&self, flags: SurfaceFlags) {
        self.flags.set(flags);
    }

    #[inline]
    pub fn surface(&self) -> &Rc<Surface<S>> {
        &self.surface
    }

    #[inline]
    pub fn rotate_from(&mut self, pending: &Self) {
        self.flags = pending.flags.clone();
        debug_assert!(Rc::ptr_eq(&self.surface, &pending.surface));
        self.frame_callback.clone_from(&pending.frame_callback);
        self.buffer = pending.buffer.clone();
        while self.antiroles.len() < pending.antiroles.len() {
            self.antiroles.push(None)
        }
        if self.antiroles.len() > pending.antiroles.len() {
            self.antiroles.truncate(pending.antiroles.len())
        }
        for (a, b) in self.antiroles.iter_mut().zip(pending.antiroles.iter()) {
            if let Some(a2) = a {
                if let Some(b2) = b {
                    a2.rotate_from(b2.as_ref());
                } else {
                    *a = None;
                }
            } else {
                if let Some(b) = b {
                    *a = Some(dyn_clone::clone_box(b.as_ref()));
                }
            }
        }
    }
}

struct RoleInfo<S: Shell> {
    vtable: SurfaceVTable<S>,
    role:   Box<dyn Role<S>>,
}

impl<S: Shell> std::fmt::Debug for RoleInfo<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleInfo")
            .field("vtable", &self.vtable)
            .field("role", &self.role.name())
            .finish()
    }
}

pub struct Surface<S: super::Shell> {
    /// The current state of the surface. Once a state is committed to current,
    /// it should not be modified.
    current:   Cell<S::Key>,
    /// The pending state of the surface, this will be moved to [`current`] when
    /// commit is called
    pending:   Cell<S::Key>,
    role_info: RefCell<Option<RoleInfo<S>>>,
}

impl<S: Shell> Default for Surface<S>
where
    S::Key: Default,
{
    fn default() -> Self {
        Self {
            current:   Default::default(),
            pending:   Default::default(),
            role_info: Default::default(),
        }
    }
}

impl<S: Shell> std::fmt::Debug for Surface<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Surface")
            .field("current", &self.current)
            .field("pending", &self.pending)
            .field("vtable", &self.role_info)
            .finish()
    }
}

impl<S: Shell> Drop for Surface<S> {
    fn drop(&mut self) {
        assert!(
            self.current == Default::default() &&
                self.pending == Default::default() &&
                self.role_info.borrow().is_none(),
            "Surface must be destroyed with Surface::destroy"
        );
    }
}

impl<S: Shell> Surface<S> {
    pub fn new(current: S::Key, pending: S::Key) -> Self {
        Self {
            current:   Cell::new(current),
            pending:   Cell::new(pending),
            role_info: Default::default(),
        }
    }

    pub fn commit(&self, shell: &mut S) {
        let pending = self.pending.get();
        let current = self.current.get();
        if let Some(commit) = self
            .role_info
            .borrow()
            .as_ref()
            .and_then(|r| r.vtable.commit)
        {
            // commit operation is overridden
            (commit)(shell, self);
        } else {
            {
                shell.commit(Some(current), pending);
                shell.rotate(current, pending);
            }
            self.swap_states();
        }
    }

    pub fn swap_states(&self) {
        self.current.swap(&self.pending);
    }

    pub fn set_current(&self, key: S::Key) {
        self.current.set(key);
    }

    pub fn set_pending(&self, key: S::Key) {
        self.pending.set(key);
    }

    pub fn pending_mut<'a>(&self, shell: &'a mut S) -> &'a mut SurfaceState<S> {
        shell.get_mut(self.pending.get()).unwrap()
    }

    pub fn pending<'a>(&self, shell: &'a S) -> &'a SurfaceState<S> {
        shell.get(self.pending.get()).unwrap()
    }

    pub fn current<'a>(&self, shell: &'a S) -> &'a SurfaceState<S> {
        shell.get(self.current.get()).unwrap()
    }

    pub fn current_mut<'a>(&self, shell: &'a mut S) -> &'a mut SurfaceState<S> {
        shell.get_mut(self.current.get()).unwrap()
    }

    pub fn has_role(&self) -> bool {
        self.role_info.borrow().is_some()
    }

    pub fn with_role<T: Role<S>, R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        self.role_info
            .borrow()
            .as_ref()
            .and_then(|r| request_ref(r.role.as_ref()))
            .map(f)
    }

    pub fn with_role_mut<T: Role<S>, R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        self.role_info
            .borrow_mut()
            .as_mut()
            .and_then(|r| request_mut(r.role.as_mut()))
            .map(f)
    }

    pub fn destroy(&self, shell: &mut S) {
        tracing::debug!(
            "Destroying surface, (current: {:?}, pending: {:?})",
            self.current.get(),
            self.pending.get()
        );
        self.deactivate_role(shell);
        SurfaceState::clear_antiroles(self.current.get(), shell);
        SurfaceState::clear_antiroles(self.pending.get(), shell);
        shell.deallocate(self.current.get());
        shell.deallocate(self.pending.get());
        self.current.set(Default::default());
        self.pending.set(Default::default());
        *self.role_info.borrow_mut() = Default::default();
    }

    pub fn is_destroyed(&self) -> bool {
        self.current.get() == S::Key::default()
    }

    pub fn deactivate_role(&self, shell: &mut S) {
        self.role_info
            .borrow_mut()
            .as_mut()
            .map(|r| r.role.deactivate(shell));
    }

    pub fn clear_damage(&self) {
        todo!()
    }

    pub fn set_role<T: Role<S>>(&self, role: T) {
        let mut role_info = self.role_info.borrow_mut();
        *role_info = Some(RoleInfo {
            vtable: SurfaceVTable {
                commit: role.commit_fn(),
            },
            role:   Box::new(role),
        });
    }
}
