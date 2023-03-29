//! Utilities (we might need to separate this out to another crate).

use std::{
    hash::Hash,
    ops::Deref,
    rc::{Rc, Weak},
};

pub mod geometry;

/// Wrapper of `futures_util::stream::AbortHandle` that automatically abort the
/// stream when dropped.
#[derive(Debug)]
pub(crate) struct AutoAbort(futures_util::stream::AbortHandle);

impl From<futures_util::stream::AbortHandle> for AutoAbort {
    fn from(value: futures_util::stream::AbortHandle) -> Self {
        Self(value)
    }
}

impl Drop for AutoAbort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A wrapper of `Rc` that implements `Hash` and `Eq` by comparing
/// raw pointer addresses.
#[derive(Debug)]
pub(crate) struct RcPtr<T>(Rc<T>);

#[allow(dead_code)] // anticipating future use
impl<T> RcPtr<T> {
    /// Create a new `RcPtr` from a value.
    pub(crate) fn new(value: T) -> Self {
        Self(Rc::new(value))
    }

    /// Return the inner `Rc`.
    pub(crate) fn into_inner(self) -> Rc<T> {
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
    /// Create a `WeakPtr` by downgrading a `Rc`.
    pub fn from_rc(t: &Rc<T>) -> Self {
        Self(Rc::downgrade(t))
    }

    /// Create an empty `WeakPtr`.
    pub fn new() -> Self {
        Self(Weak::new())
    }

    /// Attempt to upgrade the `WeakPtr` to an `Rc`.
    pub fn upgrade(&self) -> Option<Rc<T>> {
        self.0.upgrade()
    }

    /// Return the inner `Weak`.
    pub fn into_inner(self) -> Weak<T> {
        self.0
    }

    /// Get a reference to the inner `Weak`.
    pub const fn as_weak(&self) -> &Weak<T> {
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

pub(crate) mod stream {
    use std::{
        cell::RefCell,
        pin::Pin,
        rc::Rc,
        task::{Context, Poll, Waker},
    };

    use derivative::Derivative;
    use futures_core::{FusedStream, Stream};
    #[derive(Derivative, Debug)]
    #[derivative(Default(bound = ""))]
    struct ReplaceInner<S> {
        stream: Option<S>,
        waker:  Option<Waker>,
    }
    /// A stream whose inner is replaceable.
    ///
    /// The inner can be replaced using the corresponding [`Replace`] handle.
    /// The new stream will be polled the next time the [`Replaceable`]
    /// stream is polled. If the inner is `None`, then `Poll::Pending` is
    /// returned if the corresponding `Replace` handle hasn't been dropped,
    /// otherwise `Poll::Ready(None)` will be returned, since it is no
    /// longer possible to put anything that's not `None` into the inner.
    #[derive(Debug)]
    pub struct Replaceable<S> {
        inner: Rc<RefCell<ReplaceInner<S>>>,
    }

    // TODO: wake up task when inner is replaced
    #[derive(Debug)]
    pub struct Replace<S> {
        inner: Rc<RefCell<ReplaceInner<S>>>,
    }

    impl<S: Stream + Unpin> Stream for Replaceable<S> {
        type Item = S::Item;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let strong = Rc::strong_count(&self.inner);
            let mut inner = self.inner.borrow_mut();
            inner.waker.take();

            let p = inner
                .stream
                .as_mut()
                .map(|s| Pin::new(s).poll_next(cx))
                .unwrap_or(Poll::Ready(None));

            if matches!(p, Poll::Ready(Some(_)) | Poll::Pending) {
                // If p is Pending, normally the inner stream would be responsible for waking
                // the task up, but if the inner stream is replaced before the
                // task is woken up, we still need to notify the task.
                if p.is_pending() && strong > 1 {
                    inner.waker = Some(cx.waker().clone());
                }
                return p
            }

            // p == Poll::Ready(None), replace the stream with None
            inner.stream = None;
            if strong == 1 {
                // Replace handle has been dropped
                Poll::Ready(None)
            } else {
                inner.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    impl<S: Stream + Unpin> FusedStream for Replaceable<S> {
        fn is_terminated(&self) -> bool {
            Rc::strong_count(&self.inner) == 1 && self.inner.borrow().stream.is_none()
        }
    }

    impl<S> Replace<S> {
        /// Replace the stream in the corresponding [`Replaceable`] with the
        /// given stream. Returns the old stream.
        ///
        /// Note the new stream will not be immediately polled, so if there is
        /// currently a task waiting on the corresponding
        /// [`Replaceable`], it will not be woken up. You must arrange for it
        /// to be polled.
        pub fn replace(&self, stream: Option<S>) -> Option<S> {
            if stream.is_some() {
                if let Some(waker) = self.inner.borrow_mut().waker.take() {
                    // Wake up the task so the new stream will be polled.
                    waker.wake();
                }
            }
            std::mem::replace(&mut self.inner.borrow_mut().stream, stream)
        }
    }

    impl<S> Drop for Replace<S> {
        fn drop(&mut self) {
            if let Some(waker) = self.inner.borrow_mut().waker.take() {
                // If we drop the Replace, the corresponding Replaceable could return
                // Poll::Ready(None) if polled again, so we need to wake up the task to poll it
                // again.
                waker.wake();
            }
        }
    }

    /// Create a pair of [`Replaceable`] and [`Replace`]. The `Replace` can be
    /// used to replace the stream in the `Replaceable`.
    pub fn replaceable<S>() -> (Replaceable<S>, Replace<S>) {
        let inner = Rc::new(RefCell::new(ReplaceInner::default()));
        (
            Replaceable {
                inner: inner.clone(),
            },
            Replace { inner },
        )
    }
}
