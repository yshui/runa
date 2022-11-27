use std::{hash::Hash, ops::Deref, rc::Rc};
pub mod geometry;

/// A wrapper of `Rc` that implements `Hash` and `Eq` by comparing
/// raw pointer addresses.
#[derive(Debug)]
pub struct RcPtr<T>(Rc<T>);

impl<T> RcPtr<T> {
    pub fn new(value: T) -> Self {
        Self(Rc::new(value))
    }

    pub fn into_inner(self) -> Rc<T> {
        self.0
    }
}

impl<T> Hash for RcPtr<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(Rc::as_ptr(&self.0), state)
    }
}

impl<T> PartialEq for RcPtr<T> {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl<T> Eq for RcPtr<T> {}

impl<T> From<Rc<T>> for RcPtr<T> {
    fn from(value: Rc<T>) -> Self {
        Self(value)
    }
}
impl<T> Deref for RcPtr<T> {
    type Target = Rc<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl<T> AsRef<Rc<T>> for RcPtr<T> {
    fn as_ref(&self) -> &Rc<T> {
        &self.0
    }
}
impl<T> AsMut<Rc<T>> for RcPtr<T> {
    fn as_mut(&mut self) -> &mut Rc<T> {
        &mut self.0
    }
}
