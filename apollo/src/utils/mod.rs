use std::{
    hash::Hash,
    ops::Deref,
    rc::{Rc, Weak},
};

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

/// A wrapper of `Weak` that implements `Hash` and `Eq` by comparing
/// raw pointer addresses.
pub struct WeakPtr<T>(Weak<T>);

impl<T> std::fmt::Debug for WeakPtr<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeakPtr")
            .field("ptr", &self.0.as_ptr())
            .finish()
    }
}

impl<T> Clone for WeakPtr<T> {
    fn clone(&self) -> Self {
        Self(Weak::clone(&self.0))
    }
}

impl<T> WeakPtr<T> {
    pub fn new(t: &Rc<T>) -> Self {
        Self(Rc::downgrade(t))
    }

    pub fn upgrade(&self) -> Option<Rc<T>> {
        self.0.upgrade()
    }

    pub fn into_inner(self) -> Weak<T> {
        self.0
    }

    pub fn as_weak(&self) -> &Weak<T> {
        &self.0
    }
}

impl<T> PartialEq for WeakPtr<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}

impl<T> Eq for WeakPtr<T> {}

impl<T> Hash for WeakPtr<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(self.0.as_ptr(), state)
    }
}

impl<T> From<Weak<T>> for WeakPtr<T> {
    fn from(value: Weak<T>) -> Self {
        Self(value)
    }
}

#[derive(Debug)]
pub(crate) struct AutoAbort(futures_util::future::AbortHandle);

impl AutoAbort {
    pub fn new(handle: futures_util::future::AbortHandle) -> Self {
        Self(handle)
    }
}

impl Drop for AutoAbort {
    fn drop(&mut self) {
        self.0.abort();
    }
}
