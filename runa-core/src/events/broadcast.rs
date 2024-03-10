//! A simple broadcast event source based on [`async_broadcast`]

use std::{future::Future, pin::Pin};

use async_broadcast::TrySendError;
use derive_where::derive_where;
/// An event source implementation based on the [`async_broadcast`]
/// channels.
///
/// This event source is mpmc, so it can be cloned, enabling multiple sender
/// to send at the same time
#[derive(Debug)]
#[derive_where(Clone)]
pub struct Broadcast<E>(
    async_broadcast::Sender<E>,
    async_broadcast::InactiveReceiver<E>,
);

/// An event source implementation based on the [`async_broadcast`]
/// channels.
///
/// Like [`Broadcast`], except when the internal queue is full, the oldest
/// event will be dropped.
///
/// This event source is mpmc, so it can be cloned, enabling multiple sender
/// to send at the same time
#[derive(Debug)]
#[derive_where(Clone)]
pub struct Ring<E>(
    async_broadcast::Sender<E>,
    async_broadcast::InactiveReceiver<E>,
);

impl<E> Default for Broadcast<E> {
    fn default() -> Self {
        let (mut tx, rx) = async_broadcast::broadcast(16);
        tx.set_await_active(false);
        Self(tx, rx.deactivate())
    }
}

impl<E> Ring<E> {
    /// Create a new ring broadcast event source
    pub fn new(capacity: usize) -> Self {
        let (mut tx, rx) = async_broadcast::broadcast(capacity);
        tx.set_await_active(false);
        tx.set_overflow(true);
        Self(tx, rx.deactivate())
    }

    /// Change the number of events the ring event source can hold
    pub fn set_capacity(&mut self, capacity: usize) {
        self.0.set_capacity(capacity);
    }

    /// Return the number of sender associated with this event source
    pub fn sender_count(&self) -> usize {
        // The sender stored in `Ring` is never used for sending, so minus 1 here
        self.0.sender_count()
    }
}

impl<E: Clone> Ring<E> {
    /// Send a new event to all receivers
    pub fn broadcast(&self, msg: E) {
        assert!(!self.0.is_closed());
        let result = self.0.try_broadcast(msg);
        assert!(!matches!(result, Err(TrySendError::Full(_))));
    }
}

impl<E: Clone> Broadcast<E> {
    /// Send a new event to all receivers
    pub fn broadcast(&self, msg: E) -> impl Future<Output = ()> + '_ {
        // The sender is never closed because we hold a inactive receiver.
        assert!(!self.0.is_closed());

        // Here this should only error if there is no active receiver, which
        // is fine.
        async move {
            let _ = self.0.broadcast(msg).await;
        }
    }

    /// Like [`Self::broadcast`], but the returned future doesn't borrow from
    /// `self`.
    pub fn broadcast_owned(&self, msg: E) -> impl Future<Output = ()>
    where
        E: 'static,
    {
        assert!(!self.0.is_closed());
        let sender = self.0.clone();

        async move {
            let _ = sender.broadcast(msg).await;
        }
    }

    /// Like [`Self::broadcast`], but if the queue is full, instead of waiting,
    /// this function will allocate more space in the queue.
    ///
    /// # Warning
    ///
    /// If there are stuck receivers, this will cause unbounded memory growth.
    pub fn broadcast_reserve(&mut self, msg: E) {
        assert!(!self.0.is_closed());

        if self.0.is_full() {
            self.0.set_capacity(self.0.capacity() * 2);
        }

        let result = self.0.try_broadcast(msg);
        assert!(!matches!(result, Err(TrySendError::Full(_))));
    }
}

/// The event stream for Broadcast and RingBroadcast event sources
///
/// A wrapper of the broadcast receiver that deactivates the
/// receiver, without closing the sender, when dropped.
#[derive(Debug)]
pub struct Receiver<E>(Option<async_broadcast::Receiver<E>>);
impl<E> Drop for Receiver<E> {
    fn drop(&mut self) {
        if let Some(receiver) = self.0.take() {
            receiver.deactivate();
        }
    }
}
impl<E: Clone + 'static> futures_core::Stream for Receiver<E> {
    type Item = E;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.0.as_mut().unwrap();
        Pin::new(this).poll_next(cx)
    }
}

impl<E: Clone + 'static> super::EventSource<E> for Broadcast<E> {
    type Source = Receiver<E>;

    fn subscribe(&self) -> Self::Source {
        Receiver(Some(self.0.new_receiver()))
    }
}

impl<E: Clone + 'static> super::EventSource<E> for Ring<E> {
    type Source = Receiver<E>;

    fn subscribe(&self) -> Self::Source {
        Receiver(Some(self.0.new_receiver()))
    }
}
