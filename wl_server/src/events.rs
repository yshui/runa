use std::{future::Future, pin::Pin};

pub trait EventSource<Event> {
    type Source: futures_util::stream::Stream<Item = Event> + 'static;
    /// Get a stream of events from the event source.
    fn subscribe(&self) -> Self::Source;
}

/// An event source implementation based on the [`async_broadcast`] channels.
#[derive(Debug, Clone)]
pub struct BroadcastEventSource<E>(
    async_broadcast::Sender<E>,
    async_broadcast::InactiveReceiver<E>,
);

// TODO: this is equivalent to `async_broadcast` with `set_overflow(true)` and
// `set_capacity(1)`. Contemplate if we should just use `async_broadcast`
// directly.
pub mod single_state {
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

    /// An event source whose receivers will receive an event when a state is
    /// changed. It's possible for multiple changes to be aggregated into a
    /// single event.
    #[derive(Debug, Derivative)]
    #[derivative(Default(bound = ""))]
    pub struct Sender<E> {
        inner: Rc<Inner<E>>,
    }
    impl<E> Sender<E> {
        pub fn new() -> Self {
            Default::default()
        }

        pub fn send(&self, state: E) {
            *self.inner.state.borrow_mut() = Some(state);
            self.inner.version.set(self.inner.version.get() + 1);
            self.inner.event_handle.notify(usize::MAX);
        }

        pub fn new_receiver(&self) -> Receiver<E> {
            Receiver {
                inner:    Rc::downgrade(&self.inner),
                version:  self.inner.version.get(),
                listener: None,
            }
        }
    }

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
}

impl<E> Default for BroadcastEventSource<E> {
    fn default() -> Self {
        let (mut tx, rx) = async_broadcast::broadcast(16);
        tx.set_await_active(false);
        Self(tx, rx.deactivate())
    }
}

impl<E> BroadcastEventSource<E> {
    pub fn as_inner(&self) -> &async_broadcast::Sender<E> {
        &self.0
    }
}

pub type BroadcastOwned<E: Clone + 'static> = impl Future<Output = ()> + 'static;
impl<E: Clone> BroadcastEventSource<E> {
    pub fn broadcast(&self, msg: E) -> impl Future<Output = ()> + '_ {
        // The sender is never closed because we hold a inactive receiver.
        assert!(!self.0.is_closed());

        // Here this should only error if there is no active receiver, which
        // is fine.
        async move {
            let _ = self.0.broadcast(msg).await;
        }
    }

    /// Like `broadcast`, but the returned future doesn't borrow from `self`.
    pub fn broadcast_owned(&self, msg: E) -> BroadcastOwned<E>
    where
        E: 'static,
    {
        assert!(!self.0.is_closed());
        let sender = self.0.clone();

        async move {
            let _ = sender.broadcast(msg).await;
        }
    }

    /// Like `broadcast`, but if the queue is full, instead of waiting, this function will reserve
    /// more space in the queue.
    pub fn broadcast_reserve(&mut self, msg: E) {
        use async_broadcast::TrySendError;
        assert!(!self.0.is_closed());
        if self.0.is_full() {
            self.0.set_capacity(self.0.capacity() * 2);
        }
        match self.0.try_broadcast(msg) {
            Err(TrySendError::Full(_)) => unreachable!(),
            _ => ()
        }
    }
}

impl<E: Clone + 'static> EventSource<E> for BroadcastEventSource<E> {
    type Source = impl futures_core::stream::Stream<Item = E> + Unpin + 'static;

    fn subscribe(&self) -> Self::Source {
        /// A wrapper of the broadcast receiver that deactivates the receiver,
        /// without closing the sender, when dropped.
        struct ReceiverWrapper<E>(Option<async_broadcast::Receiver<E>>);
        impl<E> Drop for ReceiverWrapper<E> {
            fn drop(&mut self) {
                if let Some(receiver) = self.0.take() {
                    receiver.deactivate();
                }
            }
        }
        impl<E: Clone + 'static> futures_core::Stream for ReceiverWrapper<E> {
            type Item = E;

            fn poll_next(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Self::Item>> {
                let this = self.0.as_mut().unwrap();
                Pin::new(this).poll_next(cx)
            }
        }
        ReceiverWrapper(Some(self.0.new_receiver()))
    }
}
