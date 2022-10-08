//! To make the code somewhat generic over thread safety.
//! This is not complete, since there are some use of [`std::cell::Cell`], which is somewhat
//! difficult to transparently replace. Maybe we should avoid Cell.

#[cfg(features = "sync")]
mod sync {
    use std::sync::Mutex;
    #[repr(transparent)]
    pub struct Shared<T>(Mutex<T>);
    #[repr(transparent)]
    #[derive(Clone, Debug)]
    pub struct Rc<T>(std::sync::Arc<T>);
    impl<T> Rc<T> {
        pub fn new(value: T) -> Self {
            Self(std::sync::Arc::new(value))
        }
        pub fn as_ptr(this: &Self) -> *const T {
            std::sync::Arc::as_ptr(&this.0)
        }
        pub fn get_mut(this: &mut Self) -> Option<&mut T> {
            std::sync::Arc::get_mut(&mut this.0)
        }
        pub unsafe fn increment_strong_count(this: *const T) -> Self {
            Self(std::sync::Arc::increment_strong_count(this))
        }
        pub fn strong_count(this: &Self) -> usize {
            std::sync::Arc::strong_count(&this.0)
        }
        pub fn weak_count(this: &Self) -> usize {
            std::sync::Arc::weak_count(&this.0)
        }
        pub fn ptr_eq(this: &Self, other: &Self) -> bool {
            std::sync::Arc::ptr_eq(&this.0, &other.0)
        }
    }
    impl<T: Clone> Rc<T> {
        pub fn make_mut(this: &mut Self) -> &mut T {
            std::sync::Arc::make_mut(&mut this.0)
        }
    }
}

#[cfg(not(features = "sync"))]
mod single_threaded {
    use std::{cell::RefCell, borrow::Borrow, ops::Deref};
    #[repr(transparent)]
    pub struct Shared<T>(RefCell<T>);
    #[repr(transparent)]
    #[derive(Debug)]
    pub struct Rc<T>(std::rc::Rc<T>);
    impl<T> Clone for Rc<T> {
        fn clone(&self) -> Self {
            Self(self.0.clone())
        }
    }
    impl<T> Rc<T> {
        pub fn new(value: T) -> Self {
            Self(std::rc::Rc::new(value))
        }
        pub fn as_ptr(this: &Self) -> *const T {
            std::rc::Rc::as_ptr(&this.0)
        }
        pub fn get_mut(this: &mut Self) -> Option<&mut T> {
            std::rc::Rc::get_mut(&mut this.0)
        }
        pub unsafe fn increment_strong_count(this: *const T) {
            std::rc::Rc::increment_strong_count(this)
        }
        pub fn strong_count(this: &Self) -> usize {
            std::rc::Rc::strong_count(&this.0)
        }
        pub fn weak_count(this: &Self) -> usize {
            std::rc::Rc::weak_count(&this.0)
        }
        pub fn ptr_eq(this: &Self, other: &Self) -> bool {
            std::rc::Rc::ptr_eq(&this.0, &other.0)
        }
    }
    impl<T: Clone> Rc<T> {
        pub fn make_mut(this: &mut Self) -> &mut T {
            std::rc::Rc::make_mut(&mut this.0)
        }
    }
    impl<T> AsRef<T> for Rc<T> {
        fn as_ref(&self) -> &T {
            self.0.as_ref()
        }
    }
    impl<T> Borrow<T> for Rc<T> {
        fn borrow(&self) -> &T {
            self.0.borrow()
        }
    }
    impl<T> Deref for Rc<T> {
        type Target = T;
        fn deref(&self) -> &T {
            self.0.deref()
        }
    }
}
