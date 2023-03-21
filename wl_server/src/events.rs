pub trait EventSource<Event> {
    type Source: futures_util::stream::Stream<Item = Event> + 'static;
    /// Get a stream of events from the event source.
    fn subscribe(&self) -> Self::Source;
}

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

    /// An event source whose receivers will receive an event when a state
    /// is changed. It's possible for multiple changes to be
    /// aggregated into a single event.
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

pub mod aggregate {
    //! An event source that aggregates sent events together for each receiver.
    //!
    //! This can be used to create a sort of event-based diff update mechanism,
    //! where a normal stream-of-event sources are not suitable.
    //!
    //! A good example is the object store. If store updates are a stream of
    //! events, we can imagine a sequence of events like:
    //!
    //!     - Insert Object { id: 1, interface: wl_surface }
    //!     - Remove Object { id: 1 }
    //!     - Insert Object { id: 1, interface: wl_buffer }
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
        pub fn new() -> Self {
            Default::default()
        }
    }

    impl<E: Default> Sender<E> {
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

    pub struct Receiver<E> {
        inner:      Rc<RefCell<ReceiverInner<E>>>,
        terminated: bool,
    }

    impl<E> Sender<E> {
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
}

pub mod broadcast {
    use std::{future::Future, pin::Pin};

    pub use async_broadcast::Sender;
    use async_broadcast::TrySendError;
    use derivative::Derivative;
    /// An event source implementation based on the [`async_broadcast`]
    /// channels.
    ///
    /// This event source is mpmc, so it can be cloned, enabling multiple sender
    /// to send at the same time
    #[derive(Debug, Derivative)]
    #[derivative(Clone(bound = ""))]
    pub struct Broadcast<E>(
        async_broadcast::Sender<E>,
        async_broadcast::InactiveReceiver<E>,
    );

    /// An event source implementation based on the [`async_broadcast`]
    /// channels.
    ///
    /// Like [`Broadcast`], except when the internal queue is full, the oldest
    /// event will be dropped.
    #[derive(Debug, Derivative)]
    #[derivative(Clone(bound = ""))]
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
        pub fn new(capacity: usize) -> Self {
            let (mut tx, rx) = async_broadcast::broadcast(capacity);
            tx.set_await_active(false);
            tx.set_overflow(true);
            Self(tx, rx.deactivate())
        }

        pub fn new_sender(&self) -> async_broadcast::Sender<E> {
            self.0.clone()
        }

        pub fn set_capacity(&mut self, capacity: usize) {
            self.0.set_capacity(capacity);
        }
    }

    impl<E> Broadcast<E> {
        pub fn new_sender(&self) -> async_broadcast::Sender<E> {
            self.0.clone()
        }
    }

    pub type BroadcastOwned<E: Clone + 'static> = impl Future<Output = ()> + 'static;
    impl<E: Clone> Broadcast<E> {
        pub fn broadcast(&self, msg: E) -> impl Future<Output = ()> + '_ {
            // The sender is never closed because we hold a inactive receiver.
            assert!(!self.0.is_closed());

            // Here this should only error if there is no active receiver, which
            // is fine.
            async move {
                let _ = self.0.broadcast(msg).await;
            }
        }

        /// Like `broadcast`, but the returned future doesn't borrow from
        /// `self`.
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

        /// Like `broadcast`, but if the queue is full, instead of waiting, this
        /// function will reserve more space in the queue.
        pub fn broadcast_reserve(&mut self, msg: E) {
            assert!(!self.0.is_closed());

            if self.0.is_full() {
                self.0.set_capacity(self.0.capacity() * 2);
            }

            let result = self.0.try_broadcast(msg);
            assert!(!matches!(result, Err(TrySendError::Full(_))));
        }
    }

    impl<E: Clone> Ring<E> {
        pub fn broadcast(&self, msg: E) {
            assert!(!self.0.is_closed());
            let result = self.0.try_broadcast(msg);
            assert!(!matches!(result, Err(TrySendError::Full(_))));
        }
    }

    /// The event stream for Broadcast and RingBroadcast event sources
    ///
    /// A wrapper of the broadcast receiver that deactivates the
    /// receiver, without closing the sender, when dropped.
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

    impl<E: Clone + 'static> super::EventSource<E> for Sender<E> {
        type Source = Receiver<E>;

        fn subscribe(&self) -> Self::Source {
            Receiver(Some(self.new_receiver()))
        }
    }
}
