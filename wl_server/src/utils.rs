use std::any::Any;

use hashbrown::HashMap;
/// A collection of arbitrary typed values. Can be set and retrieved by
/// their type.
#[derive(Default, Debug)]
pub struct UnboundedAggregate {
    data: HashMap<std::any::TypeId, Box<dyn Any>>,
}

impl UnboundedAggregate {
    /// Safety: if `key` is `TypeId::of::<T>`, then `value` must be of type
    /// `Box<T>`.
    unsafe fn insert(&mut self, key: std::any::TypeId, value: Box<dyn Any>) {
        self.data.insert(key, value);
    }
}

impl UnboundedAggregate {
    pub fn get<T: Any>(&self) -> Option<&T> {
        self.data
            .get(&std::any::TypeId::of::<T>())
            // Safety: `set` ensures that the type is correct
            .map(|d| unsafe { &*(d.as_ref() as *const _ as *const _) })
    }

    pub fn get_mut<T: Any>(&mut self) -> Option<&mut T> {
        self.data
            .get_mut(&std::any::TypeId::of::<T>())
            // Safety: `set` ensures that the type is correct
            .map(|d| unsafe { &mut *(d.as_mut() as *mut _ as *mut _) })
    }

    #[inline]
    pub fn set<T: Any>(&mut self, data: T) {
        // Safety: we make sure entry of `TypeId::of::<T>` definitely has type
        // `T::Data`.
        unsafe { self.insert(std::any::TypeId::of::<T>(), Box::new(data)) }
    }
}
