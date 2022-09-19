use std::any::Any;
use std::collections::HashMap;
use std::cell::RefCell;

pub trait InterfaceMeta {
    fn interface(&self) -> &'static str;
    fn as_any(&self) -> &dyn Any;
}

use std::rc::Rc;
/// Per client mapping from object ID to objects.
#[derive(Clone)]
pub struct Store {
    map: RefCell<HashMap<u32, Rc<dyn InterfaceMeta>>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct DebugMap<'a>(&'a HashMap<u32, Rc<dyn InterfaceMeta>>);
        let map = self.map.borrow();
        let debug_map = DebugMap(&map);
        impl std::fmt::Debug for DebugMap<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_map()
                    .entries(self.0.iter().map(|(k, v)| (k, v.interface())))
                    .finish()
            }
        }
        f.debug_struct("Store").field("map", &debug_map).finish()
    }
}

pub trait ObjectStore {
    /// Insert object into the store with the given ID. Returns Ok(()) if successful, Err(T) if the
    /// ID is already in use.
    fn insert<T: InterfaceMeta + 'static>(&self, id: u32, object: T) -> Result<(), T>;
    fn get(&self, id: u32) -> Option<Rc<dyn InterfaceMeta>>;
}

impl Store {
    pub fn new() -> Self {
        Self {
            map: RefCell::new(HashMap::new()),
        }
    }
}

impl ObjectStore for Store {
    fn insert<T: InterfaceMeta + 'static>(&self, object_id: u32, object: T) -> Result<(), T> {
        match self.map.borrow_mut().entry(object_id) {
            std::collections::hash_map::Entry::Occupied(_) => Err(object),
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(Rc::new(object));
                Ok(())
            }
        }
    }
    fn get(&self, object_id: u32) -> Option<Rc<dyn InterfaceMeta>> {
        self.map.borrow().get(&object_id).map(Clone::clone)
    }
}
