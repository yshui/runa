pub mod buffers;
pub mod output;
pub mod surface;
pub mod xdg;
use std::{cell::RefCell, rc::Rc};

use wl_protocol::wayland::{wl_keyboard::v8 as wl_keyboard, wl_seat::v8 as wl_seat};
use wl_server::events::EventSource;

#[derive(Clone, Debug)]
pub enum ShellEvent {
    Render,
}

pub trait Shell: Sized + EventSource<ShellEvent> + 'static {
    /// A token to surfaces.
    ///
    /// Eq and PartialEq should compare if the keys point to the same surface
    /// state.
    ///
    /// Tokens to surface states should be reference counted, if a token to
    /// a surface state exists, the surface state should not be freed.
    ///
    /// A token must be released, impls of Shell can choose to panic
    /// if it was dropped it without being released.
    type Token: std::fmt::Debug + Copy + PartialEq + Eq + std::hash::Hash;

    /// A buffer type. We allow a user supplied buffer type instead of `dyn
    /// Buffer` to avoid virutal call overhead, and allow for a more
    /// flexible Buffer trait.
    type Buffer: buffers::BufferLike;

    /// Allocate a SurfaceState and returns a handle to it.
    fn allocate(&mut self, state: surface::SurfaceState<Self>) -> Self::Token;

    /// Release a token.
    fn destroy(&mut self, key: Self::Token);

    /// Get a reference to a SurfaceState by key.
    ///
    /// Returns None if the key is invalid.
    fn get(&self, key: Self::Token) -> &surface::SurfaceState<Self>;

    /// Get a mutable reference to a SurfaceState.
    fn get_mut(&mut self, key: Self::Token) -> &mut surface::SurfaceState<Self>;

    /// Get mutable references to multiple SurfaceStates.
    ///
    /// # Panic
    ///
    /// Panics if any of the keys are invalid, or if any two of the keys are
    /// equal.
    fn get_disjoint_mut<const N: usize>(
        &mut self,
        keys: [Self::Token; N],
    ) -> [&mut surface::SurfaceState<Self>; N];

    /// Callback which is called when a role is added to a surface corresponds
    /// to the given surface state. A role can be attached using a committed
    /// state or a pending state, and they should have the same effects.
    ///
    /// # Panic
    ///
    /// Panics if the handle is invalid.
    fn role_added(&mut self, key: Self::Token, role: &'static str);
    fn role_deactivated(&mut self, key: Self::Token, role: &'static str);

    /// A commit happened on the surface which used to have surface state `old`.
    /// The new state is `new`. If `old` is None, this is the first commit
    /// on the surface. After this call returns, `new` is considered currently
    /// committed.
    ///
    /// old can be equal to new, if no changes has been made to double buffered
    /// surface states since the last commit.
    ///
    /// Note, for synced subsurface, this is called when `new` became cached
    /// state.
    ///
    /// # Panic
    ///
    /// This function is allowed to panic if either handle is invalid. Or if
    /// `old` has never been committed before.
    fn post_commit(&mut self, old: Option<Self::Token>, new: Self::Token);
}

pub trait HasShell: buffers::HasBuffer {
    type Shell: Shell<Buffer = <Self as buffers::HasBuffer>::Buffer>;
    fn shell(&self) -> &RefCell<Self::Shell>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepeatInfo {
    pub rate:  i32,
    pub delay: i32,
}

#[derive(Clone, Debug)]
pub struct Keymap {
    pub format: wl_keyboard::enums::KeymapFormat,
    pub fd:     Rc<std::os::unix::io::OwnedFd>,
    pub size:   u32,
}

#[derive(Clone, Debug)]
pub enum SeatEvent {
    KeymapChanged(Keymap),
    RepeatInfoChanged(RepeatInfo),
}

pub trait Seat: EventSource<SeatEvent> {
    fn capabilities(&self) -> wl_seat::enums::Capability;
    fn repeat_info(&self) -> RepeatInfo;
    fn keymap(&self) -> &Keymap;
    fn name(&self) -> &str;
}
