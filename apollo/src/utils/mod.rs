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

pub mod stream {
    use std::{
        cell::RefCell,
        pin::Pin,
        rc::Rc,
        task::{Context, Poll},
    };

    use futures_core::{FusedStream, Stream};
    /// A stream whose inner is replaceable.
    ///
    /// The inner can be replaced using the corresponding [`Replace`] handle.
    /// The new stream will be polled the next time the [`Replaceable`]
    /// stream is polled. If the inner is `None`, then `Poll::Pending` is
    /// returned if the corresponding `Replace` handle hasn't been dropped,
    /// otherwise `Poll::Ready(None)` will be returned, since it is no
    /// longer possible to put anything that's not `None` into the inner.
    pub struct Replaceable<S> {
        stream: Rc<RefCell<Option<S>>>,
    }

    // TODO: wake up task when inner is replaced
    pub struct Replace<S> {
        stream: Rc<RefCell<Option<S>>>,
    }

    impl<S: Stream + Unpin> Stream for Replaceable<S> {
        type Item = S::Item;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let strong = Rc::strong_count(&self.stream);
            let mut stream = self.stream.borrow_mut();
            let p = stream
                .as_mut()
                .map(|s| Pin::new(s).poll_next(cx))
                .unwrap_or(Poll::Ready(None));

            if matches!(p, Poll::Ready(Some(_)) | Poll::Pending) {
                return p
            }

            // p == Poll::Ready(None), replace the stream with None
            *stream = None;
            if strong == 1 {
                // Replace handle has been dropped
                Poll::Ready(None)
            } else {
                Poll::Pending
            }
        }
    }

    impl<S: Stream + Unpin> FusedStream for Replaceable<S> {
        fn is_terminated(&self) -> bool {
            Rc::strong_count(&self.stream) == 1 && self.stream.borrow().is_none()
        }
    }

    impl<S> Replace<S> {
        /// Replace the stream in the corresponding [`Replaceable`] with the
        /// given stream.
        ///
        /// Note the new stream will not be immediately polled, so if there is
        /// currently a task waiting on the corresponding
        /// [`Replaceable`], it will not be woken up. You must arrange for it
        /// to be polled.
        pub fn replace(&self, stream: Option<S>) -> Option<S> {
            std::mem::replace(&mut self.stream.borrow_mut(), stream)
        }
    }

    /// Create a pair of [`Replaceable`] and [`Replace`]. The `Replace` can be
    /// used to replace the stream in the `Replaceable`.
    pub fn replaceable<S>() -> (Replaceable<S>, Replace<S>) {
        let stream = Rc::new(RefCell::new(None));
        (
            Replaceable {
                stream: stream.clone(),
            },
            Replace { stream },
        )
    }
}
