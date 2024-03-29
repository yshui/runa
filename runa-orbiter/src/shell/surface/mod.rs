//! Wayland surfaces
//!
//! This is unlike [`compositor::Surface`](crate::objects::compositor::Surface),
//! which is a proxy object in the client's object store representing an actual
//! surface, which is defined here.

use std::{
    any::Any,
    cell::{Cell, Ref, RefCell, RefMut},
    fmt::Debug,
    rc::{Rc, Weak},
};

use derive_where::derive_where;
use dlv_list::{Index, VecList};
use hashbrown::HashSet;
use ordered_float::NotNan;
use runa_core::{
    events::{broadcast, single_state, EventSource},
    provide_any::{request_mut, request_ref},
};
use runa_wayland_types::NewId;
use tinyvecdeq::tinyvecdeq::TinyVecDeq;

use super::{buffers::AttachedBuffer, output::Output, xdg::Layout, Shell};
use crate::{
    objects::{
        self,
        input::{KeyboardActivity, PointerActivity},
    },
    utils::{
        geometry::{coords, Point, Scale},
        WeakPtr,
    },
};

/// Some roles defined in the wayland protocol
pub mod roles;

/// A entry in a surface's stack
///
/// Each surface has a stack, composed of the surface itself and its
/// subsurfaces.
#[derive(Debug, Clone)]
pub(crate) enum StackEntry<Token> {
    /// The surface itself
    Self_,
    /// A subsurface
    Subsurface {
        token:    Token,
        position: Point<i32, coords::Surface>,
    },
}

/// Index into a surface's stack
#[derive_where(Debug, Clone, Copy)]
pub struct StackIndex<Token>(pub(crate) Index<StackEntry<Token>>);

impl<T> From<Index<StackEntry<T>>> for StackIndex<T> {
    fn from(index: Index<StackEntry<T>>) -> Self {
        Self(index)
    }
}

impl<Token> StackEntry<Token> {
    /// Position of a surface in a stack, relative to the surface the stack
    /// belongs to.
    pub(crate) fn position(&self) -> Point<i32, coords::Surface> {
        match self {
            StackEntry::Self_ => Point::new(0, 0),
            StackEntry::Subsurface { position, .. } => *position,
        }
    }

    pub(crate) fn set_position(&mut self, position: Point<i32, coords::Surface>) {
        match self {
            StackEntry::Self_ => panic!("cannot set position of self"),
            StackEntry::Subsurface { position: p, .. } => *p = position,
        }
    }
}

/// The surface state
///
/// This holds the modifiable states of a surface.
pub struct State<S: Shell> {
    /// Weak reference to the surface owning this state
    surface:                    Weak<Surface<S>>,
    /// List of of all the unfired frame callbacks
    pub(crate) frame_callbacks: TinyVecDeq<[u32; 4]>,
    /// A stack of child surfaces and self.
    stack:                      VecList<StackEntry<S::Token>>,
    /// The position of this surface state in its own stack.
    pub(crate) stack_index:     Index<StackEntry<S::Token>>,
    buffer:                     Option<AttachedBuffer<S::Buffer>>,
    /// Scale of the buffer, a fraction with a denominator of 120
    buffer_scale:               u32,
    role_state:                 Option<Box<dyn roles::State>>,
}

/// Pending changes to a surface state
pub struct PendingState<S: Shell> {
    frame_callbacks: TinyVecDeq<[u32; 4]>,
    buffer:          Option<AttachedBuffer<S::Buffer>>,
    buffer_scale:    Option<u32>,
    stack:           VecList<StackEntry<S::Token>>,
    role_state:      Option<Box<dyn roles::State>>,
}

impl<S: Shell> Debug for PendingState<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use super::buffers::BufferLike;
        f.debug_struct("PendingSurfaceState")
            .field("buffer", &self.buffer.as_ref().map(|b| b.inner.object_id()))
            .field("buffer_scale", &self.buffer_scale)
            .field("stack", &self.stack)
            .field("role_state", &self.role_state)
            .field("frame_callbacks", &self.frame_callbacks)
            .finish()
    }
}

impl<S: Shell> PendingState<S> {
    fn set_role_state<T: roles::State>(&mut self, state: T) {
        self.role_state = Some(Box::new(state));
    }

    pub(crate) fn buffer(&self) -> Option<&AttachedBuffer<S::Buffer>> {
        self.buffer.as_ref()
    }

    pub(crate) fn damage_buffer(&self) {
        use super::buffers::BufferLike;
        if let Some(buffer) = self.buffer.as_ref() {
            buffer.inner.damage()
        }
    }

    pub(crate) fn stack_mut(&mut self) -> &mut VecList<StackEntry<S::Token>> {
        &mut self.stack
    }

    pub(crate) fn set_buffer_scale(&mut self, scale: u32) {
        self.buffer_scale = Some(scale);
    }

    /// Set the buffer.
    pub(crate) fn set_buffer_from_object(&mut self, buffer: &objects::Buffer<S::Buffer>) {
        self.buffer = Some(buffer.buffer.attach());
    }

    /// Add a frame callback.
    pub(crate) fn frame_callback_mut(&mut self) -> &mut TinyVecDeq<[u32; 4]> {
        &mut self.frame_callbacks
    }
}

impl<S: Shell> std::fmt::Debug for State<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::shell::buffers::BufferLike;
        f.debug_struct("SurfaceState")
            .field("surface", &self.surface.upgrade().map(|s| s.object_id()))
            .field("frame_callbacks", &self.frame_callbacks)
            .field("stack", &self.stack)
            .field("buffer", &self.buffer.as_ref().map(|b| b.inner.object_id()))
            .field("buffer_scale", &self.buffer_scale_f32())
            .field("role_state", &self.role_state)
            .finish()
    }
}

impl<S: Shell> Default for State<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Shell> State<S> {
    /// Create a new surface state
    fn new() -> Self {
        let mut stack = VecList::new();
        let stack_index = stack.push_back(StackEntry::Self_);
        Self {
            frame_callbacks: Default::default(),
            stack,
            stack_index,
            buffer: None,
            buffer_scale: 120,
            role_state: None,
            surface: Weak::new(),
        }
    }

    /// Get a weak reference to the surface owning this state
    #[inline]
    pub fn weak_surface(&self) -> Weak<Surface<S>> {
        self.surface.clone()
    }

    /// Get a reference to the surface owning this state
    #[inline]
    pub fn surface(&self) -> Option<Rc<Surface<S>>> {
        self.surface.upgrade()
    }

    /// Create a new copy of this surface state, with `changes` applied.
    fn apply_pending(&self, changes: &mut PendingState<S>) -> Self {
        if let Some(buffer) = changes.buffer.as_ref() {
            use crate::shell::buffers::BufferLike;
            buffer.inner.acquire();
        }
        let role_state = changes
            .role_state
            .take()
            .or_else(|| self.role_state.as_ref().map(|r| dyn_clone::clone_box(&**r)));
        let mut frame_callbacks = self.frame_callbacks.clone();
        frame_callbacks.extend(changes.frame_callbacks.drain(..));
        Self {
            frame_callbacks,
            surface: self.surface.clone(),
            stack: changes.stack.clone(),
            stack_index: self.stack_index,
            buffer: changes.buffer.take(),
            buffer_scale: changes.buffer_scale.unwrap_or(self.buffer_scale),
            role_state,
        }
    }

    /// Returns the token of the parent surface state of this surface state, if
    /// any. None if this surface is not a subsurface.
    pub fn parent(&self) -> Option<S::Token> {
        let role_state = self.role_state.as_ref()?;
        let role_state = (&**role_state as &dyn Any)
            .downcast_ref::<roles::subsurface::State<S::Token>>()?;
        role_state.parent
    }

    /// Sets the parent token in the subsurface role state of this surface
    /// state. If this surface is not a subsurface, returns false, otherwise
    /// returns true.
    pub fn set_parent(&mut self, parent: Option<S::Token>) -> bool {
        self.role_state
            .as_mut()
            .and_then(|role_state| {
                (&mut **role_state as &mut dyn Any)
                    .downcast_mut::<roles::subsurface::State<S::Token>>()
            })
            .map(|role_state| {
                role_state.parent = parent;
                true
            })
            .unwrap_or(false)
    }

    /// Find the top of the subtree rooted at this surface. Returns the token,
    /// and it's offset relative to this surface.
    pub fn top(mut this: S::Token, shell: &S) -> (S::Token, Point<i32, coords::Surface>) {
        let mut offset = Point::new(0, 0);
        loop {
            let next = shell.get(this);
            let first = next.stack.front().unwrap();
            // `top` is the  next surface in `self`'s stack, but it isn't necessarily the
            // next surface in the entire subtree stack. Because `top`
            // itself can have a stack. So we need to recursively find the top most surface
            // in `top`'s stack. Unless `top` points to `Self_` in which case we are done.
            match first {
                StackEntry::Self_ => break,
                &StackEntry::Subsurface { token, position } => {
                    this = token;
                    offset += position;
                },
            }
        }
        (this, offset)
    }

    /// Find the bottom of the subtree rooted at this surface. Returns the
    /// token, and it's offset relative to this surface.
    ///
    /// # Example
    ///
    /// Say surface A has stack "B A C D", and surface B has stack "E D F G".
    /// Then `A.bottom()` will return E. Because B is the bottom
    /// of A's immediate stack, and the bottom surface of B's stack is E.
    pub fn bottom(mut this: S::Token, shell: &S) -> (S::Token, Point<i32, coords::Surface>) {
        let mut offset = Point::new(0, 0);
        loop {
            let next = shell.get(this);
            let last = next.stack.back().unwrap();
            match last {
                StackEntry::Self_ => break, /* `bottom` is the bottom of its own stack, so *
                                                    * we don't need to keep descending. */
                &StackEntry::Subsurface { token, position } => {
                    this = token;
                    offset += position;
                },
            }
        }
        (this, offset)
    }

    /// The the surface on top of the `index` surface in the subtree rooted at
    /// this surface. Returns the token, and it's offset relative to this
    /// surface.
    ///
    /// # Example
    ///
    /// Say surface A has stack "B A C D", and surface D has stack "E D F G".
    /// Then `A.next_in_stack(C)` will return E. Because D is the next surface
    /// of C in A's stack, and the top-most surface of D's subtree is E.
    pub fn next_in_stack(
        this: S::Token,
        index: StackIndex<S::Token>,
        shell: &S,
    ) -> Option<(S::Token, Point<i32, coords::Surface>)> {
        let this_surface = shell.get(this);
        let next_index = this_surface.stack.get_next_index(index.0);
        if let Some(next_index) = next_index {
            // Safety: next_index is a valid index returned by
            // get_next_index/get_previous_index
            let next_child = unsafe { this_surface.stack.get_unchecked(next_index) };
            match next_child {
                StackEntry::Self_ => Some((this, Point::new(0, 0))),
                &StackEntry::Subsurface { token, position } => {
                    let (top, offset) = Self::top(token, shell);
                    Some((top, offset + position))
                },
            }
        } else {
            None
        }
    }

    /// The the surface beneath the `index` surface in the subtree rooted at
    /// this surface. Returns the token, and it's offset relative to this
    /// surface.
    ///
    /// # Example
    ///
    /// Say surface A has stack "B A C D", and surface C has stack "E C F G".
    /// Then `A.prev_in_stack(D)` will return G. Because C is the previous
    /// surface of D in A's stack, and the bottom most surface of C's stack
    /// is G.
    pub fn prev_in_stack(
        this: S::Token,
        index: StackIndex<S::Token>,
        shell: &S,
    ) -> Option<(S::Token, Point<i32, coords::Surface>)> {
        let this_surface = shell.get(this);
        let prev_index = this_surface.stack.get_previous_index(index.0);
        if let Some(prev_index) = prev_index {
            let prev_child = unsafe { this_surface.stack.get_unchecked(prev_index) };
            match prev_child {
                StackEntry::Self_ => Some((this, Point::new(0, 0))),
                &StackEntry::Subsurface { token, position } => {
                    let (bottom, offset) = Self::bottom(token, shell);
                    Some((bottom, offset + position))
                },
            }
        } else {
            None
        }
    }

    /// Return the buffer scale
    #[inline]
    pub fn buffer_scale_f32(&self) -> Scale<NotNan<f32>> {
        use num_traits::AsPrimitive;
        let scale: NotNan<f32> = self.buffer_scale.as_();
        Scale::uniform(scale / 120.0)
    }

    /// Set the buffer scale
    #[inline]
    pub fn set_buffer_scale(&mut self, scale: u32) {
        self.buffer_scale = scale;
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

    /// Get a reference to the buffer.
    pub fn buffer(&self) -> Option<&Rc<S::Buffer>> {
        self.buffer.as_ref().map(|b| &b.inner)
    }

    /// Set role related state.
    pub fn set_role_state<T: roles::State>(&mut self, state: T) {
        self.role_state = Some(Box::new(state));
    }

    /// Get a reference to the role related state.
    ///
    /// None if there is no role related state assigned to this surface.
    /// Some(None) if `T` is not the correct type.
    pub fn role_state<T: roles::State>(&self) -> Option<Option<&T>> {
        self.role_state
            .as_ref()
            .map(|s| (&**s as &dyn Any).downcast_ref::<T>())
    }

    /// Get a mut reference to the set of all unfired frame callbacks attached
    /// to the surface.
    pub(crate) fn frame_callbacks_mut(&mut self) -> &mut TinyVecDeq<[u32; 4]> {
        &mut self.frame_callbacks
    }

    /// Get a unique reference to the role related state.
    ///
    /// None if there is no role related state assigned to this surface.
    /// Some(None) if `T` is not the correct type.
    pub fn role_state_mut<T: roles::State>(&mut self) -> Option<Option<&mut T>> {
        self.role_state
            .as_mut()
            .map(|s| (&mut **s as &mut dyn Any).downcast_mut::<T>())
    }

    /// Assuming surface state `token` is going to be released, scan the tree
    /// for any transient children that might be able to be freed as well,
    /// and append them to `queue`.
    ///
    /// Items found by this function aren't guaranteed to be freed, if some of
    /// them are referenced by a version of the surface state, then they
    /// will be resurrected later, in [`Surface::apply_pending`].
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
                if let &StackEntry::Subsurface {
                    token: child_token, ..
                } = e
                {
                    let child_state = shell.get(child_token);
                    let child_subsurface_state = (&**child_state.role_state.as_ref().unwrap()
                        as &dyn Any)
                        .downcast_ref::<roles::subsurface::State<S::Token>>()
                        .unwrap();
                    let parent = child_subsurface_state.parent;
                    if parent.map(|p| p == token).unwrap_or(true) {
                        // `token` is the oldest parent of child, so we might be able to free this
                        // child, if a newer version of the surface state
                        // doesn't reference it.
                        queue.push(child_token);
                    }
                }
            }
            head += 1;
        }
    }
}

pub(crate) type OutputSet = Rc<RefCell<HashSet<WeakPtr<Output>>>>;

/// An event emitted from a surface when the output it is on changes.
#[derive(Clone, Debug)]
pub(crate) struct OutputEvent(pub(crate) OutputSet);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PointerEvent {
    pub time:      u32,
    pub object_id: u32,
    pub kind:      PointerActivity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeyboardEvent {
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
/// A surface.
pub struct Surface<S: super::Shell> {
    /// The current state of the surface. Once a state is committed to current,
    /// it should not be modified.
    current:              Cell<Option<S::Token>>,
    /// The pending state of the surface, this will be applied to
    /// [`Self::current`] when commit is called
    pending_state:        RefCell<PendingState<S>>,
    role:                 RefCell<Option<Box<dyn roles::Role<S>>>>,
    outputs:              OutputSet,
    output_change_events: single_state::Sender<OutputEvent>,
    pointer_events:       broadcast::Ring<PointerEvent>,
    keyboard_events:      broadcast::Ring<KeyboardEvent>,
    layout_change_events: single_state::Sender<LayoutEvent>,
    object_id:            u32,
}

impl<S: Shell> std::fmt::Debug for Surface<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Surface")
            .field("current", &self.current)
            .field("pending", &self.pending_state)
            .field("object_id", &self.object_id)
            .finish()
    }
}

impl<S: Shell> Drop for Surface<S> {
    fn drop(&mut self) {
        assert!(
            self.current == Default::default(),
            "Surface must be destroyed with Surface::destroy"
        );
    }
}

impl<S: Shell> Surface<S> {
    /// Create a new surface
    #[must_use]
    pub(crate) fn new(
        object_id: NewId,
        shell: &mut S,
        pointer_events: broadcast::Ring<PointerEvent>,
        keyboard_events: broadcast::Ring<KeyboardEvent>,
    ) -> Rc<Self> {
        let surface_state = State::new();
        let pending_state = RefCell::new(PendingState {
            stack:           surface_state.stack.clone(),
            buffer:          None,
            role_state:      None,
            buffer_scale:    None,
            frame_callbacks: Default::default(),
        });
        let state_key = shell.allocate(surface_state);
        let surface = Rc::new(Self {
            current: Cell::new(Some(state_key)),
            pending_state,
            role: Default::default(),
            outputs: Default::default(),
            output_change_events: Default::default(),
            layout_change_events: Default::default(),
            pointer_events,
            keyboard_events,
            object_id: object_id.0,
        });
        shell.get_mut(state_key).surface = Rc::downgrade(&surface);
        surface
    }
}

// TODO: maybe we can unshare Surface, not wrapping it in Rc<>
impl<S: Shell> Surface<S> {
    /// Get the parent surface if this surface has a subsurface role.
    pub fn parent(&self) -> Option<Rc<Self>> {
        let role = self.role::<roles::subsurface::Subsurface<S>>();
        role.map(|r| r.parent().upgrade().unwrap())
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
        let mut pending = self.pending_mut();
        let current = self.current_key();
        tracing::debug!(
            "applying pending state {current:?} state: {:?} pending: {pending:?}",
            shell.get(current)
        );
        scratch_buffer.clear();

        // Find potentially free-able surface states and set their parents to
        // None, so later they can either be resurrected with a new parent, or
        // be freed.
        State::scan_for_freeing(current, shell, scratch_buffer);
        tracing::debug!("potential freeable surface states: {:?}", scratch_buffer);
        for &token in &scratch_buffer[..] {
            // Free-able surface states aren't always subsurfaces, because `self`
            // might be in the list as well. So we ignores the return value of `set_parent`.
            let state = shell.get_mut(token);
            state.set_parent(None);
        }

        // Now, apply changes from `pending`
        let current_state = shell.get(current);
        let new_current_state = current_state.apply_pending(&mut *pending);
        let new_current = shell.allocate(new_current_state);

        // At this point, scratch_buffer contains a list of surface states that
        // potentially can be freed.
        let free_candidates_end = scratch_buffer.len();

        // Go through the new children and resurrect them if they are in the
        // free candidates list.
        scratch_buffer.push(new_current);
        let mut head = free_candidates_end;

        // Recursively update descendants' parent_info now we have committed.
        // A descendent's parent info will be updated if it is None, which means it
        // either hasn't been referenced by a committed surface state yet, or
        // the above freeing process has freed its old parent. In both cases, we
        // can update its parent info to point to the new parent.
        while head < scratch_buffer.len() {
            let token = scratch_buffer[head];
            let state = shell.get(token);
            let child_start = scratch_buffer.len();
            for e in state.stack.iter() {
                if let &StackEntry::Subsurface {
                    token: child_token, ..
                } = e
                {
                    let child_state = shell.get(child_token);
                    if child_state.parent().is_some() {
                        continue
                    }
                    tracing::debug!("{:?} is still reachable", child_token);
                    scratch_buffer.push(child_token);
                }
            }
            for &child_token in &scratch_buffer[child_start..] {
                let child_state = shell.get_mut(child_token);
                child_state.set_parent(Some(new_current));
            }
            head += 1;
        }
        scratch_buffer.truncate(free_candidates_end);
        self.current.set(Some(new_current));
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
    /// applied to the current state when committed. The difference is
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

        let old_current = self.current_key();
        self.apply_pending(shell, scratch_buffer);

        // Call post_commit hooks before we free the old states, they might still need
        // them.
        if let Some(role) = self.role.borrow_mut().as_mut() {
            if role.is_active() {
                role.post_commit(shell, self);
            }
        }
        shell.post_commit(Some(old_current), self.current_key());

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

    /// Return the object ID of this surface inside the object store of the
    /// client owning this surface.
    pub fn object_id(&self) -> u32 {
        self.object_id
    }

    /// Set the current surface state
    pub fn set_current(&self, key: S::Token) {
        self.current.set(Some(key));
    }

    /// Get the current surface state token.
    pub fn current_key(&self) -> S::Token {
        self.current.get().unwrap()
    }

    /// Get a unique reference to the pending surface state.
    pub fn pending_mut(&self) -> RefMut<'_, PendingState<S>> {
        self.pending_state.borrow_mut()
    }

    /// Get a reference to the pending surface state.
    pub fn pending(&self) -> Ref<'_, PendingState<S>> {
        self.pending_state.borrow()
    }

    /// Get a reference to the current surface state.
    pub fn current<'a>(&self, shell: &'a S) -> &'a State<S> {
        shell.get(self.current_key())
    }

    /// Get a unique reference to the current surface state.
    pub fn current_mut<'a>(&self, shell: &'a mut S) -> &'a mut State<S> {
        shell.get_mut(self.current_key())
    }

    /// Returns true if the surface has a role attached. This will keep
    /// returning true even after the role has been deactivated.
    pub fn has_role(&self) -> bool {
        self.role.borrow().is_some()
    }

    /// Returns true if the surface has a role, and that role is active.
    pub fn role_is_active(&self) -> bool {
        self.role
            .borrow()
            .as_ref()
            .map(|r| r.is_active())
            .unwrap_or(false)
    }

    /// Borrow the role object of the surface.
    pub fn role<T: roles::Role<S>>(&self) -> Option<Ref<'_, T>> {
        let role = self.role.borrow();
        Ref::filter_map(role, |r| r.as_ref().and_then(|r| request_ref(&**r))).ok()
    }

    /// Mutably borrow the role object of the surface.
    pub fn role_mut<T: roles::Role<S>>(&self) -> Option<RefMut<'_, T>> {
        let role = self.role.borrow_mut();
        RefMut::filter_map(role, |r| r.as_mut().and_then(|r| request_mut(&mut **r))).ok()
    }

    /// Destroy a surface and its associated resources.
    ///
    /// This function will deactivate the role associated with the surface, if
    /// any. (Although, as specified by wl_surface interface v6, the role
    /// must be destroyed before the surface. We keep the deactivation here
    /// too to support older clients). And also destruct any associated
    /// surface states that become orphaned by destroying the surface.
    ///
    /// # Arguments
    ///
    /// - `shell`: the shell that owns this surface.
    /// - `scratch_buffer`: a scratch buffer used to store the tokens of the
    ///   surface states for going through them.
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
            self.pending_state
        );

        self.deactivate_role(shell);

        if self.current(shell).parent().is_none() {
            tracing::debug!(
                "Surface {:p} is not referenced by any parent, freeing",
                self
            );
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
                let StackEntry::Subsurface {
                    token: child_token, ..
                } = child
                else {
                    continue;
                };
                let child = shell.get_mut(child_token);
                let role_state = child
                    .role_state_mut::<roles::subsurface::State<S::Token>>()
                    .expect("subsurface role state missing")
                    .expect("subsurface role state has unexpected type");
                if role_state.parent == Some(current_key) {
                    role_state.parent = None;
                }

                // We need to deactivate the subsurface role of the child, but we
                // don't have access to child's `Surface` (which could already be
                // released by the way).
                //
                // It should be enough to just set the role `is_active` to false.
                // Ideally we also want to set `Subsurface::parent` to an invalid
                // weak reference, but this `Surface` should be released soon after
                // this function returns.
                role_state.is_active.set(false);
            }
            shell.destroy(current_key);
        }
        self.current.set(None);
    }

    /// Deactivate the role assigned to this surface.
    pub fn deactivate_role(&self, shell: &mut S) {
        tracing::debug!(
            "Deactivating role of surface {:p}, role_state: {:?}",
            self,
            self.current(shell).role_state
        );
        if let Some(role) = self.role.borrow_mut().as_mut() {
            if role.is_active() {
                role.deactivate(shell);
                self.pending_mut().role_state = None;
                shell.role_deactivated(self.current_key(), role.name());
                assert!(!role.is_active());
            }
        };
    }

    /// Clear buffer damage, NOT IMPLEMENTED YET
    pub fn clear_damage(&self) {
        todo!()
    }

    /// Assign a role to this surface.
    pub fn set_role<T: roles::Role<S>>(&self, role: T, shell: &mut S) {
        let role_name = role.name();
        {
            let mut role_mut = self.role.borrow_mut();
            *role_mut = Some(Box::new(role));
        }
        shell.role_added(self.current_key(), role_name);
    }

    /// Get the set of outputs this surface is currently on.
    pub fn outputs(&self) -> Ref<'_, HashSet<WeakPtr<Output>>> {
        self.outputs.borrow()
    }

    /// Mutably borrow the set of outputs this surface is currently on.
    pub fn outputs_mut(&self) -> RefMut<'_, HashSet<WeakPtr<Output>>> {
        self.outputs.borrow_mut()
    }

    /// Send an event notifying that the set of outputs this surface is on has
    /// changed.
    pub fn notify_output_changed(&self) {
        self.output_change_events
            .send(OutputEvent(self.outputs.clone()));
    }

    /// Send an event notifying that the layout of this surface has changed.
    pub fn notify_layout_changed(&self, layout: Layout) {
        self.layout_change_events.send(LayoutEvent(layout));
    }

    /// Send a pointer event
    pub fn pointer_event(&self, event: PointerActivity) {
        let event = PointerEvent {
            time:      crate::time::elapsed().as_millis() as u32,
            object_id: self.object_id,
            kind:      event,
        };
        self.pointer_events.broadcast(event);
    }

    /// Send a keyboard event
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
