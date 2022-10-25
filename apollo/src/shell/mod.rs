pub mod buffers;
pub mod surface;
use slotmap::{SlotMap, DefaultKey};

pub trait Shell: Sized + 'static {
    type Key: std::fmt::Debug + Copy + PartialEq + Eq;
    type Data;
    /// Allocate a SurfaceState and returns a handle to it.
    fn allocate(&mut self, state: surface::SurfaceState<Self>) -> Self::Key;
    /// Deallocate a SurfaceState.
    ///
    /// # Panic
    ///
    /// Panics if the handle is invalid.
    fn deallocate(&mut self, key: Self::Key);
    fn new_data(&self) -> Self::Data;

    fn get(&self, key: Self::Key) -> Option<&surface::SurfaceState<Self>>;
    /// Get a mutable reference to a SurfaceState. Implementation might choose
    /// to prevent this for committed surface states, they could return a
    /// None or even panic in this case.
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
    /// Returns a mutable reference to the `old` state, and a immutable
    /// reference to the `new` state.
    ///
    /// # Panic
    ///
    /// This function is allowed to panic if either handle is invalid. Or if
    /// `old` has never been committed before.
    fn commit(&mut self, old: Option<Self::Key>, new: Self::Key);
}

#[derive(Default)]
pub struct DefaultShell {
    storage: SlotMap<DefaultKey, surface::SurfaceState<Self>>,
}
impl std::fmt::Debug for DefaultShell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultShell")
            .finish()
    }
}
pub struct DefaultShellData {
    pub(crate) is_current: bool,
}
impl Shell for DefaultShell {
    type Key = DefaultKey;
    type Data = DefaultShellData;

    fn allocate(&mut self, state: surface::SurfaceState<Self>) -> Self::Key {
        self.storage.insert(state)
    }

    fn deallocate(&mut self, key: Self::Key) {
        self.storage.remove(key);
    }

    fn get_mut(&mut self, key: Self::Key) -> Option<&mut surface::SurfaceState<Self>> {
        self.storage.get_mut(key)
    }

    fn get(&self, key: Self::Key) -> Option<&surface::SurfaceState<Self>> {
        self.storage.get(key)
    }
    fn new_data(&self) -> Self::Data {
        DefaultShellData {
            is_current: false,
        }
    }

    fn rotate(&mut self, to: Self::Key, from: Self::Key) {
        let [to, from] = self.storage.get_disjoint_mut([to, from]).unwrap();
        to.rotate_from(from);
    }

    fn role_added(&mut self, key: Self::Key, role: &'static str) {
        todo!()
    }

    fn commit(&mut self, old: Option<Self::Key>, new: Self::Key) {
        let data = &mut self.storage.get_mut(new).unwrap().data;
        assert!(!data.is_current);
        data.is_current = true;
        if let Some(old) = old {
            let data = &mut self.storage.get_mut(old).unwrap().data;
            assert!(data.is_current);
            data.is_current = false;
        }
        todo!()
    }
}
