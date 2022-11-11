pub mod buffers;
pub mod surface;
pub mod xdg;
use std::cell::RefCell;

use dlv_list::{Index, VecList};
use slotmap::{DefaultKey, SlotMap};
use wl_server::server::Server;
use derivative::Derivative;

pub trait Shell: Sized + 'static {
    /// The key to surfaces. Default value of `Key` must be an invalid key.
    /// Using the default key should always result in an error, or getting None.
    type Key: std::fmt::Debug + Copy + PartialEq + Eq + Default;
    type Buffer: buffers::Buffer;
    /// Allocate a SurfaceState and returns a handle to it.
    fn allocate(&mut self, state: surface::SurfaceState<Self>) -> Self::Key;
    /// Deallocate a SurfaceState.
    ///
    /// # Panic
    ///
    /// Panics if the handle is invalid.
    fn deallocate(&mut self, key: Self::Key);

    fn get(&self, key: Self::Key) -> Option<&surface::SurfaceState<Self>>;
    /// Get a mutable reference to a SurfaceState.
    fn get_mut(&mut self, key: Self::Key) -> Option<&mut surface::SurfaceState<Self>>;
    /// Called right after `commit`. `from` is the incoming current state, `to`
    /// is the incoming pending state. This function should call `rotate`
    /// function on the surface state, which will copy the state from `from`
    /// to `to`.
    fn rotate(&mut self, to: Self::Key, from: Self::Key);

    /// Callback which is called when a role is added to a surface corresponds
    /// to the given surface state. A role can be attached using a committed
    /// state or a pending state.
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
}

pub trait HasShell: buffers::HasBuffer {
    type Shell: Shell<Buffer = <Self as buffers::HasBuffer>::Buffer>;
    fn shell(&self) -> &RefCell<Self::Shell>;
}

#[derive(Derivative)]
#[derivative(Default(bound = ""), Debug(bound = ""))]
pub struct DefaultShell<S: buffers::Buffer> {
    storage: SlotMap<DefaultKey, (surface::SurfaceState<Self>, DefaultShellData)>,
    stack: VecList<DefaultKey>,
}

#[derive(Default, Debug)]
pub struct DefaultShellData {
    pub is_current: bool,
    pub stack_index: Option<Index<DefaultKey>>,
}
impl<B: buffers::Buffer> DefaultShell<B> {
    pub fn stack(&self) -> impl DoubleEndedIterator<Item = &DefaultKey> {
        self.stack.iter()
    }
}
impl<B: buffers::Buffer> Shell for DefaultShell<B> {
    type Key = DefaultKey;
    type Buffer = B;

    #[tracing::instrument(skip_all)]
    fn allocate(&mut self, state: surface::SurfaceState<Self>) -> Self::Key {
        self.storage.insert((state, DefaultShellData::default()))
    }

    fn deallocate(&mut self, key: Self::Key) {
        tracing::debug!("Deallocating {:?}", key);
        let (_, data) = self.storage.remove(key).unwrap();
        if let Some(stack_index) = data.stack_index {
            self.stack.remove(stack_index);
        }
    }

    fn get_mut(&mut self, key: Self::Key) -> Option<&mut surface::SurfaceState<Self>> {
        self.storage.get_mut(key).map(|v| &mut v.0)
    }

    fn get(&self, key: Self::Key) -> Option<&surface::SurfaceState<Self>> {
        self.storage.get(key).map(|v| &v.0)
    }

    fn rotate(&mut self, to_key: Self::Key, from_key: Self::Key) {
        let [to, from] = self.storage.get_disjoint_mut([to_key, from_key]).unwrap();
        to.0.rotate_from(&from.0);
    }

    fn role_added(&mut self, key: Self::Key, role: &'static str) {
        if role == "xdg_toplevel" {
            let (_, data) = self.storage.get_mut(key).unwrap();
            data.stack_index = Some(self.stack.push_back(key));
            tracing::debug!("Added to stack: {:?}", self.stack);
        }
    }

    fn commit(&mut self, old: Option<Self::Key>, new: Self::Key) {
        let data = &mut self.storage.get_mut(new).unwrap().1;
        assert!(!data.is_current);
        data.is_current = true;
        if let Some(old) = old {
            let old_data = &mut self.storage.get_mut(old).unwrap().1;
            assert!(old_data.is_current);
            old_data.is_current = false;
            if let Some(index) = old_data.stack_index.take() {
                // If the old state is in the window stack, replace it with the new state.
                *self.stack.get_mut(index).unwrap() = new;
                // Safety: we did the same thing above, with unwrap().
                unsafe { self.storage.get_mut(new).unwrap_unchecked() }.1.stack_index = Some(index);
            }
        }
    }
}
