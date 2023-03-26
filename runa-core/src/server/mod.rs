//! Traits and types related to the server global context.

use std::{future::Future, rc::Rc};

use derivative::Derivative;

use crate::{
    events::{self, EventSource},
    Serial,
};

pub mod traits;

use traits::GlobalsUpdate;

/// Reference implementation of the [`GlobalStore`](traits::GlobalStore) trait.
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct GlobalStore<G> {
    globals:      crate::IdAlloc<Rc<G>>,
    update_event: events::broadcast::Broadcast<GlobalsUpdate<G>>,
}

impl<G> FromIterator<G> for GlobalStore<G> {
    fn from_iter<T: IntoIterator<Item = G>>(iter: T) -> Self {
        let mut id_alloc = crate::IdAlloc::default();
        for global in iter.into_iter() {
            id_alloc.next_serial(Rc::new(global));
        }
        GlobalStore {
            globals:      id_alloc,
            update_event: Default::default(),
        }
    }
}

impl<G: 'static> EventSource<GlobalsUpdate<G>> for GlobalStore<G> {
    type Source =
        <events::broadcast::Broadcast<GlobalsUpdate<G>> as EventSource<GlobalsUpdate<G>>>::Source;

    fn subscribe(&self) -> Self::Source {
        self.update_event.subscribe()
    }
}

/// GlobalStore will notify listeners when globals are added or removed. The
/// notification will be sent to the slot registered as "wl_registry".
impl<G: 'static> traits::GlobalStore<G> for GlobalStore<G> {
    type Iter<'a> = <&'a Self as IntoIterator>::IntoIter where Self: 'a, G: 'a;

    type InsertFut<'a, I> = impl Future<Output = u32> + 'a where Self: 'a, I: 'a + Into<G>;
    type RemoveFut<'a> = impl Future<Output = bool> + 'a where Self: 'a;

    fn insert<'a, I: Into<G> + 'a>(&'a mut self, global: I) -> Self::InsertFut<'_, I> {
        let global = Rc::new(global.into());
        let id = self.globals.next_serial(global.clone());
        async move {
            self.update_event
                .broadcast(GlobalsUpdate::Added(id, global))
                .await;
            id
        }
    }

    fn get(&self, id: u32) -> Option<&Rc<G>> {
        self.globals.get(id)
    }

    fn remove(&mut self, id: u32) -> Self::RemoveFut<'_> {
        let removed = self.globals.expire(id);
        async move {
            if removed {
                self.update_event
                    .broadcast(GlobalsUpdate::Removed(id))
                    .await;
            }
            removed
        }
    }

    fn iter(&self) -> Self::Iter<'_> {
        self.into_iter()
    }
}
impl<'a, G> IntoIterator for &'a GlobalStore<G> {
    type IntoIter = crate::IdIter<'a, Rc<G>>;
    type Item = (u32, &'a Rc<G>);

    fn into_iter(self) -> Self::IntoIter {
        self.globals.iter()
    }
}
