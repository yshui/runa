pub mod buffers;
pub mod surface;
pub mod xdg;
pub mod output;
use std::cell::RefCell;

pub trait Shell: Sized + 'static {
    /// The key to surfaces. Default value of `Key` must be an invalid key.
    /// Using the default key should always result in an error, or getting None.
    type Key: std::fmt::Debug + Copy + PartialEq + Eq + Default;
    /// A buffer type. We allow a user supplied buffer type instead of `dyn
    /// Buffer` to avoid virutal call overhead, and allow for a more
    /// flexible Buffer trait.
    type Buffer: buffers::Buffer;
    /// Allocate a SurfaceState and returns a handle to it.
    fn allocate(&mut self, state: surface::SurfaceState<Self>) -> Self::Key;
    /// Deallocate a SurfaceState.
    ///
    /// # Panic
    ///
    /// Panics if the handle is invalid.
    fn deallocate(&mut self, key: Self::Key);

    /// Get a reference to a SurfaceState by key.
    ///
    /// Returns None if the key is invalid.
    fn get(&self, key: Self::Key) -> Option<&surface::SurfaceState<Self>>;
    /// Get a mutable reference to a SurfaceState.
    fn get_mut(&mut self, key: Self::Key) -> Option<&mut surface::SurfaceState<Self>>;
    /// Called right after `commit`. `from` is the incoming current state, `to`
    /// is the incoming pending state & the outgoing current state. This
    /// function should call `rotate` function on the surface state, which
    /// will copy the state from `from` to `to`.
    fn rotate(&mut self, to: Self::Key, from: Self::Key);

    /// Callback which is called when a role is added to a surface corresponds
    /// to the given surface state. A role can be attached using a committed
    /// state or a pending state, and they should have the same effects.
    ///
    /// # Panic
    ///
    /// Panics if the handle is invalid.
    fn role_added(&mut self, key: Self::Key, role: &'static str);

    /// A commit happened on the surface which used to have surface state `old`.
    /// The new state is `new`. If `old` is None, this is the first commit
    /// on the surface. After this call returns, `new` is considered currently
    /// committed.
    ///
    /// Note, for synced subsurface, this is called when `new` became cached
    /// state.
    ///
    /// # Panic
    ///
    /// This function is allowed to panic if either handle is invalid. Or if
    /// `old` has never been committed before.
    fn commit(&mut self, old: Option<Self::Key>, new: Self::Key);
    /// Add a listener to be notified when the shell has been rendered.
    /// TODO: change to per-surface listener, like for the configure event
    fn add_render_listener(&self, listener: (wl_server::events::EventHandle, usize));
    fn remove_render_listener(&self, listener: (wl_server::events::EventHandle, usize)) -> bool;
}

pub trait HasShell: buffers::HasBuffer {
    type Shell: Shell<Buffer = <Self as buffers::HasBuffer>::Buffer>;
    fn shell(&self) -> &RefCell<Self::Shell>;
}

pub type ShellOf<T> = <T as HasShell>::Shell;
