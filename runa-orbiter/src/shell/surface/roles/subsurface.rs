//! Role object for `wl_subsurface`.

use std::{
    cell::Cell,
    rc::{Rc, Weak},
};

use derive_where::derive_where;
use dlv_list::Index;
use runa_core::provide_any;
use runa_wayland_protocols::wayland::wl_subsurface;

use crate::{
    shell::{
        surface::{Surface, StackEntry, State as SurfaceState},
        Shell,
    },
    utils::geometry::{coords, Point},
};

/// The `wl_subsurface` role.
///
/// # Note about cache and pending states
///
/// A surface normally has a pending and a current state. Changes are
/// stored in the pending state first, then applied to the current
/// state when `wl_surface.commit` is called.
///
/// Subsurfaces has one more state - the cached state. This state exists if
/// the subsurface is in synchronized mode. In sync mode, commit applies
/// pending state to a cached state, and the cached state is applied to
/// current state when the parent calls commit, if the partent is desynced;
/// otherwise the cached state becomes part of the parent's cached
/// state.
///
/// We can see this as a tree of surface states, rooted at a "top-level"
/// surface, such as a surface with the `xdg_toplevel` role. The root's
/// current state references the children's current states, and the
/// children's current states in turn reference the grand-children's, so
/// on and so forth. When a synced child commits, its current state updates,
/// but it doesn't update its parent's reference to its current state.
/// So the parent still references the previous state, until the parent
/// also commits.
///
/// A complication is when a child is destroyed, either by destroying the
/// surface or deactivating its role, it's immediately removed, without
/// going through the pending or the cached state. We can detect this by
/// checking if the role object is active, while going through the tree
/// of surfaces.
#[derive(Debug)]
#[derive_where(Clone)]
pub struct Subsurface<S: Shell> {
    sync:                   bool,
    inherited_sync:         bool,
    pub(super) is_active:   Rc<Cell<bool>>,
    /// Index of this surface in parent's `stack` list.
    /// Note this index should be stable across parent updates, including
    /// appending to the stack, reordering the stack. a guarantee
    /// from VecList.
    pub(crate) stack_index: Index<StackEntry<S::Token>>,
    pub(super) parent:      Weak<Surface<S>>,
}

#[derive(Debug, Clone)]
pub(in crate::shell) struct State<Token> {
    /// Parent surface *state* of this surface *state*. A surface state is
    /// only considered a parent after it has been committed. This
    /// is different from [`Subsurface::parent`], which is the
    /// `Rc<Surface>` (surface, not surface state) that is the parent of
    /// this surface. A surface can have multiple surface states
    /// each have different parent surface states. But a surface can
    /// have only one parent surface.
    ///
    /// A surface state can have multiple parents because of the sync
    /// mechanism of subsurfaces. i.e. a subsurface can be attached
    /// to a parent, then the parent has its own parent. When
    /// the parent commits, its old state will still be referenced by
    /// the grandparent, and it will have a new cached state.
    /// Both the old state and the new state will be "parents"
    /// of this surface state.
    ///
    /// If that's the case, this field will point to the oldest, still valid
    /// parent. For states visible from a "root" surface (e.g. a
    /// xdg_toplevel), this conveniently forms a path towards the
    /// root's current state.
    pub(in crate::shell) parent: Option<Token>,

    /// See [`Subsurface::stack_index`]
    pub(in crate::shell) stack_index: Index<StackEntry<Token>>,

    /// Whether the corresponding role is active.
    pub(in crate::shell) is_active: Rc<Cell<bool>>,
}
impl<Token: std::fmt::Debug + Clone + 'static> super::State for State<Token> {}
impl<S: Shell> Subsurface<S> {
    /// Attach a surface to a parent surface, and add the subsurface role to
    /// id.
    pub fn attach(parent: Rc<Surface<S>>, surface: Rc<Surface<S>>, shell: &mut S) -> bool {
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
        let mut parent_pending = parent.pending_mut();
        let stack_index = parent_pending
            .stack
            .push_front(StackEntry::Subsurface {
                token:    surface.current_key(),
                position: Point::new(0, 0),
            });
        let is_active = Rc::new(Cell::new(true));
        let role = Self {
            sync: true,
            inherited_sync: true,
            is_active: is_active.clone(),
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
            .pending_mut()
            .set_role_state(State::<S::Token> {
                parent: None,
                stack_index,
                is_active,
            });
        true
    }

    /// Returns a weak reference to the parent surface.
    pub fn parent(&self) -> &Weak<Surface<S>> {
        &self.parent
    }
}

impl<S: Shell> super::Role<S> for Subsurface<S> {
    fn name(&self) -> &'static str {
        wl_subsurface::v1::NAME
    }

    fn is_active(&self) -> bool {
        self.is_active.get()
    }

    fn deactivate(&mut self, _shell: &mut S) {
        tracing::debug!("deactivate subsurface role {}", self.is_active.get());
        if !self.is_active.get() {
            return
        }
        // Deactivating the subsurface role is immediate, but we don't know
        // how many other surface states there are that are referencing this
        // subsurface state, as our ancestors can have any number of "cached"
        // states. And we aren't keeping track of all of them. Instead we
        // mark it inactive, and skip over inactive states when we iterate
        // over the subsurface tree.
        self.is_active.set(false);

        // Remove ourself from parent's pending stack, so when the parent
        // eventually commits, it will drop us.
        let parent = self
            .parent
            .upgrade()
            .expect("surface is destroyed but its state is still being used");
        let mut parent_pending_state = parent.pending_mut();
        parent_pending_state.stack.remove(self.stack_index).unwrap();
        self.parent = Weak::new();
    }

    fn provide<'a>(&'a self, demand: &mut provide_any::Demand<'a>) {
        demand.provide_ref(self);
    }

    fn provide_mut<'a>(&'a mut self, demand: &mut provide_any::Demand<'a>) {
        demand.provide_mut(self);
    }

    fn post_commit(&mut self, shell: &mut S, surface: &Surface<S>) {
        // update the state referenced in parent's pending state's stack.
        let parent = self
            .parent
            .upgrade()
            .expect("surface is destroyed but its state is still being used");

        let mut parent_pending_state = parent.pending_mut();
        let parent_pending_stack_entry = parent_pending_state
            .stack
            .get_mut(self.stack_index)
            .unwrap();
        let StackEntry::Subsurface { token, .. } = parent_pending_stack_entry else {
            panic!("subsurface stack entry has unexpected type")
        };
        *token = surface.current_key();

        // the current state is now referenced by the parent's pending state,
        // clear the parent field. (parent could have been set because pending state was
        // cloned from a previous current state)
        let current = surface.current_mut(shell);
        let role_state = current
            .role_state_mut::<State<S::Token>>()
            .expect("subsurface role state missing")
            .expect("subsurface role state has unexpected type");
        role_state.parent = None;
    }
}

/// Double ended iterator for iterating over a surface and its subsurfaces
/// in the order they are stacked.
///
/// The forward order is from bottom to top. This iterates over the
/// committed states of the surfaces, as defined by `wl_surface.commit`.
pub fn subsurface_iter<S: Shell>(
    root: S::Token,
    s: &S,
) -> impl DoubleEndedIterator<Item = (S::Token, Point<i32, coords::Surface>)> + '_ {
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
                if let Some((next, offset)) =
                    SurfaceState::$next_in_stack(curr_head.0, ret.stack_index.into(), self.shell)
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
                            .role_state::<State<S::Token>>()
                            .expect("subsurface role state missing")
                            .expect("subsurface role state has unexpected type");
                        let parent_key = role_state.parent.unwrap_or_else(|| {
                            panic!(
                                "surface state {curr:?} (key: {:?}) has no parent, but is in a \
                                 stack",
                                curr_head.0
                            )
                        });
                        let parent = self.shell.get(parent_key);
                        let stack_index = role_state.stack_index;
                        offset -= parent.stack.get(stack_index).unwrap().position();

                        if let Some((next, next_offset)) =
                            SurfaceState::$next_in_stack(parent_key, stack_index.into(), self.shell)
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
                    self.$next_maybe_deactivated();
                    let ret = self.shell.get(self.head[0].0);
                    let role_active = ret
                        .role_state::<State<S::Token>>()
                        // If the role state is not SubsurfaceState, or if the role state
                        // doesn't exist, then the surface is top-level.
                        .flatten()
                        .map_or(true, |role_state| role_state.is_active.get());
                    if role_active {
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

    impl<S: Shell> SubsurfaceIter<'_, S> {
        generate_advance!(next_in_stack, next_maybe_deactivated, next, 0);

        generate_advance!(prev_in_stack, prev_maybe_deactivated, prev, 1);
    }

    impl<S: Shell> Iterator for SubsurfaceIter<'_, S> {
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
    impl<S: Shell> DoubleEndedIterator for SubsurfaceIter<'_, S> {
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
            SurfaceState::top(root, s),
            SurfaceState::bottom(root, s),
        ],
        is_empty: false,
    }
}
