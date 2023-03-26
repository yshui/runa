//! Reference implementation of the [`traits::Store`](super::traits::Store) trait.

use std::any::{Any, TypeId};

use derivative::Derivative;
use futures_core::Stream;
use hashbrown::{hash_map, HashMap};
use indexmap::{IndexMap, IndexSet};

use super::{traits, CLIENT_MAX_ID};
use crate::{events, objects};

/// A reference object store implementation
///
/// # Note
///
/// You must call [`Store::clear_for_disconnect`] before the object store is
/// dropped. Otherwise, the object store will panic.
#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub struct Store<Object> {
    map:          HashMap<u32, Object>,
    by_type:      HashMap<TypeId, (Box<dyn Any>, IndexSet<u32>)>,
    /// Next ID to use for server side object allocation
    #[derivative(Default(value = "CLIENT_MAX_ID + 1"))]
    next_id:      u32,
    /// Number of server side IDs left
    #[derivative(Default(value = "u32::MAX - CLIENT_MAX_ID"))]
    ids_left:     u32,
    event_source: events::aggregate::Sender<UpdatesAggregate>,
}

impl<Object: objects::AnyObject> Store<Object> {
    #[inline]
    fn remove(&mut self, id: u32) -> Option<Object> {
        let object = self.map.remove(&id)?;
        let type_id = objects::AnyObject::type_id(&object);
        let interface = object.interface();
        match self.by_type.entry(type_id) {
            hash_map::Entry::Occupied(mut v) => {
                let (_, ids) = v.get_mut();
                let removed = ids.remove(&id);
                assert!(removed);
                if ids.is_empty() {
                    v.remove();
                }
            },
            hash_map::Entry::Vacant(_) => panic!("Incosistent object store state"),
        };

        self.event_source.send(traits::StoreUpdate {
            kind:      traits::StoreUpdateKind::Removed { interface },
            object_id: id,
        });
        Some(object)
    }

    #[inline]
    fn try_insert_with<F: FnOnce() -> Object>(
        &mut self,
        id: u32,
        f: F,
    ) -> Option<(&mut Object, &mut dyn Any)> {
        let entry = self.map.entry(id);
        match entry {
            hash_map::Entry::Occupied(_) => None,
            hash_map::Entry::Vacant(v) => {
                let object = f();
                let interface = object.interface();
                let type_id = objects::AnyObject::type_id(&object);
                let (state, ids) = self
                    .by_type
                    .entry(type_id)
                    .or_insert_with(|| (object.new_singleton_state(), IndexSet::new()));
                ids.insert(id);
                let ret = v.insert(object);
                self.event_source.send(traits::StoreUpdate {
                    kind:      traits::StoreUpdateKind::Inserted { interface },
                    object_id: id,
                });
                Some((ret, &mut **state))
            },
        }
    }
}

#[derive(Debug, Default)]
struct UpdatesAggregate {
    /// Map object ID to the diff that needs to be applied.
    events: IndexMap<u32, traits::StoreUpdateKind>,
}

impl Iterator for UpdatesAggregate {
    type Item = traits::StoreUpdate;

    fn next(&mut self) -> Option<Self::Item> {
        self.events
            .pop()
            .map(|(object_id, kind)| traits::StoreUpdate { object_id, kind })
    }
}

impl UpdatesAggregate {
    fn extend_one(&mut self, id: u32, item: traits::StoreUpdateKind) {
        use indexmap::map::Entry;
        match self.events.entry(id) {
            Entry::Vacant(v) => {
                v.insert(item);
            },
            Entry::Occupied(mut o) => {
                use traits::StoreUpdateKind::*;
                match (o.get(), &item) {
                    (Inserted { .. }, Removed { .. }) => {
                        // Inserted + Removed = ()
                        o.remove();
                    },
                    (
                        Removed {
                            interface: old_interface,
                        },
                        Inserted {
                            interface: new_interface,
                        },
                    ) => {
                        // Removed + Inserted = Replaced
                        o.insert(Replaced {
                            old_interface,
                            new_interface,
                        });
                    },
                    (Replaced { old_interface, .. }, Removed { .. }) => {
                        // Replaced + Removed = Removed + Inserted + Removed = Removed
                        o.insert(Removed {
                            interface: old_interface,
                        });
                    },
                    (Inserted { .. }, Inserted { .. }) |
                    (Removed { .. }, Removed { .. }) |
                    (Replaced { .. }, Inserted { .. }) => {
                        // Inserted + Inserted =  Removed + Removed = !
                        panic!("Object {id} inserted or removed twice");
                    },
                    // A replaced event should never be sent directly, it should only be generated
                    // from aggregating a removed and an inserted event.
                    (_, Replaced { .. }) => unreachable!(),
                }
            },
        }
    }
}

impl Extend<traits::StoreUpdate> for UpdatesAggregate {
    fn extend<T: IntoIterator<Item = traits::StoreUpdate>>(&mut self, iter: T) {
        for event in iter {
            self.extend_one(event.object_id, event.kind);
        }
    }
}

impl<O> Store<O> {
    /// Remove all objects from the store. MUST be called before the store is
    /// dropped, to ensure `on_disconnect` is called for all objects.
    pub fn clear_for_disconnect<Ctx>(&mut self, server_ctx: &mut Ctx::ServerContext)
    where
        Ctx: traits::Client,
        O: objects::AnyObject + objects::Object<Ctx>,
    {
        tracing::debug!("Clearing store for disconnect");
        for (_, (mut state, ids)) in self.by_type.drain() {
            for id in ids {
                let obj = self.map.get_mut(&id).unwrap();
                tracing::debug!(
                    "Calling on_disconnect for object {id}, interface {}",
                    obj.interface()
                );
                obj.on_disconnect(server_ctx, &mut *state);
            }
        }
        self.map.clear();
        self.ids_left = u32::MAX - CLIENT_MAX_ID;
        self.next_id = CLIENT_MAX_ID + 1;
    }
}

impl<Object> Drop for Store<Object> {
    fn drop(&mut self) {
        assert!(self.map.is_empty(), "Store not cleared before drop");
    }
}

impl<O: objects::AnyObject> events::EventSource<traits::StoreUpdate> for Store<O> {
    type Source = impl Stream<Item = traits::StoreUpdate> + 'static;

    fn subscribe(&self) -> Self::Source {
        self.event_source.subscribe()
    }
}

impl<O: objects::AnyObject> traits::Store<O> for Store<O> {
    type ByType<'a, T> = impl Iterator<Item = (u32, &'a T)> + 'a where O: 'a, T: objects::MonoObject + 'a;
    type IdsByType<'a, T> = impl Iterator<Item = u32> + 'a where O: 'a, T: 'static;

    #[inline]
    fn insert<T: Into<O> + 'static>(&mut self, object_id: u32, object: T) -> Result<&mut T, T> {
        if object_id > CLIENT_MAX_ID {
            return Err(object)
        }

        let mut orig = Some(object);
        let ret = Self::try_insert_with(self, object_id, || orig.take().unwrap().into());
        if let Some(orig) = orig {
            Err(orig)
        } else {
            Ok(ret.unwrap().0.cast_mut().unwrap())
        }
    }

    #[inline]
    fn insert_with_state<T: Into<O> + objects::MonoObject + 'static>(
        &mut self,
        object_id: u32,
        object: T,
    ) -> Result<(&mut T, &mut T::SingletonState), T> {
        if object_id > CLIENT_MAX_ID {
            return Err(object)
        }

        let mut orig = Some(object);
        let ret = Self::try_insert_with(self, object_id, || orig.take().unwrap().into());
        if let Some(orig) = orig {
            Err(orig)
        } else {
            let (obj, state) = ret.unwrap();
            let state = state.downcast_mut().unwrap();
            Ok((obj.cast_mut().unwrap(), state))
        }
    }

    #[inline]
    fn allocate<T: Into<O> + 'static>(&mut self, object: T) -> Result<(u32, &mut T), T> {
        if self.ids_left == 0 {
            // Store full
            return Err(object)
        }

        let mut curr = self.next_id;

        // Find the next unused id
        loop {
            if !self.map.contains_key(&curr) {
                break
            }
            if curr == u32::MAX {
                curr = CLIENT_MAX_ID + 1;
            } else {
                curr += 1;
            }
        }

        self.next_id = if curr == u32::MAX {
            CLIENT_MAX_ID + 1
        } else {
            curr + 1
        };
        self.ids_left -= 1;

        let inserted = Self::try_insert_with(self, curr, || object.into()).unwrap();
        Ok((curr, inserted.0.cast_mut().unwrap()))
    }

    fn remove(&mut self, object_id: u32) -> Option<O> {
        if object_id > CLIENT_MAX_ID {
            self.ids_left += 1;
        }
        Self::remove(self, object_id)
    }

    fn get_state<T: objects::MonoObject>(&self) -> Option<&T::SingletonState> {
        self.by_type
            .get(&std::any::TypeId::of::<T>())
            .map(|(s, _)| s.downcast_ref().unwrap())
    }

    fn get_state_mut<T: objects::MonoObject>(&mut self) -> Option<&mut T::SingletonState> {
        self.by_type
            .get_mut(&std::any::TypeId::of::<T>())
            .map(|(s, _)| s.downcast_mut().unwrap())
    }

    fn get_with_state<T: objects::MonoObject>(
        &self,
        id: u32,
    ) -> Result<(&T, &T::SingletonState), traits::GetError> {
        let o = self.map.get(&id).ok_or(traits::GetError::IdNotFound(id))?;
        let obj = o.cast::<T>().ok_or(traits::GetError::TypeMismatch(id))?;
        let state = self
            .by_type
            .get(&std::any::TypeId::of::<T>())
            .map(|(s, _)| s.downcast_ref().unwrap())
            .unwrap();
        Ok((obj, state))
    }

    fn get_with_state_mut<'a, T: objects::MonoObject>(
        &'a mut self,
        id: u32,
    ) -> Result<(&'a mut T, &'a mut T::SingletonState), traits::GetError> {
        let o: &'a mut O = self
            .map
            .get_mut(&id)
            .ok_or(traits::GetError::IdNotFound(id))?;
        let obj = o
            .cast_mut::<T>()
            .ok_or(traits::GetError::TypeMismatch(id))?;
        let state = self
            .by_type
            .get_mut(&std::any::TypeId::of::<T>())
            .map(|(s, _)| s.downcast_mut().unwrap())
            .unwrap();
        Ok((obj, state))
    }

    fn get<T: 'static>(&self, object_id: u32) -> Result<&T, traits::GetError> {
        let o = self
            .map
            .get(&object_id)
            .ok_or(traits::GetError::IdNotFound(object_id))?;
        o.cast().ok_or(traits::GetError::TypeMismatch(object_id))
    }

    fn get_mut<T: 'static>(&mut self, id: u32) -> Result<&mut T, traits::GetError> {
        let o = self
            .map
            .get_mut(&id)
            .ok_or(traits::GetError::IdNotFound(id))?;
        o.cast_mut().ok_or(traits::GetError::TypeMismatch(id))
    }

    fn contains(&self, id: u32) -> bool {
        self.map.contains_key(&id)
    }

    fn try_insert_with(&mut self, id: u32, f: impl FnOnce() -> O) -> Option<&mut O> {
        if id > CLIENT_MAX_ID {
            return None
        }
        Self::try_insert_with(self, id, f).map(|(o, _)| o)
    }

    fn try_insert_with_state<T: Into<O> + objects::MonoObject + 'static>(
        &mut self,
        id: u32,
        f: impl FnOnce(&mut T::SingletonState) -> T,
    ) -> Option<(&mut T, &mut T::SingletonState)> {
        let entry = self.map.entry(id);
        match entry {
            hash_map::Entry::Occupied(_) => None,
            hash_map::Entry::Vacant(v) => {
                let type_id = std::any::TypeId::of::<T>();

                let (state, ids) = self
                    .by_type
                    .entry(type_id)
                    .or_insert_with(|| (Box::new(T::new_singleton_state()), IndexSet::new()));

                ids.insert(id);

                let state = (**state).downcast_mut().unwrap();
                let object = f(state);

                let ret = v.insert(object.into());

                self.event_source.send(traits::StoreUpdate {
                    kind:      traits::StoreUpdateKind::Inserted {
                        interface: T::INTERFACE,
                    },
                    object_id: id,
                });
                Some((ret.cast_mut().unwrap(), state))
            },
        }
    }

    fn by_type<T: objects::MonoObject>(&self) -> Self::ByType<'_, T> {
        self.by_type
            .get(&std::any::TypeId::of::<T>())
            .into_iter()
            .flat_map(move |(_, ids)| {
                ids.iter()
                    .map(move |id| (*id, self.map.get(id).unwrap().cast().unwrap()))
            })
    }

    fn ids_by_type<T: 'static>(&self) -> Self::IdsByType<'_, T> {
        self.by_type
            .get(&std::any::TypeId::of::<T>())
            .into_iter()
            .flat_map(|(_, ids)| ids.iter().copied())
    }
}
