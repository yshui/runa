use std::{
    any::Any,
    cell::{Cell, Ref, RefCell, RefMut},
    rc::{Rc, Weak},
};

use derivative::Derivative;
use dlv_list::{Index, VecList};
use dyn_clone::DynClone;
use hashbrown::HashSet;
use ordered_float::NotNan;
use tinyvec::TinyVec;
use tinyvecdeq::tinyvecdeq::TinyVecDeq;
use wl_protocol::wayland::wl_pointer::v9 as wl_pointer;
use wl_server::{
    events::{broadcast, single_state, EventSource},
    provide_any::{request_mut, request_ref, Demand, Provider},
};

use super::{buffers::AttachedBuffer, output::Output, xdg::Layout, Shell};
use crate::{
    objects,
    utils::{
        geometry::{coords, Point, Scale},
        WeakPtr,
    },
};

pub trait Role<S: Shell>: Any {
    fn name(&self) -> &'static str;
    // As specified by the wayland protocol, a surface can be assigned a role, then
    // have the role object destroyed. This makes the role "inactive", but the
    // surface cannot be assigned a different role. So we keep the role object
    // but "deactivate" it.
    fn is_active(&self) -> bool;
    /// Deactivate the role.
    fn deactivate(&mut self, shell: &mut S);
    fn provide<'a>(&'a self, _demand: &mut Demand<'a>) {}
    fn provide_mut<'a>(&'a mut self, _demand: &mut Demand<'a>) {}
    /// Called before the pending state becomes the current state, in
    /// Surface::commit. If an error is returned, the commit will be
    /// stopped.
    fn pre_commit(&mut self, shell: &mut S, surface: &Surface<S>) -> Result<(), &'static str> {
        Ok(())
    }
    fn post_commit(&mut self, shell: &mut S, surface: &Surface<S>) {}
}

pub trait RoleState: Any + DynClone + std::fmt::Debug + 'static {}

impl<S: Shell> Provider for dyn Role<S> {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        self.provide(demand);
    }

    fn provide_mut<'a>(&'a mut self, demand: &mut Demand<'a>) {
        self.provide_mut(demand);
    }
}

pub mod roles {
    use std::rc::{Rc, Weak};

    use derivative::Derivative;
    use dlv_list::Index;
    use wl_protocol::wayland::wl_subsurface;
    use wl_server::provide_any;

    use crate::{
        shell::Shell,
        utils::geometry::{coords, Point},
    };

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
    #[derive(Derivative)]
    #[derivative(Debug, Clone(bound = ""))]
    pub struct Subsurface<S: Shell> {
        sync:                   bool,
        inherited_sync:         bool,
        pub(super) is_active:   bool,
        /// Index of this surface in parent's `stack` list.
        /// Note this index should be stable across parent updates, including
        /// appending to the stack, reordering the stack. a guarantee
        /// from VecList.
        pub(crate) stack_index: Index<super::SurfaceStackEntry<S>>,
        pub(super) parent:      Weak<super::Surface<S>>,
    }

    #[derive(Derivative)]
    #[derivative(Debug, Clone(bound = ""), Copy(bound = ""))]
    pub(super) struct SubsurfaceState<S: Shell> {
        /// Parent surface *state* of this surface *state*. A surface state is
        /// only considered a parent after it has been committed. This
        /// is different from the parent surface, which is the
        /// `Rc<Surface>` that is the parent of this surface. A surface can have
        /// multiple surface states each have different parent surface
        /// states. But a surface can have only one parent surface.
        ///
        /// A surface state can have multiple parents because of the sync
        /// mechanism of subsurfaces. i.e. a subsurface can be attached
        /// to a parent, then the parent has its own parent. When
        /// the parent is committed, its old state will still be referenced by
        /// the grandparent, and it will have a new committed state.
        /// Both the old state and the new state will be "parents"
        /// of this surface state.
        ///
        /// If that's the case, this field will point to the oldest, still valid
        /// parent. For states visible from a "root" surface (e.g. a
        /// xdg_toplevel), this convienently forms a path towards the
        /// root's current state.
        pub(super) parent: Option<S::Token>,
    }
    impl<S: Shell> super::RoleState for SubsurfaceState<S> {}
    impl<S: Shell> Subsurface<S> {
        pub fn attach(
            parent: Rc<super::Surface<S>>,
            surface: Rc<super::Surface<S>>,
            shell: &mut S,
        ) -> bool {
            if surface.role.borrow().is_some() {
                // already has a role
                tracing::debug!("surface {:p} already has a role", Rc::as_ptr(&surface));
                return false
            }
            // Preventing cycle creation
            if Rc::ptr_eq(&parent.root(), &surface) {
                tracing::debug!("cycle detected");
                return false
            }
            let parent_pending = parent.pending_mut(shell);
            let stack_index = parent_pending.stack.push_front(super::SurfaceStackEntry {
                token:    surface.current_key(),
                position: Point::new(0, 0),
            });
            let role = Self {
                sync: true,
                inherited_sync: true,
                is_active: true,
                stack_index,
                parent: Rc::downgrade(&parent),
            };
            tracing::debug!(
                "attach {:p} to {:p}",
                Rc::as_ptr(&surface),
                Rc::as_ptr(&parent)
            );
            surface.set_role(role, shell);
            surface
                .current_mut(shell)
                .set_role_state(SubsurfaceState::<S> { parent: None });
            surface
                .pending_mut(shell)
                .set_role_state(SubsurfaceState::<S> { parent: None });
            true
        }

        pub fn parent(&self) -> &Weak<super::Surface<S>> {
            &self.parent
        }
    }
    impl<S: Shell> super::Role<S> for Subsurface<S> {
        fn name(&self) -> &'static str {
            wl_subsurface::v1::NAME
        }

        fn is_active(&self) -> bool {
            self.is_active
        }

        fn deactivate(&mut self, shell: &mut S) {
            tracing::debug!("deactivate subsurface role {}", self.is_active);
            if !self.is_active {
                return
            }
            // Deactivating the subsurface role is immediate, but we don't know how many
            // other surface states that are referencing this subsurface state,
            // as our parent can have any number of "cached" states.  So we
            // can't remove ourself from them. Instead we mark it inactive, and
            // skip over inactive states when we iterate over the subsurface
            // tree.
            self.is_active = false;

            // Remove ourself from parent's pending stack, so when the parent
            // eventually commits, it will drop us.
            let parent = self
                .parent
                .upgrade()
                .expect("surface is destroyed but its state is still being used");
            let parent_pending_state = parent.pending_mut(shell);
            parent_pending_state.stack.remove(self.stack_index).unwrap();
            self.parent = Weak::new();
        }

        fn provide<'a>(&'a self, demand: &mut provide_any::Demand<'a>) {
            demand.provide_ref(self);
        }

        fn provide_mut<'a>(&'a mut self, demand: &mut provide_any::Demand<'a>) {
            demand.provide_mut(self);
        }

        fn post_commit(&mut self, shell: &mut S, surface: &super::Surface<S>) {
            // update the state referenced in parent's pending state's stack.
            let parent = self
                .parent
                .upgrade()
                .expect("surface is destroyed but its state is still being used");

            let parent_pending_state = parent.pending_mut(shell);
            let parent_pending_stack_entry = parent_pending_state
                .stack
                .get_mut(self.stack_index)
                .unwrap();
            parent_pending_stack_entry.token = surface.current_key();

            // the current state is now referenced by the parent's pending state,
            // clear the parent field. (parent could have been set because pending state was
            // cloned from a previous current state)
            let current = surface.current_mut(shell);
            let role_state = current
                .role_state_mut::<SubsurfaceState<S>>()
                .expect("subsurface role state missing")
                .expect("subsurface role state has unexpected type");
            role_state.parent = None;
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
    #[allow(clippy::needless_lifetimes)]
    pub fn subsurface_iter<'a, S: Shell>(
        root: S::Token,
        s: &'a S,
    ) -> impl DoubleEndedIterator<Item = (S::Token, Point<i32, coords::Surface>)> + 'a {
        macro_rules! generate_advance {
            ($next_in_stack:ident, $next_maybe_deactivated:ident, $next:ident, $id:literal) => {
                /// Advance the front pointer to the next surface in the
                /// stack. The next surface might be deactivated.
                fn $next_maybe_deactivated(&mut self) {
                    if self.head[0].0 == self.head[1].0 {
                        self.is_empty = true;
                    }
                    if self.is_empty {
                        return
                    }

                    // The head we are advancing
                    let curr_head = &mut self.head[$id];

                    let ret = self.shell.get(curr_head.0);
                    if let Some((next, offset)) = ret
                        .stack_index
                        .and_then(|i| ret.$next_in_stack(i, self.shell))
                    {
                        curr_head.1 += offset;
                        curr_head.0 = next;
                    } else {
                        // `ret` is  at the bottom/top of its own stack. this includes the case of
                        // `ret` being the only surface in its stack. So we need return
                        // upwards to the parent, and find the next surface in the parent's
                        // stack. We do this repeatedly if we are also at the end of the
                        // parent's stack.
                        let mut curr = ret;
                        let mut offset = curr_head.1;
                        *curr_head = loop {
                            let role_state = curr
                                .role_state::<SubsurfaceState<S>>()
                                .expect("subsurface role state missing")
                                .expect("subsurface role state has unexpected type");
                            let parent_key = role_state.parent.unwrap_or_else(|| {
                                panic!(
                                    "surface state {curr:?} (key: {:?}) has no parent, but is in \
                                     a stack",
                                    curr_head.0
                                )
                            });
                            let parent = self.shell.get(parent_key);
                            let curr_surface = curr.surface.upgrade().unwrap();
                            let stack_index =
                                curr_surface.role::<Subsurface<S>>().unwrap().stack_index;
                            offset -= parent.stack.get(stack_index).unwrap().position;

                            if let Some((next, next_offset)) =
                                parent.$next_in_stack(stack_index, self.shell)
                            {
                                offset += next_offset;
                                break (next, offset)
                            }
                            curr = parent;
                        };
                    }
                }

                fn $next(&mut self) {
                    while !self.is_empty {
                        use super::Role;
                        self.$next_maybe_deactivated();
                        let ret = self.shell.get(self.head[0].0);
                        let ret_surface = ret.surface.upgrade().unwrap();
                        let role = ret_surface.role::<Subsurface<S>>();
                        if role.map(|r| r.is_active()).unwrap_or(true) {
                            break
                        }
                    }
                }
            };
        }
        struct SubsurfaceIter<'a, S: Shell> {
            shell:    &'a S,
            /// Key and offset from the root surface.
            head:     [(S::Token, Point<i32, coords::Surface>); 2],
            is_empty: bool,
        }

        impl<'a, S: Shell> SubsurfaceIter<'a, S> {
            generate_advance!(next_in_stack, next_maybe_deactivated, next, 0);

            generate_advance!(prev_in_stack, prev_maybe_deactivated, prev, 1);
        }

        impl<'a, S: Shell> Iterator for SubsurfaceIter<'a, S> {
            type Item = (S::Token, Point<i32, coords::Surface>);

            fn next(&mut self) -> Option<Self::Item> {
                if self.is_empty {
                    None
                } else {
                    let ret = self.head[0];
                    self.next();
                    Some(ret)
                }
            }
        }
        impl<'a, S: Shell> DoubleEndedIterator for SubsurfaceIter<'a, S> {
            fn next_back(&mut self) -> Option<Self::Item> {
                if self.is_empty {
                    None
                } else {
                    let ret = self.head[1];
                    self.prev();
                    Some(ret)
                }
            }
        }

        SubsurfaceIter {
            shell:    s,
            head:     [
                super::SurfaceState::top(root, s),
                super::SurfaceState::bottom(root, s),
            ],
            is_empty: false,
        }
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

/// A entry in a surface's stack
///
/// Each surface has a stack, composed of itself and its subsurfaces.
#[derive(Derivative)]
#[derivative(Debug(bound = ""), Clone(bound = ""))]
pub struct SurfaceStackEntry<S: Shell> {
    /// The token of the surface in the stack. Note, a surface is in its own
    /// stack too.
    pub(crate) token:    S::Token,
    pub(crate) position: Point<i32, coords::Surface>,
}

impl<S: Shell> SurfaceStackEntry<S> {
    pub fn position(&self) -> Point<i32, coords::Surface> {
        self.position
    }
}

/// To support the current state and the pending state double buffering, the
/// surface state must be deep cloneable.
pub struct SurfaceState<S: Shell> {
    /// A set of flags that can be mutate even when the state is the current
    /// state of a surface.
    flags:                         Cell<SurfaceFlags>,
    surface:                       Weak<Surface<S>>,
    /// The index just pass the last frame callback attached to this surface.
    /// For the definition of frame callback index, see
    /// [`Surface::first_frame_callback_index`]. Each surface state always owns
    /// a prefix of the surfaces frame callbacks, so it's sufficient to
    /// store the index of the last frame callback.
    pub(crate) frame_callback_end: u32,
    /// A stack of child surfaces and self.
    stack:                         VecList<SurfaceStackEntry<S>>,
    /// The position of this surface state in its own stack.
    /// None if the stack is just self.
    pub(crate) stack_index:        Option<Index<SurfaceStackEntry<S>>>,
    buffer:                        Option<AttachedBuffer<S::Buffer>>,
    // HACK! TODO: properly implement pending commit
    buffer_changed:                bool,
    /// Scale of the buffer, a fraction with a denominator of 120
    buffer_scale:                  u32,
    role_state:                    Option<Box<dyn RoleState>>,
}

impl<S: Shell> std::fmt::Debug for SurfaceState<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::shell::buffers::BufferLike;
        f.debug_struct("SurfaceState")
            .field("flags", &self.flags)
            .field("surface", &self.surface.upgrade().map(|s| s.object_id()))
            .field("frame_callback_end", &self.frame_callback_end)
            .field("stack", &self.stack)
            .field("buffer", &self.buffer.as_ref().map(|b| b.inner.object_id()))
            .field("buffer_scale", &self.buffer_scale_f32())
            .field("role_state", &self.role_state)
            .finish()
    }
}

impl<S: Shell> SurfaceState<S> {
    pub fn new(surface: Rc<Surface<S>>) -> Self {
        Self {
            flags:              Cell::new(SurfaceFlags { destroyed: false }),
            surface:            Rc::downgrade(&surface),
            frame_callback_end: 0,
            stack:              Default::default(),
            stack_index:        None,
            buffer:             None,
            buffer_changed:     false,
            buffer_scale:       120,
            role_state:         None,
        }
    }

    pub fn stack(&self) -> &VecList<SurfaceStackEntry<S>> {
        &self.stack
    }

    pub fn stack_mut(&mut self) -> &mut VecList<SurfaceStackEntry<S>> {
        &mut self.stack
    }
}
impl<S: Shell> Clone for SurfaceState<S> {
    fn clone(&self) -> Self {
        Self {
            flags:              self.flags.clone(),
            surface:            self.surface.clone(),
            frame_callback_end: self.frame_callback_end,
            stack:              self.stack.clone(),
            stack_index:        self.stack_index,
            buffer:             self.buffer.clone(),
            buffer_changed:     self.buffer_changed,
            buffer_scale:       self.buffer_scale,
            role_state:         self.role_state.as_ref().map(|x| dyn_clone::clone_box(&**x)),
        }
    }
}

impl<S: Shell> SurfaceState<S> {
    pub fn parent(&self) -> Option<S::Token> {
        let role_state = self.role_state.as_ref()?;
        let role_state = (&**role_state as &dyn Any).downcast_ref::<roles::SubsurfaceState<S>>()?;
        role_state.parent
    }

    /// Find the top of the subtree rooted at this surface. Returns the token,
    /// and it's offset relative to this surface.
    pub fn top(mut this: S::Token, shell: &S) -> (S::Token, Point<i32, coords::Surface>) {
        let mut offset = Point::new(0, 0);
        loop {
            let next = shell.get(this);
            if let Some(first) = next.stack.front() {
                // `top` is the  next surface in `self`'s stack, but it isn't necessarily the
                // next surface in the entire subtree stack. Because `top`
                // itself can have a stack. So we need to
                // recursively find the top most surface in `top`'s stack.
                if this != first.token {
                    this = first.token;
                    offset += first.position;
                } else {
                    // `top` is the top of its own stack, so we don't need to keep descending.
                    break
                }
            } else {
                break
            }
        }
        (this, offset)
    }

    /// The the next surface of `index` in the subtree rooted at this surface.
    /// Returns the token, and it's offset relative to this surface.
    ///
    /// # Example
    ///
    /// Say surface A has stack "B A C D", and surface D has stack "E D F G".
    /// Then `A.next_in_stack(C)` will return E. Because D is the next surface
    /// of C in A's stack, and the first surface of D's stack is E.
    pub fn next_in_stack(
        &self,
        index: Index<SurfaceStackEntry<S>>,
        shell: &S,
    ) -> Option<(S::Token, Point<i32, coords::Surface>)> {
        let next_index = self.stack.get_next_index(index);
        if let Some(next_index) = next_index {
            // Safety: next_index is a valid index returned by
            // get_next_index/get_previous_index
            let next_child = unsafe { self.stack.get_unchecked(next_index) };
            if next_index != self.stack_index.unwrap() {
                let (top, offset) = Self::top(next_child.token, shell);
                Some((top, offset + next_child.position))
            } else {
                // Next surface in self's stack is self itself, so we don't need to keep
                // descending.
                Some((next_child.token, next_child.position))
            }
        } else {
            None
        }
    }

    /// Find the bottom of the subtree rooted at this surface. Returns the
    /// token, and it's offset relative to this surface.
    pub fn bottom(mut this: S::Token, shell: &S) -> (S::Token, Point<i32, coords::Surface>) {
        let mut offset = Point::new(0, 0);
        loop {
            let next = shell.get(this);
            if let Some(last) = next.stack.back() {
                if this != last.token {
                    this = last.token;
                    offset += last.position;
                } else {
                    break
                }
            } else {
                break
            }
        }
        (this, offset)
    }

    /// The the previous surface of `index` in the subtree rooted at this
    /// surface. Returns the token, and it's offset relative to this
    /// surface.
    ///
    /// # Example
    ///
    /// Say surface A has stack "B A C D", and surface C has stack "E C F G".
    /// Then `A.prev_in_stack(D)` will return G. Because C is the previous
    /// surface of D in A's stack, and the bottom most surface of C's stack
    /// is G.
    pub fn prev_in_stack(
        &self,
        index: Index<SurfaceStackEntry<S>>,
        shell: &S,
    ) -> Option<(S::Token, Point<i32, coords::Surface>)> {
        let prev_index = self.stack.get_previous_index(index);
        if let Some(prev_index) = prev_index {
            let prev_child = unsafe { self.stack.get_unchecked(prev_index) };
            if prev_index != self.stack_index.unwrap() {
                let (bottom, offset) = Self::bottom(prev_child.token, shell);
                Some((bottom, offset + prev_child.position))
            } else {
                Some((prev_child.token, prev_child.position))
            }
        } else {
            None
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
    pub fn buffer_scale(&self) -> u32 {
        self.buffer_scale
    }

    #[inline]
    pub fn buffer_scale_f32(&self) -> Scale<NotNan<f32>> {
        use num_traits::AsPrimitive;
        let scale: NotNan<f32> = self.buffer_scale.as_();
        Scale::uniform(scale / 120.0)
    }

    #[inline]
    pub fn set_buffer_scale(&mut self, scale: u32) {
        self.buffer_scale = scale;
    }

    #[inline]
    pub fn surface(&self) -> &Weak<Surface<S>> {
        &self.surface
    }

    // TODO: take rectangles
    /// Mark the surface's buffer as damaged. No-op if the surface has no
    /// buffer.
    pub fn damage_buffer(&mut self) {
        use super::buffers::BufferLike;
        if let Some(buffer) = self.buffer.as_ref() {
            buffer.inner.damage();
        }
    }

    #[tracing::instrument(level = "debug", skip(self))]
    pub(crate) fn set_buffer(&mut self, buffer: Option<AttachedBuffer<S::Buffer>>) {
        self.buffer_changed = true;
        self.buffer = buffer;
    }

    pub fn set_buffer_from_object(&mut self, buffer: &objects::Buffer<S::Buffer>) {
        self.set_buffer(Some(buffer.buffer.attach()));
    }

    pub fn buffer(&self) -> Option<&Rc<S::Buffer>> {
        self.buffer.as_ref().map(|b| &b.inner)
    }

    pub fn add_frame_callback(&mut self, callback: u32) {
        let surface = self
            .surface()
            .upgrade()
            .expect("adding frame callback to dead surface");
        surface.frame_callbacks.borrow_mut().push_back(callback);
        self.frame_callback_end += 1;
        debug_assert_eq!(
            self.frame_callback_end,
            surface.first_frame_callback_index.get() +
                surface.frame_callbacks.borrow().len() as u32
        );
    }

    pub fn set_role_state<T: RoleState>(&mut self, state: T) {
        self.role_state = Some(Box::new(state));
    }

    pub fn role_state<T: RoleState>(&self) -> Option<Option<&T>> {
        self.role_state
            .as_ref()
            .map(|s| (&**s as &dyn Any).downcast_ref::<T>())
    }

    pub fn role_state_mut<T: RoleState>(&mut self) -> Option<Option<&mut T>> {
        self.role_state
            .as_mut()
            .map(|s| (&mut **s as &mut dyn Any).downcast_mut::<T>())
    }

    /// Assuming `token` is going to be released, scan the tree for any
    /// transient children that can be freed as well, and append them to
    /// `queue`.
    pub fn scan_for_freeing(token: S::Token, shell: &S, queue: &mut Vec<S::Token>) {
        let this = shell.get(token);
        let mut head = queue.len();
        let parent = this.parent();
        if parent.is_none() {
            queue.push(token);
        }
        while head < queue.len() {
            let token = queue[head];
            let state = shell.get(token);
            for e in state.stack.iter() {
                if e.token == token {
                    continue
                }
                let child_state = shell.get(e.token);
                let child_subsurface_state = (&**child_state.role_state.as_ref().unwrap()
                    as &dyn Any)
                    .downcast_ref::<roles::SubsurfaceState<S>>()
                    .unwrap();
                let parent = child_subsurface_state.parent;
                if parent.map(|p| p == token).unwrap_or(true) {
                    // `token` is the oldest parent of child, so we can free the child.
                    queue.push(e.token);
                }
            }
            head += 1;
        }
    }
}

pub(crate) type OutputSet = Rc<RefCell<HashSet<WeakPtr<Output>>>>;

#[derive(Clone, Debug)]
pub struct OutputEvent(pub(crate) OutputSet);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerEventKind {
    Motion {
        coords: Point<NotNan<f32>, coords::Surface>,
    },
    Button {
        // TODO: remove coords, and require first event on a surface to be Motion
        button: u32,
        state:  wl_pointer::enums::ButtonState,
    },
    Leave,
    // TODO: axis
    // ...
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerEvent {
    pub time:      u32,
    pub object_id: u32,
    pub kind:      PointerEventKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyboardState {
    /// All the currently pressed keys, most keyboard doesn't have more than
    /// 6 key rollover, so 8 should be enough to avoid allocations for most
    /// cases.
    pub keys:                TinyVec<[u8; 8]>,
    pub depressed_modifiers: u32,
    pub latched_modifiers:   u32,
    pub locked_modifiers:    u32,
    /// This is called the "group" in the wayland protocol
    pub effective_layout:    u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyboardActivity {
    Key(KeyboardState),
    Leave,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyboardEvent {
    pub time:      u32,
    pub object_id: u32,
    pub activity:  KeyboardActivity,
}

#[derive(Clone, Debug)]
pub(crate) struct LayoutEvent(pub(crate) Layout);
/// Maximum number of frame callbacks that can be registered on a surface.
pub const MAX_FRAME_CALLBACKS: usize = 100;
// TODO: make Surface not shared. Role objects can just contain an object id
// maybe?
pub struct Surface<S: super::Shell> {
    /// The current state of the surface. Once a state is committed to current,
    /// it should not be modified.
    current:                    Cell<Option<S::Token>>,
    /// The pending state of the surface, this will be moved to [`current`] when
    /// commit is called
    pending:                    Cell<Option<S::Token>>,
    /// Set of all states associated with this surface
    /// XXX: XXX: maybe delete this? instead, track the set of all unfired frame
    /// callbacks associated with this surface, in any of its surface
    /// states.
    frame_callbacks:            RefCell<TinyVecDeq<[u32; 4]>>,
    /// The index of the first frame callback stored in `frame_callbacks`. Frame
    /// callbacks attached to a surface is numbered starting from 0, and
    /// loops over when it reaches `u32::MAX`. Callbacks are removed from
    /// `frame_callbacks` when they are fired, and the index is incremented
    /// accordingly.
    first_frame_callback_index: Cell<u32>,
    role:                       RefCell<Option<Box<dyn Role<S>>>>,
    outputs:                    OutputSet,
    output_change_events:       single_state::Sender<OutputEvent>,
    pointer_events:             broadcast::Ring<PointerEvent>,
    keyboard_events:            broadcast::Ring<KeyboardEvent>,
    layout_change_events:       single_state::Sender<LayoutEvent>,
    object_id:                  u32,
}

impl<S: Shell> std::fmt::Debug for Surface<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Surface")
            .field("current", &self.current)
            .field("pending", &self.pending)
            .field("frame_callbacks", &self.frame_callbacks)
            .field(
                "first_frame_callback_index",
                &self.first_frame_callback_index,
            )
            .field("object_id", &self.object_id)
            .finish()
    }
}

impl<S: Shell> Drop for Surface<S> {
    fn drop(&mut self) {
        assert!(
            self.current == Default::default() && self.pending == Default::default(),
            "Surface must be destroyed with Surface::destroy"
        );
        assert!(
            self.frame_callbacks.borrow().is_empty(),
            "Surface must be have no frame callbacks when dropped."
        );
    }
}

impl<S: Shell> Surface<S> {
    #[must_use]
    pub fn new(
        object_id: wl_types::NewId,
        pointer_events: broadcast::Ring<PointerEvent>,
        keyboard_events: broadcast::Ring<KeyboardEvent>,
    ) -> Self {
        Self {
            current: Cell::new(None),
            pending: Cell::new(None),
            role: Default::default(),
            outputs: Default::default(),
            output_change_events: Default::default(),
            layout_change_events: Default::default(),
            pointer_events,
            keyboard_events,
            object_id: object_id.0,
            frame_callbacks: Default::default(),
            first_frame_callback_index: 0.into(),
        }
    }

    pub fn frame_callbacks(&self) -> &RefCell<TinyVecDeq<[u32; 4]>> {
        &self.frame_callbacks
    }

    pub fn first_frame_callback_index(&self) -> u32 {
        self.first_frame_callback_index.get()
    }

    pub fn set_first_frame_callback_index(&self, index: u32) {
        self.first_frame_callback_index.set(index)
    }
}

// TODO: maybe we can unshare Surface, not wrapping it in Rc<>
impl<S: Shell> Surface<S> {
    /// Get the parent surface of this surface has a subsurface role.
    pub fn parent(&self) -> Option<Rc<Self>> {
        let role = self.role::<roles::Subsurface<S>>();
        role.map(|r| r.parent.upgrade().unwrap())
    }

    /// Follow the parent link of this surface until the root is reached.
    pub fn root(self: &Rc<Self>) -> Rc<Self> {
        let mut root = self.clone();
        loop {
            let Some(next) = root.parent() else { break };
            root = next;
        }
        root
    }

    /// Set the pending state as the current state of this surface.
    ///
    /// Put potentially free-able surface states into `scratch_buffer`. Also
    /// updates the frame_callbacks indices.
    fn apply_pending(&self, shell: &mut S, scratch_buffer: &mut Vec<S::Token>) {
        let new_current = self.pending_key();
        let old_current = self.current_key();
        if new_current == old_current {
            return
        }
        scratch_buffer.clear();

        // Update the frame callback indices
        {
            let new_state = shell.get_mut(new_current);
            new_state.frame_callback_end =
                self.first_frame_callback_index.get() + self.frame_callbacks.borrow().len() as u32;
        }

        self.set_current(new_current);
        tracing::trace!("new surface state: {:?}", shell.get(new_current));

        SurfaceState::scan_for_freeing(old_current, shell, scratch_buffer);
        tracing::debug!("potential freeable surface states: {:?}", scratch_buffer);
        for &child_token in &scratch_buffer[..] {
            let child_state = shell.get_mut(child_token);
            if let Some(role_state) = child_state.role_state.as_mut() {
                let child_subsurface_state = (&mut **role_state as &mut dyn Any)
                    .downcast_mut::<roles::SubsurfaceState<S>>()
                    .unwrap();
                child_subsurface_state.parent = None;
            }
        }

        // At this point, scratch_buffer contains a list of surface states that
        // potentially can be freed.
        let free_candidates_end = scratch_buffer.len();

        let new_state = shell.get(new_current);
        scratch_buffer.push(new_current);
        let mut head = free_candidates_end;

        // Recursively update descendents' parent_info now we have committed.
        // A descendent's parent info will be updated if it is None, which means it
        // either hasn't been referenced by a committed surface state yet, or
        // the above freeing process has freed its old parent. In both cases, we
        // can update its parent info to point to the new parent.
        while head < scratch_buffer.len() {
            let token = scratch_buffer[head];
            let state = shell.get(token);
            let child_start = scratch_buffer.len();
            for e in state.stack.iter() {
                if e.token == token {
                    continue
                }
                let child_state = shell.get(e.token);
                let child_subsurface_state = child_state
                    .role_state::<roles::SubsurfaceState<S>>()
                    .expect("subsurface role state missing")
                    .expect("subsurface role state has unexpected type");
                if child_subsurface_state.parent.is_some() {
                    continue
                }
                tracing::debug!("{:?} is still reachable", e.token);
                scratch_buffer.push(e.token);
            }
            for &child_token in &scratch_buffer[child_start..] {
                let [child_state, state] = shell.get_disjoint_mut([child_token, token]);
                let child_subsurface_state = (&mut **child_state.role_state.as_mut().unwrap()
                    as &mut dyn Any)
                    .downcast_mut::<roles::SubsurfaceState<S>>()
                    .unwrap();
                child_subsurface_state.parent = Some(new_current);
            }
            head += 1;
        }
        scratch_buffer.truncate(free_candidates_end);
    }

    /// Commit the pending state to the current state.
    ///
    /// # Arguments
    ///
    /// * `shell` - The shell to use to get the current state.
    /// * `scratch_buffer` - A scratch buffer to use for temporary storage. we
    ///   take this argument so we don't have to allocate a new buffer every
    ///   time.
    ///
    /// Returns if the commit is successful.
    ///
    /// TODO: FIXME: this implementation of commit is inaccurate. Per wayland
    /// spec, the pending state is not a shadow state, where changes are
    /// applied to. Instead it's a collection of pending changes, that are
    /// applied to the current state when commited. The difference is
    /// subtle. For example, if buffer transform changes between two
    /// damage_buffer requests, both requests should use the new transform;
    /// instead of the first using the old transform and the second using
    /// the new transform.
    pub fn commit(
        &self,
        shell: &mut S,
        scratch_buffer: &mut Vec<S::Token>,
    ) -> Result<(), &'static str> {
        tracing::debug!(?self, "generic surface commit");

        if let Some(role) = self.role.borrow_mut().as_mut() {
            if role.is_active() {
                role.pre_commit(shell, self)?;
            }
        }
        let new_current = self.pending_key();
        let old_current = self.current_key();

        let new_state = shell.get(new_current);
        if new_state.buffer_changed {
            // We have a new buffer, we need to call acquire on it.
            if let Some(buffer) = &new_state.buffer {
                use crate::shell::buffers::BufferLike;
                buffer.inner.acquire();
            }
        }

        self.apply_pending(shell, scratch_buffer);

        // Call post_commit hooks before we free the old states, they might still need
        // them.
        if let Some(role) = self.role.borrow_mut().as_mut() {
            if role.is_active() {
                role.post_commit(shell, self);
            }
        }
        shell.post_commit(Some(old_current), new_current);

        // Now we have updated parent info, if any of the surface states iterated over
        // in the first freeing pass still doesn't have a parent, they can be
        // freed. (this also includes the old current state)
        for &token in &scratch_buffer[..] {
            let state = shell.get(token);
            if state.parent().is_none() {
                shell.destroy(token);
            }
        }
        scratch_buffer.clear();

        // TODO: release states
        Ok(())
    }

    pub fn object_id(&self) -> u32 {
        self.object_id
    }

    pub fn set_current(&self, key: S::Token) {
        self.current.set(Some(key));
    }

    pub fn set_pending(&self, key: S::Token) {
        self.pending.set(Some(key));
    }

    pub fn pending_key(&self) -> S::Token {
        self.pending.get().unwrap()
    }

    pub fn current_key(&self) -> S::Token {
        self.current.get().unwrap()
    }

    pub fn pending_mut<'a>(&self, shell: &'a mut S) -> &'a mut SurfaceState<S> {
        let current = self.current_key();
        let pending = self.pending_key();
        if current == pending {
            // If the pending state is the same as the current state, we need to
            // duplicate it so we can modify it.
            tracing::debug!(
                "Creating a new pending state for surface {}",
                self.object_id
            );
            let current_key = self.current_key();
            let new_pending_key = shell.allocate(self.current(shell).clone());
            let new_pending_state = shell.get_mut(new_pending_key);

            // the current_state's stack has an entry that points to current_state itself,
            // and new_pending_state is a copy of that. so we need to update that to point
            // to the new_pending_state itself.
            new_pending_state
                .stack
                .iter_mut()
                .find(|e| e.token == current_key)
                .unwrap()
                .token = new_pending_key;
            new_pending_state.buffer_changed = false;
            self.set_pending(new_pending_key);
        }
        shell.get_mut(self.pending_key())
    }

    pub fn pending<'a>(&self, shell: &'a S) -> &'a SurfaceState<S> {
        shell.get(self.pending_key())
    }

    pub fn current<'a>(&self, shell: &'a S) -> &'a SurfaceState<S> {
        shell.get(self.current_key())
    }

    pub fn current_mut<'a>(&self, shell: &'a mut S) -> &'a mut SurfaceState<S> {
        shell.get_mut(self.current_key())
    }

    /// Returns true if the surface has a role attached. This will keep
    /// returning true even after the role has been deactivated.
    #[must_use]
    pub fn has_role(&self) -> bool {
        self.role.borrow().is_some()
    }

    /// Returns true if the surface has a role, and that role is active.
    #[must_use]
    pub fn role_is_active(&self) -> bool {
        self.role
            .borrow()
            .as_ref()
            .map(|r| r.is_active())
            .unwrap_or(false)
    }

    /// Borrow the role object of the surface.
    #[must_use]
    pub fn role<T: Role<S>>(&self) -> Option<Ref<T>> {
        let role = self.role.borrow();
        Ref::filter_map(role, |r| r.as_ref().and_then(|r| request_ref(&**r))).ok()
    }

    /// Mutably borrow the role object of the surface.
    #[must_use]
    pub fn role_mut<T: Role<S>>(&self) -> Option<RefMut<T>> {
        let role = self.role.borrow_mut();
        RefMut::filter_map(role, |r| r.as_mut().and_then(|r| request_mut(&mut **r))).ok()
    }

    pub fn destroy(&self, shell: &mut S, scratch_buffer: &mut Vec<S::Token>) {
        // This function needs to do these things:
        //  - free self.current if it's not referenced by any parent surface states.
        //  - free self.pending
        //  - also free any states that are only reachable via self.current
        //
        // Which can be split into 2 cases:
        //
        // 1. if self.current can be freed:
        //   - semi-commit the pending state, so any outdated states referenced only by
        //     the current state can be found and will be freed.
        //   - now the pending state is the current state, and its children's oldest
        //     parent link should have been updated.
        //   - free self.current (was self.pending before semi-commit), and disconnect
        //     its children's parent link.
        // 2. if self.current can't be freed:
        //   - just free self.pending if it's not the same as self.current
        tracing::debug!(
            "Destroying surface {:p}, (id: {}, current: {:?}, pending: {:?})",
            self,
            self.object_id,
            self.current,
            self.pending
        );

        self.deactivate_role(shell);

        if self.current(shell).parent().is_none() {
            self.apply_pending(shell, scratch_buffer);
            for &token in scratch_buffer.iter() {
                let state = shell.get(token);
                if state.parent().is_none() {
                    shell.destroy(token);
                }
            }
            scratch_buffer.clear();

            // disconnect subsurface states from the now committed pending state,
            // without freeing them, because they are still current in the
            // subsurfaces.
            // get the list of our children, swap the stack out so we don't have to borrow
            // `shell`.
            let state = self.current_mut(shell);
            let mut stack = Default::default();
            std::mem::swap(&mut state.stack, &mut stack);

            // orphan all our children
            let current_key = self.current_key();
            for child in stack {
                if child.token == current_key {
                    continue
                }
                let child = shell.get_mut(child.token);
                let role_state = child
                    .role_state_mut::<roles::SubsurfaceState<S>>()
                    .expect("subsurface role state missing")
                    .expect("subsurface role state has unexpected type");
                if role_state.parent == Some(current_key) {
                    role_state.parent = None;
                }

                // We need to deactivate the subsurface role of the child, but we don't want to
                // call roles::Subsurface::deactivate() because it wants to modify
                // the parent's (this surface's) pending state, which will cause a
                // new pending state to be duplicated and assigned to us, which we
                // don't want to happen.
                let child_surface = child.surface().upgrade().unwrap();
                let mut child_role = child_surface.role_mut::<roles::Subsurface<S>>().unwrap();
                child_role.is_active = false;
                child_role.parent = Weak::new();
            }
            shell.destroy(current_key);
        } else {
            // can't free self.current, so just free self.pending
            if self.pending_key() != self.current_key() {
                shell.destroy(self.pending_key());
            }
        }
        self.pending.set(None);
        self.current.set(None);
    }

    pub fn deactivate_role(&self, shell: &mut S) {
        if let Some(role) = self.role.borrow_mut().as_mut() {
            if role.is_active() {
                role.deactivate(shell);
                shell.get_mut(self.current_key()).role_state = None;
                shell.get_mut(self.pending_key()).role_state = None;
                shell.role_deactivated(self.current_key(), role.name());
                assert!(!role.is_active());
            }
        };
    }

    pub fn clear_damage(&self) {
        todo!()
    }

    pub fn set_role<T: Role<S>>(&self, role: T, shell: &mut S) {
        let role_name = role.name();
        {
            let mut role_mut = self.role.borrow_mut();
            *role_mut = Some(Box::new(role));
        }
        shell.role_added(self.current_key(), role_name);
    }

    pub fn outputs(&self) -> impl std::ops::Deref<Target = HashSet<WeakPtr<Output>>> + '_ {
        self.outputs.borrow()
    }

    pub fn outputs_mut(&self) -> RefMut<'_, HashSet<WeakPtr<Output>>> {
        self.outputs.borrow_mut()
    }

    pub fn notify_output_changed(&self) {
        self.output_change_events
            .send(OutputEvent(self.outputs.clone()));
    }

    pub fn notify_layout_changed(&self, layout: Layout) {
        self.layout_change_events.send(LayoutEvent(layout));
    }

    pub fn pointer_event(&self, event: PointerEventKind) {
        let event = PointerEvent {
            time:      crate::time::elapsed().as_millis() as u32,
            object_id: self.object_id,
            kind:      event,
        };
        self.pointer_events.broadcast(event);
    }

    pub fn keyboard_event(&self, event: KeyboardActivity) {
        let event = KeyboardEvent {
            time:      crate::time::elapsed().as_millis() as u32,
            object_id: self.object_id,
            activity:  event,
        };
        self.keyboard_events.broadcast(event);
    }
}

impl<S: Shell> EventSource<OutputEvent> for Surface<S> {
    type Source = single_state::Receiver<OutputEvent>;

    fn subscribe(&self) -> Self::Source {
        self.output_change_events.new_receiver()
    }
}

impl<S: Shell> EventSource<LayoutEvent> for Surface<S> {
    type Source = single_state::Receiver<LayoutEvent>;

    fn subscribe(&self) -> Self::Source {
        self.layout_change_events.new_receiver()
    }
}
