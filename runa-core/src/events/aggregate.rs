//! An event source that aggregates sent events together for each receiver.
//!
//! This can be used to create a sort of event-based diff update mechanism,
//! where a normal stream-of-event sources are not suitable.
//!
//! A good example is the object store. If store updates are a stream of
//! events, we can imagine a sequence of events like:
//!
//!    - Insert Object { id: 1, interface: wl_surface }
//!    - Remove Object { id: 1 }
//!    - Insert Object { id: 1, interface: wl_buffer }
//!
//! If listener only started reading the events after the third event, they
//! will start processing the first insertion event and find out object 1 is
//! not actually a wl_surface, and be confused.
//!
//! What should really happen is that the first two events should "cancel
//! out" for that listener, and the listener should only see the third one.
//!
//! The aggregate event source will do exactly that.

use std::{
    cell::RefCell,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

use futures_core::{FusedStream, Stream};

#[derive(Debug)]
struct ReceiverInner<E> {
    event: E,
    waker: Option<Waker>,
}

/// A sender for the aggregate event source.
///
/// The type parameter `E` is not the event type, it represents the internal
/// state of the event source. It can be seen as an aggregate of the actual
/// event type.
#[derive(Debug)]
pub struct Sender<E> {
    receivers: RefCell<Vec<Rc<RefCell<ReceiverInner<E>>>>>,
}

impl<E> Default for Sender<E> {
    fn default() -> Self {
        Self {
            receivers: Vec::new().into(),
        }
    }
}

impl<E> Sender<E> {
    /// Create a new sender for a aggregate event source
    pub fn new() -> Self {
        Default::default()
    }
}

impl<E: Default> Sender<E> {
    /// Creata a new receiver for this sender
    pub fn new_receiver(&self) -> Receiver<E> {
        let inner = Rc::new(RefCell::new(ReceiverInner {
            event: E::default(),
            waker: None,
        }));
        self.receivers.borrow_mut().push(inner.clone());
        Receiver {
            inner,
            terminated: false,
        }
    }
}

/// A receiver for the aggregate event source.
///
/// The type parameter `E` is not the event type, it represents the internal
/// state of the event source. It can be seen as an aggregate of the actual
/// event type.
#[derive(Debug)]
pub struct Receiver<E> {
    inner:      Rc<RefCell<ReceiverInner<E>>>,
    terminated: bool,
}

impl<E> Sender<E> {
    /// Send an event to all receivers
    pub fn send<I: Clone>(&self, event: I)
    where
        E: Extend<I>,
    {
        for receiver in self.receivers.borrow().iter() {
            let mut receiver = receiver.borrow_mut();
            receiver.event.extend(Some(event.clone()));
            if let Some(waker) = receiver.waker.take() {
                waker.wake_by_ref();
            }
        }
    }
}

impl<E: Iterator + Default + 'static> super::EventSource<E::Item> for Sender<E> {
    type Source = impl Stream<Item = E::Item> + FusedStream + Unpin;

    fn subscribe(&self) -> Self::Source {
        self.new_receiver()
    }
}

impl<E: Iterator> Stream for Receiver<E> {
    type Item = E::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Self { inner, terminated } = self.get_mut();
        let strong_count = Rc::strong_count(inner);
        let mut inner = inner.borrow_mut();
        // If another task is already polling this receiver, we wake up that task.
        if let Some(old_waker) = inner.waker.take() {
            if !old_waker.will_wake(cx.waker()) {
                old_waker.wake()
            }
        }

        if let Some(item) = inner.event.next() {
            return Poll::Ready(Some(item))
        }

        if strong_count == 1 {
            // We are the only owner, meaning the sender is gone
            *terminated = true;
            return Poll::Ready(None)
        }

        inner.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl<E: Iterator> FusedStream for Receiver<E> {
    fn is_terminated(&self) -> bool {
        self.terminated
    }
}
