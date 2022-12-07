use std::{
    cell::RefCell,
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
#[derive(Debug)]
pub struct WeakPtr<T>(Weak<T>);
impl<T> WeakPtr<T> {
    pub fn new(t: &Rc<T>) -> Self {
        Self(Rc::downgrade(t))
    }

    pub fn upgrade(&self) -> Option<Rc<T>> {
        self.0.upgrade()
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

/// A double buffered value of T.
///
/// This allows concurrent reads and writes to the value. When there are no
/// readers, changes can be made to the value normally. When readers are
/// present, writes to this value will be buffered and NOT immediately visible
/// to the readers.
///
/// Only read side can persist across awaits, the write side should never be
/// borrowed across awaits. Although that's still discouraged, as holding a
/// reader prevents updates from being visible until all readers are dropped.
///
/// When the first reader starts to read, the buffered value is retrieved
/// (swapped) by using the BufSwap trait. The default behavior is copy out the
/// updates then CLEAR the buffer. If the write end is being borrowed while the
/// first reader starts to read, this will panic. But this can be avoided by not
/// borrowing the write side across awaits.
///
/// Using this is better than `tmp = value.borrow().drain().collect()` when you
/// need to read `value` but can't keep the borrow (e.g. because you can't hold
/// a borrow across await). Because this doesn't allocate memory every time.

#[derive(Default)]
pub struct Double<T> {
    front: Rc<T>,
    back:  Rc<RefCell<T>>,
}

impl<T: BufSwap> Double<T> {
    /// Start reading. If this is the first reader, this will retrieve the
    /// updates from the buffer using the BufSwap trait, usually this means
    /// the buffer content will be cleared; otherwise, the value retrieved by
    /// the first reader will be returned.
    pub fn read(&mut self) -> Rc<T> {
        if let Some(read) = self.read_exclusive() {
            read
        } else {
            self.front.clone()
        }
    }

    /// Like [`read`], but makes sure we are the first and only reader,
    /// returns None otherwise.
    pub fn read_exclusive(&mut self) -> Option<Rc<T>> {
        let back = self.back.clone();
        if let Some(front_mut) = Rc::get_mut(&mut self.front) {
            // We are the first reader, swap the buffers.
            front_mut.swap(
                &mut back
                    .try_borrow_mut()
                    .expect("Trying to start read a Double while the write end is borrowed"),
            );
            Some(self.front.clone())
        } else {
            None
        }
    }
}

impl<T> Double<T> {
    /// Get a copy of the write end of Double.
    pub fn write_end(&self) -> Rc<RefCell<T>> {
        self.back.clone()
    }
}

pub trait BufSwap {
    fn swap(&mut self, back: &mut Self);
}

impl<K, V> BufSwap for hashbrown::HashMap<K, V> {
    fn swap(&mut self, back: &mut Self) {
        std::mem::swap(self, back);
        back.clear();
    }
}
