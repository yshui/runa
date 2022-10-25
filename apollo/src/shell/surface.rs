use std::{
    cell::{Cell, Ref, RefCell, RefMut},
    collections::LinkedList,
    rc::Rc,
};

use dlv_list::VecList;
use dyn_clone::DynClone;
use wl_server::provide_any::{request_mut, request_ref, Demand, Provider};

use super::Shell;
pub trait Role: DynClone + 'static {
    fn name(&self) -> &'static str;
    // As specified by the wayland protocol, a surface can be assigned a role, then
    // have the role object destroyed. This makes the role "inactive", but the
    // surface cannot be assigned a different role. So we keep the role object
    // but "deactivate" it.
    fn active(&self) -> bool;
    /// Deactivate the role. This is done through a shared reference, because
    /// when the role object is destroyed, the role is deactivated immediately,
    /// without needing to call commit. So this should be callable from the
    /// shared [`Surface::current`] state.
    fn deactivate(&self);
    /// Copy from another role state on the same surface.
    fn rotate_from(&mut self, other: &dyn Role);
    fn provide<'a>(&'a self, demand: &mut Demand<'a>);
}

impl Provider for dyn Role {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        self.provide(demand);
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
pub trait Antirole: DynClone + 'static {
    /// The role this anti-role is for.
    fn name(&self) -> &'static str;
    fn provide<'a>(&'a self, demand: &mut Demand<'a>);
    fn rotate_from(&mut self, other: &dyn Antirole);
}

impl Provider for dyn Antirole {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        self.provide(demand);
    }
}

pub mod roles {
    use std::{cell::Cell, rc::Rc};

    use dlv_list::VecList;
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
    pub struct Subsurface<S: Shell> {
        sync:           bool,
        inherited_sync: bool,
        parent:         Rc<super::Surface<S>>,
        active:         Cell<bool>,
        /// Index of this surface in the parent's stack
        stack_index:    dlv_list::Index<S::Key>,
        /// The state used by the parent surface when child surfaces is
        /// committed but the parent surface hasn't.
        stashed_state:  S::Key,
    }
    impl<S: Shell> super::Role for Subsurface<S> {
        fn name(&self) -> &'static str {
            wl_subsurface::v1::NAME
        }

        fn active(&self) -> bool {
            self.active.get()
        }

        fn deactivate(&self) {
            self.active.set(false);
        }

        fn rotate_from(&mut self, other: &(dyn super::Role + 'static)) {
            let other = request_ref::<Self, _>(other).unwrap();
            self.clone_from(other);
        }

        fn provide<'a>(&'a self, demand: &mut provide_any::Demand<'a>) {
            demand.provide_ref(self);
        }
    }
    pub fn subsurface_commit<S: Shell>(s: &mut S, surface: &super::Surface<S>) {
        let new = surface.pending.get();
        let old = surface.current.get();
        let new_state = s.get(new).unwrap();
        let new_role: &Subsurface<S> = new_state.role().expect(
            "surface has subsurface role, but subsurface role data not found in pending state",
        );
        let stashed_state = new_role.stashed_state;
        let parent = new_role.parent.pending(s);
        let parent_antirole = parent.antirole::<SubsurfaceParent<S>>().unwrap();
        if *parent_antirole.children.get(new_role.stack_index).unwrap() == stashed_state {
            // Parent is using the stashed state. We are free to modify the old
            // state.
            s.commit(Some(old), new);
            s.rotate(old, new);
            surface.current.swap(&surface.pending);
        } else {
            // If parent is not using the stashed state, it must be using the current state.
            debug_assert_eq!(
                *parent_antirole.children.get(new_role.stack_index).unwrap(),
                old
            );
            // We need to stash away the current `current`, and use our stashed state as new
            // pending, because that one isn't being used.
            s.commit(Some(old), new);
            s.rotate(stashed_state, new);
            let new_state = s.get_mut(new).unwrap();
            let new_role = new_state.role_mut::<Subsurface<S>>().unwrap();
            new_role.stashed_state = old;
            surface.set_pending(stashed_state);
        }
    }
    impl<S: Shell> Clone for Subsurface<S> {
        fn clone(&self) -> Self {
            Self {
                sync:           self.sync,
                inherited_sync: self.inherited_sync,
                parent:         self.parent.clone(),
                active:         self.active.clone(),
                stack_index:    self.stack_index,
                stashed_state:  self.stashed_state,
            }
        }
    }
    /// The anti-role of the wl_subsurface role.
    pub struct SubsurfaceParent<S: Shell> {
        /// Position of this surface, in this surface's
        /// stack. i.e. self.children[self.index] == self.
        pub(crate) index:    dlv_list::Index<S::Key>,
        pub(crate) children: VecList<S::Key>,
        pub(crate) changed:  bool,
    }
    impl<S: Shell> Clone for SubsurfaceParent<S> {
        fn clone(&self) -> Self {
            Self {
                children: self.children.clone(),
                index:    self.index,
                changed:  false,
            }
        }
    }
    impl<S: Shell> Antirole for SubsurfaceParent<S> {
        fn provide<'a>(&'a self, demand: &mut provide_any::Demand<'a>) {
            demand.provide_ref(self);
        }

        fn rotate_from(&mut self, other: &dyn Antirole) {
            let other = request_ref::<Self, _>(other).expect("mismatched antirole type");
            if other.changed {
                self.children = other.children.clone(); // Use clone_from
                self.index = other.index;
            }
            self.changed = false;
        }

        fn name(&self) -> &'static str {
            wl_subsurface::v1::NAME
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
    ) -> impl Iterator<Item = &'a super::SurfaceState<S>> + 'a {
        struct SubsurfaceIter<'a, S: Shell> {
            shell: &'a S,
            head:  S::Key,
            tail:  S::Key,
            empty: bool,
        }
        impl<'a, S: Shell> Iterator for SubsurfaceIter<'a, S> {
            type Item = &'a super::SurfaceState<S>;

            fn next(&mut self) -> Option<Self::Item> {
                if self.empty {
                    return None
                }
                let ret = self.shell.get(self.head).unwrap();
                if self.head == self.tail {
                    self.empty = true;
                    return Some(ret)
                }
                if let Some(p) = ret.antirole::<SubsurfaceParent<S>>() {
                    // inner node
                    if let Some(next) = p.children.get_next_index(p.index) {
                        // find the next surface in current surface's stack
                        let next_child = *p.children.get(next).unwrap(); // use unwrap_unchecked
                                                                         // find the bottom most surface in `next_child`'s descendants
                        self.head = find_first(next_child, self.shell);
                        return Some(ret)
                    }
                }
                // current surface is the end of its stack. this includes leaf nodes.
                let mut curr = ret;
                self.head = loop {
                    let role = curr.role::<Subsurface<S>>().unwrap();
                    let parent_key = role.parent.current.get();
                    let parent = self.shell.get(parent_key).unwrap();
                    let parent_antirole = parent.antirole::<SubsurfaceParent<S>>().unwrap();
                    if let Some(next) = parent_antirole.children.get_next_index(role.stack_index) {
                        let next = *parent_antirole.children.get(next).unwrap();
                        if next != parent_key {
                            // the next surface in the parent's stack is not the parent itself. we
                            // need to find the bottom most surface in `next`'s descendants.
                            break find_first(next, self.shell)
                        } else {
                            // the next surface in the parent's stack is the parent itself.
                            break next
                        }
                    }
                    curr = parent;
                };
                Some(ret)
            }
        }
        impl<'a, S: Shell> DoubleEndedIterator for SubsurfaceIter<'a, S> {
            fn next_back(&mut self) -> Option<Self::Item> {
                if self.empty {
                    return None
                }
                let ret = self.shell.get(self.tail).unwrap();
                if self.head == self.tail {
                    self.empty = true;
                    return Some(ret)
                }
                if let Some(p) = ret.antirole::<SubsurfaceParent<S>>() {
                    // inner node
                    if let Some(prev) = p.children.get_previous_index(p.index) {
                        // find the next surface in current surface's stack
                        let prev = *p.children.get(prev).unwrap(); // TODO use unwrap_unchecked
                                                                   // find the top most surface in `next`'s descendants
                        self.tail = find_last(prev, self.shell);
                        return Some(ret)
                    }
                }
                // current surface is the end of its stack. this includes leaf nodes.
                let mut curr = ret;
                self.tail = loop {
                    let role = curr.role::<Subsurface<S>>().unwrap();
                    let parent_key = role.parent.current.get();
                    let parent = self.shell.get(parent_key).unwrap();
                    let parent_antirole = parent.antirole::<SubsurfaceParent<S>>().unwrap();
                    if let Some(prev) = parent_antirole
                        .children
                        .get_previous_index(role.stack_index)
                    {
                        let prev = *parent_antirole.children.get(prev).unwrap();
                        if prev != parent_key {
                            // the next surface in the parent's stack is not the parent itself. we
                            // need to find the top most surface in `next`'s descendants.
                            break find_last(prev, self.shell)
                        } else {
                            // the next surface in the parent's stack is the parent itself.
                            break prev
                        }
                    }
                    curr = parent;
                };
                Some(ret)
            }
        }
        fn find_first<S: Shell>(root: S::Key, s: &S) -> S::Key {
            let mut curr = root;
            while let Some(p) = s.get(curr).unwrap().antirole::<SubsurfaceParent<S>>() {
                let front = *p
                    .children
                    .front()
                    .expect("subsurface parent must have at least one child");
                if front == curr {
                    break
                }
                curr = front;
            }
            curr
        }
        fn find_last<S: Shell>(root: S::Key, s: &S) -> S::Key {
            let mut curr = root;
            while let Some(p) = s.get(curr).unwrap().antirole::<SubsurfaceParent<S>>() {
                let back = *p
                    .children
                    .back()
                    .expect("subsurface parent must have at least one child");
                if back == curr {
                    break
                }
                curr = back;
            }
            curr
        }
        let ret = SubsurfaceIter {
            shell: s,
            head:  find_first(root, s),
            tail:  find_last(root, s),
            empty: false,
        };
        ret
    }
}

#[derive(Copy, Clone)]
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

#[derive(Default)]
pub struct SurfaceVTable<S: Shell> {
    pub role: &'static str,
    commit:   Option<fn(&mut S, &Surface<S>)>,
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
    role:           Option<Box<dyn Role>>,
    /// A list of antiroles, ordered by their names.
    antiroles:      VecList<Box<dyn Antirole>>,
    buffer:         Option<Rc<dyn super::buffers::Buffer>>,
    pub data:       S::Data,
}

impl<S: Shell> SurfaceState<S> {
    #[inline]
    pub fn antirole<T: Antirole>(&self) -> Option<&T> {
        self.antiroles.iter().find_map(|r| request_ref(r.as_ref()))
    }

    #[inline]
    pub fn antirole_mut<T: Antirole>(&mut self) -> Option<&mut T> {
        self.antiroles
            .iter_mut()
            .find_map(|r| request_mut(r.as_mut()))
    }

    #[inline]
    pub fn new_antirole<T: Antirole>(&mut self, antirole: T) {
        let indices = self.antiroles.indices();
        if indices.len() == 0 {
            self.antiroles.push_back(Box::new(antirole));
            return
        }
        for index in indices {
            // Safety: the head index is returned from the indices iterator, which
            // is guaranteed to be valid.
            let curr = unsafe { self.antiroles.get(index).unwrap_unchecked() };
            assert!(curr.name() != antirole.name());
            if curr.name() > antirole.name() {
                self.antiroles.insert_before(index, Box::new(antirole));
                break
            }
        }
    }

    #[inline]
    pub fn role<T: Role>(&self) -> Option<&T> {
        self.role.as_ref().and_then(|r| request_ref(r.as_ref()))
    }

    #[inline]
    pub fn role_mut<T: Role>(&mut self) -> Option<&mut T> {
        self.role.as_mut().and_then(|r| request_mut(r.as_mut()))
    }

    #[inline]
    pub fn remove_antirole<T: Antirole>(&mut self) {
        for index in self.antiroles.indices() {
            // Safety: the head index is returned from the indices iterator, which
            // is guaranteed to be valid.
            let curr = unsafe { self.antiroles.get(index).unwrap_unchecked() };
            if request_ref::<T, _>(curr.as_ref()).is_some() {
                self.antiroles.remove(index);
                break
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
        if let Some(ref mut role) = self.role {
            let pending_role = pending.role.as_ref().unwrap();
            role.rotate_from(pending_role.as_ref());
        } else if let Some(pending_role) = pending.role.as_ref() {
            self.role = Some(dyn_clone::clone_box(pending_role.as_ref()));
        }
        // TODO copy antiroles
        self.buffer = pending.buffer.clone();
    }
}

pub struct Surface<S: super::Shell> {
    /// The current state of the surface. Once a state is committed to current,
    /// it should not be modified.
    current: Cell<S::Key>,
    /// The pending state of the surface, this will be moved to [`current`] when
    /// commit is called
    pending: Cell<S::Key>,
    vtable:  Cell<SurfaceVTable<S>>,
}

impl<S: Shell> Drop for Surface<S> {
    fn drop(&mut self) {
        panic!("Surface must be destroyed with Surface::destroy");
    }
}

impl<S: Shell> Surface<S> {
    pub fn commit(&self, shell: &mut S) {
        let pending = self.pending.get();
        let current = self.current.get();
        if let Some(commit) = self.vtable.get().commit {
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

    pub fn destroy(&self, shell: &mut S) {
        shell.deallocate(self.current.get());
    }

    pub fn deactivate_role(&self) {
        todo!()
    }

    pub fn clear_damage(&self) {
        todo!()
    }

    pub fn vtable(&self) -> SurfaceVTable<S> {
        self.vtable.get()
    }
}
