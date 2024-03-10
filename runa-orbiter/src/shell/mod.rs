//! Traits and types for the shell.
//!
//! A shell is the user facing component of the compositor. It manages a set of
//! surfaces, and is responsible of presenting them to the user, as well as
//! accepting user interactions. It is in control of all the visual aspects of
//! a compositor.

pub mod buffers;
pub mod output;
pub mod surface;
pub mod xdg;
use std::cell::RefCell;

use runa_core::events::EventSource;
use runa_wayland_protocols::wayland::{wl_keyboard::v9 as wl_keyboard, wl_seat::v9 as wl_seat};

/// Events emitted by a shell
#[derive(Clone, Debug, Copy)]
pub enum ShellEvent {
    /// The current set of surfaces have been rendered to screen.
    ///
    /// TODO: in future, we wants to be finer grained. as one render doesn't
    /// necessarily render all of the surfaces, e.g. some surfaces might not
    /// be visible.
    Render,
}

/// Shell
///
/// This is the fundamental interface to the compositor's shell. A shell needs
/// to support management of surface states, i.e. allocation, deallocation, and
/// access. The allocated states are reference via a token, in order to avoid
/// lifetime and ownership difficulties.
///
/// The shell interface also defines a number of callbacks, which `runa-orbiter`
/// will call in response to various operations done to surfaces.
///
/// The shell is also an event source, it must emit the event defined in
/// [`ShellEvent`] for various interfaces implemented here to function properly.
pub trait Shell: Sized + EventSource<ShellEvent> + 'static {
    /// A token to surfaces.
    ///
    /// Eq and PartialEq should compare if the keys point to the same surface
    /// state.
    ///
    /// A token must be released, impls of Shell can choose to panic
    /// if it was dropped it without being released.
    type Token: std::fmt::Debug + Copy + PartialEq + Eq + std::hash::Hash;

    /// A buffer type. We allow a user supplied buffer type instead of `dyn
    /// Buffer` to avoid virtual call overhead, and allow for a more
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
    fn get_mut(&mut self, key: Self::Token) -> &mut surface::SurfaceState<Self> {
        self.get_disjoint_mut([key])[0]
    }

    /// Get mutable references to multiple SurfaceStates.
    ///
    /// # Panic
    ///
    /// May panic if any of the keys are invalid, or if any two of the keys are
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
    /// May panic if the handle is invalid.
    #[allow(unused_variables)]
    fn role_added(&mut self, key: Self::Token, role: &'static str) {}

    /// Callback that is called when a surface has its assigned role
    /// deactivated.
    ///
    /// # Panic
    ///
    /// May panic if the handle is invalid.
    #[allow(unused_variables)]
    fn role_deactivated(&mut self, key: Self::Token, role: &'static str) {}

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
    /// This function may panic if either handle is invalid. Or if
    /// `old` has never been committed before.
    #[allow(unused_variables)]
    fn post_commit(&mut self, old: Option<Self::Token>, new: Self::Token) {}
}

/// Shell access
///
/// Implemented by the server context (see
/// [`ServerContext`](runa_core::client::traits::Client::ServerContext)) to
/// indicate that it has a shell.
pub trait HasShell: buffers::HasBuffer {
    /// Type of the shell
    type Shell: Shell<Buffer = <Self as buffers::HasBuffer>::Buffer>;

    /// Get a reference to the shell
    fn shell(&self) -> &RefCell<Self::Shell>;
}

/// Keyboard repeat repeat_info
///
/// See `wl_keyboard.repeat_info` for more details.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepeatInfo {
    pub rate:  i32,
    pub delay: i32,
}

/// Keymap
#[derive(Debug)]
pub struct Keymap {
    /// Format of the keymap
    pub format: wl_keyboard::enums::KeymapFormat,
    /// File descriptor of the keymap
    pub fd:     std::os::unix::io::OwnedFd,
    /// Size of the keymap in bytes
    pub size:   u32,
}

/// Events emitted by a seat
///
/// TODO: change to a bitflag struct, and use the aggregate event source.
#[derive(Clone, Debug, Copy)]
pub enum SeatEvent {
    /// The keymap has changed
    KeymapChanged,
    /// The repeat info has changed
    RepeatInfoChanged,
    /// The capabilities of the seat has changed
    CapabilitiesChanged,
    /// The name of the seat has changed
    NameChanged,
}

/// Seat
///
/// A seat is a set of input output devices attached to a computer. This trait
/// is mainly used by interface implementations to get information about mouse
/// and keyboard devices attached.
///
/// This is also an event source, it must emit events when information about the
/// seat changes.
///
/// This needs to be implemented by the
/// [`ServerContext`](runa_core::client::traits::Client::ServerContext).
pub trait Seat: EventSource<SeatEvent> {
    /// Get the capabilities of the seat.
    ///
    /// Whether the seat has a pointer, keyboard, or touch device attached.
    fn capabilities(&self) -> wl_seat::enums::Capability;

    /// Get the repeat info of the keyboard.
    fn repeat_info(&self) -> RepeatInfo;

    /// Get the current keymap.
    fn keymap(&self) -> &Keymap;

    /// Get the name of the seat.
    fn name(&self) -> &str;
}
