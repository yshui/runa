//! A event source that a single event, and overwrites it when a new event is
//! sent.

use std::{
    cell::{Cell, RefCell},
    future::Future,
    pin::Pin,
    rc::{Rc, Weak},
    task::{
        ready,
        Poll::{self, Ready},
    },
};

use derivative::Derivative;
use futures_core::Stream;

#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
struct Inner<E> {
    event_handle: event_listener::Event,
    state:        RefCell<Option<E>>,
    version:      Cell<u64>,
}

/// An event source whose receivers will receive an event when a state
/// is changed. It's possible for multiple changes to be
/// aggregated into a single event.
#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub struct Sender<E> {
    inner: Rc<Inner<E>>,
}

/// Sender for a single state event source.
impl<E> Sender<E> {
    /// Create a new event source
    pub fn new() -> Self {
        Default::default()
    }

    /// Send a new event to all receivers, replace the sent previous event
    pub fn send(&self, state: E) {
        *self.inner.state.borrow_mut() = Some(state);
        self.inner.version.set(self.inner.version.get() + 1);
        self.inner.event_handle.notify(usize::MAX);
    }

    /// Create a new receiver for this event source
    pub fn new_receiver(&self) -> Receiver<E> {
        Receiver {
            inner:    Rc::downgrade(&self.inner),
            version:  self.inner.version.get(),
            listener: None,
        }
    }
}

/// Receiver for a single state event source.
#[derive(Debug)]
pub struct Receiver<E> {
    inner:    Weak<Inner<E>>,
    listener: Option<event_listener::EventListener>,
    version:  u64,
}

impl<E: Clone> Stream for Receiver<E> {
    type Item = E;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(inner) = this.inner.upgrade() else {
            // All senders are gone
            return Ready(None)
        };
        loop {
            if this.version != inner.version.get() {
                this.listener = None;
                this.version = inner.version.get();
                return Ready(Some(inner.state.borrow().clone().unwrap()))
            }
            let listener = this
                .listener
                .get_or_insert_with(|| inner.event_handle.listen());
            ready!(Pin::new(listener).poll(cx));
        }
    }
}
