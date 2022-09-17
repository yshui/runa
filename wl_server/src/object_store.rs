use futures_lite::Future;
use std::any::Any;
use std::{borrow::Borrow, collections::HashMap};
use wl_io::de::WaylandBufAccess;

pub trait InterfaceMeta {
    fn interface(&self) -> &'static str;
    fn as_any(&self) -> &dyn Any;
}

pub trait MessageBroker<Interfaces, Ctx> {
    type Error;
    type Fut<'a>: Future<Output = Result<(), Self::Error>> + 'a;
    /// Dispatch message to a specific interface based on metadata stored in object_store.
    fn dispatch<'a, R>(
        ctx: &mut Ctx,
        object_store: &mut Store,
        object_id: u32,
        buf: &mut WaylandBufAccess<'a, R>,
    ) -> Self::Fut<'a>
    where
        R: wl_io::AsyncBufReadWithFd;
}
use std::rc::Rc;
/// Per client mapping from object ID to objects.
pub struct Store {
    map: HashMap<u32, Rc<dyn InterfaceMeta>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct DebugMap<'a>(&'a HashMap<u32, Rc<dyn InterfaceMeta>>);
        let debug_map = DebugMap(&self.map);
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
    fn insert<T: InterfaceMeta + 'static>(&mut self, id: u32, object: T);
    fn get(&self, id: u32) -> Option<Rc<dyn InterfaceMeta>>;
}

impl Store {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }
}

impl ObjectStore for Store {
    fn insert<T: InterfaceMeta + 'static>(&mut self, object_id: u32, object: T) {
        self.map.insert(object_id, Rc::new(object));
    }
    fn get(&self, object_id: u32) -> Option<Rc<dyn InterfaceMeta>> {
        self.map.get(&object_id).map(Clone::clone)
    }
}
