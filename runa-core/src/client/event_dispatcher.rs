use std::{
    any::Any,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    task::{ready, Context, Poll, Waker},
};

use futures_core::{FusedFuture, Stream};
use futures_util::{
    stream::{FuturesUnordered, StreamFuture},
    StreamExt,
};

use super::traits;
use crate::utils::one_shot_signal;

type PairedEventHandlerFut<'a, H, Ctx>
where
    H: traits::EventHandler<Ctx>,
    Ctx: traits::Client,
= impl Future<Output = Result<(EventHandlerOutput, H), (H::Message, H)>> + 'a;

/// A helper function for storing the event handler's future.
///
/// Event handler's generate a future that references the event handler itself,
/// if we store that directly in [`PairedEventHandler`] we would have a
/// self-referential struct, so we use this async fn to get around that.
///
/// The stop signal is needed so when the future can to be stopped early, for
/// example when a [`PendingEventFut`] is dropped.
fn paired_event_handler_driver<'ctx, H, Ctx: traits::Client>(
    mut handler: H,
    mut stop_signal: one_shot_signal::Receiver,
    mut message: H::Message,
    objects: &'ctx mut Ctx::ObjectStore,
    connection: &'ctx mut Ctx::Connection,
    server_context: &'ctx Ctx::ServerContext,
) -> PairedEventHandlerFut<'ctx, H, Ctx>
where
    H: traits::EventHandler<Ctx>,
{
    async move {
        use futures_util::{select, FutureExt};
        select! {
            () = stop_signal => {
                Err((message, handler))
            }
            ret = handler.handle_event(objects, connection, server_context, &mut message).fuse() => {
                Ok((ret, handler))
            }
        }
    }
}

type EventHandlerOutput =
    Result<traits::EventHandlerAction, Box<dyn std::error::Error + Send + Sync + 'static>>;
#[pin_project::pin_project]
struct PairedEventHandler<'fut, Ctx: traits::Client, ES: Stream, H: traits::EventHandler<Ctx>> {
    #[pin]
    event_source:  ES,
    should_retain: bool,
    handler:       Option<H>,
    message:       Option<ES::Item>,
    #[pin]
    fut:           Option<PairedEventHandlerFut<'fut, H, Ctx>>,
    stop_signal:   Option<one_shot_signal::Sender>,
    _ctx:          PhantomData<Ctx>,
}

impl<Ctx: traits::Client, ES: Stream, H: traits::EventHandler<Ctx>> Stream
    for PairedEventHandler<'_, Ctx, ES, H>
{
    type Item = ();

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        if this.message.is_none() {
            let Some(message) = ready!(this.event_source.poll_next(cx)) else {
                return Poll::Ready(None);
            };
            *this.message = Some(message);
        };
        Poll::Ready(Some(()))
    }
}

trait AnyEventHandler: Stream<Item = ()> {
    type Ctx: traits::Client;
    fn poll_handle(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<EventHandlerOutput>;

    fn start_handle<'a>(
        self: Pin<Box<Self>>,
        objects: &'a mut <Self::Ctx as traits::Client>::ObjectStore,
        connection: &'a mut <Self::Ctx as traits::Client>::Connection,
        server_context: &'a <Self::Ctx as traits::Client>::ServerContext,
    ) -> Pin<Box<dyn AnyEventHandler<Ctx = Self::Ctx> + 'a>>;

    fn stop_handle(
        self: Pin<Box<Self>>,
    ) -> (
        Pin<Box<dyn AnyEventHandler<Ctx = Self::Ctx>>>,
        traits::EventHandlerAction,
    );
}

impl<Ctx, ES, H> PairedEventHandler<'_, Ctx, ES, H>
where
    Ctx: traits::Client,
    ES: Stream + 'static,
    H: traits::EventHandler<Ctx> + 'static,
{
    /// Lengthen or shorten the lifetime parameter of the returned
    /// `PairedEventHandler`.
    ///
    /// # Panics
    ///
    /// This function verifies the `fut` field of `Self` is `None`, i.e. `Self`
    /// does not contain any references. If this is not the case, this function
    /// panics.
    fn coerce_lifetime<'a>(self: Pin<Box<Self>>) -> Pin<Box<PairedEventHandler<'a, Ctx, ES, H>>> {
        assert!(self.fut.is_none());
        // Safety: this is safe because `fut` is `None` and thus does not contain any
        // references. And we do not move `self` out of the `Pin<Box<Self>>`.
        unsafe {
            let raw = Box::into_raw(Pin::into_inner_unchecked(self));
            Pin::new_unchecked(Box::from_raw(raw.cast()))
        }
    }
}

impl<Ctx, ES, H> AnyEventHandler for PairedEventHandler<'_, Ctx, ES, H>
where
    Ctx: traits::Client,
    ES: Stream + 'static,
    H: traits::EventHandler<Ctx, Message = ES::Item> + 'static,
{
    type Ctx = Ctx;

    fn poll_handle(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<EventHandlerOutput> {
        let mut this = self.project();
        let fut = this.fut.as_mut().as_pin_mut().unwrap();
        let (ret, handler) = ready!(fut
            .poll(cx)
            .map(|ret| ret.unwrap_or_else(|_| unreachable!("future stopped unexpectedly"))));
        *this.handler = Some(handler);
        *this.stop_signal = None;
        this.fut.set(None);
        // EventHandlerAction::Stop or Err() means we should stop handling events
        *this.should_retain =
            *this.should_retain && matches!(ret, Ok(traits::EventHandlerAction::Keep));
        Poll::Ready(ret)
    }

    fn start_handle<'a>(
        self: Pin<Box<Self>>,
        objects: &'a mut <Self::Ctx as traits::Client>::ObjectStore,
        connection: &'a mut <Self::Ctx as traits::Client>::Connection,
        server_context: &'a <Self::Ctx as traits::Client>::ServerContext,
    ) -> Pin<Box<dyn AnyEventHandler<Ctx = Self::Ctx> + 'a>> {
        // Shorten the lifetime of `Self`. So we can store `fut` with lifetime `'a` in
        // it.
        let mut shortened = self.coerce_lifetime();
        let mut this = shortened.as_mut().project();
        let message = this.message.take().unwrap();
        let handler = this.handler.take().unwrap();
        assert!(this.stop_signal.is_none());
        assert!(*this.should_retain);
        let (tx, stop_signal) = one_shot_signal::new_pair();
        *this.stop_signal = Some(tx);

        let new_fut = paired_event_handler_driver(
            handler,
            stop_signal,
            message,
            objects,
            connection,
            server_context,
        );
        this.fut.set(Some(new_fut));
        shortened
    }

    fn stop_handle(
        mut self: Pin<Box<Self>>,
    ) -> (
        Pin<Box<dyn AnyEventHandler<Ctx = Self::Ctx>>>,
        traits::EventHandlerAction,
    ) {
        use futures_util::task::noop_waker_ref;
        let mut this = self.as_mut().project();
        // Stop the handler, so when we poll it, it will give us the handler back.
        let Some(stop_signal) = this.stop_signal.take() else {
            // Already stopped
            let should_retain = *this.should_retain;
            assert!(self.handler.is_some());
            return (self.coerce_lifetime(), if should_retain {
                traits::EventHandlerAction::Keep
            } else {
                traits::EventHandlerAction::Stop
            });
        };
        stop_signal.send();

        let mut cx = Context::from_waker(noop_waker_ref());
        let mut fut = this.fut.as_mut().as_pin_mut().unwrap();
        let result = loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(result) => break result,
                Poll::Pending => {},
            }
        };
        match result {
            Ok((ret, handler)) => {
                // The handler completed before it was stopped by `stop_signal`
                *this.handler = Some(handler);
                this.fut.set(None);
                *this.should_retain =
                    *this.should_retain && matches!(ret, Ok(traits::EventHandlerAction::Keep));
                let should_retain = *this.should_retain;
                (
                    self.coerce_lifetime(),
                    if should_retain {
                        traits::EventHandlerAction::Keep
                    } else {
                        traits::EventHandlerAction::Stop
                    },
                )
            },
            Err((msg, handler)) => {
                // The handler was stopped by `stop_signal`
                *this.handler = Some(handler);
                *this.message = Some(msg);
                // The handler was not completed, so it's inconlusive whether it would have
                // returned `EventHandlerAction::Keep` or not. So we keep it just in case.
                (self.coerce_lifetime(), traits::EventHandlerAction::Keep)
            },
        }
    }
}

type BoxedAnyEventHandler<Ctx> = Pin<Box<dyn AnyEventHandler<Ctx = Ctx>>>;

pub struct EventDispatcher<Ctx> {
    handlers:       FuturesUnordered<StreamFuture<BoxedAnyEventHandler<Ctx>>>,
    active_handler: Option<BoxedAnyEventHandler<Ctx>>,
    waker:          Option<Waker>,
}

impl<Ctx> std::fmt::Debug for EventDispatcher<Ctx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventDispatcher").finish()
    }
}

impl<Ctx> Default for EventDispatcher<Ctx> {
    fn default() -> Self {
        Self {
            handlers:       FuturesUnordered::new(),
            active_handler: None,
            waker:          None,
        }
    }
}

impl<Ctx> EventDispatcher<Ctx> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<Ctx: traits::Client + 'static> traits::EventDispatcher<Ctx> for EventDispatcher<Ctx> {
    fn add_event_handler<M: Any>(
        &mut self,
        event_source: impl Stream<Item = M> + 'static,
        handler: impl traits::EventHandler<Ctx, Message = M> + 'static,
    ) {
        let pinned = Box::pin(PairedEventHandler {
            event_source,
            handler: Some(handler),
            should_retain: true,
            message: None,
            fut: None,
            stop_signal: None,
            _ctx: PhantomData::<Ctx>,
        });
        let pinned = pinned as Pin<Box<dyn AnyEventHandler<Ctx = Ctx>>>;
        let pinned = pinned.into_future();
        self.handlers.push(pinned);
        if let Some(w) = self.waker.take() {
            w.wake();
        }
    }
}

impl<Ctx: traits::Client + 'static> EventDispatcher<Ctx> {
    /// Poll for the next event, which needs to be handled with the use of the
    /// client context.
    ///
    /// # Caveats
    ///
    /// If this is called from multiple tasks, those tasks will just keep waking
    /// up each other and waste CPU cycles.
    pub fn poll_next<'a>(&'a mut self, cx: &mut Context<'_>) -> Poll<PendingEvent<'a, Ctx>> {
        loop {
            if self.active_handler.is_some() {
                return Poll::Ready(PendingEvent { dispatcher: self })
            }
            match Pin::new(&mut self.handlers).poll_next(cx) {
                Poll::Ready(Some((Some(()), handler))) => {
                    self.active_handler = Some(handler);
                },
                Poll::Ready(Some((None, _))) => (),
                Poll::Ready(None) | Poll::Pending => {
                    // There is no active handler. `FuturesUnordered` will wake us up if there are
                    // handlers that are ready. But we also need to wake up if there are new
                    // handlers added. So we store the waker.
                    if let Some(w) = self.waker.take() {
                        if w.will_wake(cx.waker()) {
                            self.waker = Some(w);
                        } else {
                            // Wake the previous waker, because it's going to be replaced.
                            w.wake();
                            self.waker = Some(cx.waker().clone());
                        }
                    } else {
                        self.waker = Some(cx.waker().clone());
                    }
                    return Poll::Pending
                },
            }
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> impl FusedFuture<Output = PendingEvent<'_, Ctx>> + '_ {
        struct Next<'a, Ctx> {
            dispatcher: Option<&'a mut EventDispatcher<Ctx>>,
        }
        impl<'a, Ctx: traits::Client + 'static> Future for Next<'a, Ctx> {
            type Output = PendingEvent<'a, Ctx>;

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = self.dispatcher.as_deref_mut().unwrap();
                ready!(this.poll_next(cx));

                let this = self.dispatcher.take().unwrap();
                Poll::Ready(PendingEvent { dispatcher: this })
            }
        }

        impl<'a, Ctx: traits::Client + 'static> FusedFuture for Next<'a, Ctx> {
            fn is_terminated(&self) -> bool {
                self.dispatcher.is_none()
            }
        }

        Next {
            dispatcher: Some(self),
        }
    }

    /// Handle all events that are current queued. The futures returned will
    /// resolve as soon as there are no more events queued in this event
    /// dispatcher. It will not wait for new events to be queued, but it may
    /// wait for the futures returned by the event handlers to resolve.
    pub async fn handle_queued_events<'ctx>(
        &'ctx mut self,
        objects: &'ctx mut Ctx::ObjectStore,
        connection: &'ctx mut Ctx::Connection,
        server_context: &'ctx Ctx::ServerContext,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // We are not going to wait on the event dispatcher, so we use a noop waker.
        let waker = futures_util::task::noop_waker();
        let cx = &mut Context::from_waker(&waker);
        while let Poll::Ready(pending_event) = self.poll_next(cx) {
            pending_event
                .handle(objects, connection, server_context)
                .await?;
        }
        Ok(())
    }
}

pub struct PendingEvent<'a, Ctx> {
    dispatcher: &'a mut EventDispatcher<Ctx>,
}

pub struct PendingEventFut<'dispatcher, 'ctx, Ctx: traits::Client> {
    dispatcher: &'dispatcher mut EventDispatcher<Ctx>,
    fut:        Option<Pin<Box<dyn AnyEventHandler<Ctx = Ctx> + 'ctx>>>,
}

impl<'dispatcher, 'ctx, Ctx: traits::Client> Future for PendingEventFut<'dispatcher, 'ctx, Ctx> {
    type Output = EventHandlerOutput;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.fut.as_mut().unwrap().as_mut().poll_handle(cx)
    }
}

impl<'dispatcher, 'ctx, Ctx: traits::Client> Drop for PendingEventFut<'dispatcher, 'ctx, Ctx> {
    fn drop(&mut self) {
        if let Some(fut) = self.fut.take() {
            let (fut, action) = fut.stop_handle();
            if action == traits::EventHandlerAction::Keep {
                self.dispatcher.handlers.push(fut.into_future());
                if let Some(w) = self.dispatcher.waker.take() {
                    w.wake();
                }
            }
        }
    }
}

impl<'this, Ctx: traits::Client> PendingEvent<'this, Ctx> {
    /// Start handling the event.
    ///
    /// # Notes
    ///
    /// If you `std::mem::forget` the returned future, the event handler will be
    /// permanently removed from the event dispatcher.
    ///
    /// Like [`traits::EventHandler::handle_event`], the future returned cannot
    /// race with other futures in the same client context.
    pub fn handle<'a>(
        self,
        objects: &'a mut Ctx::ObjectStore,
        connection: &'a mut Ctx::Connection,
        server_context: &'a Ctx::ServerContext,
    ) -> PendingEventFut<'this, 'a, Ctx>
    where
        'this: 'a,
    {
        let fut = self.dispatcher.active_handler.take().unwrap();
        let fut = fut.start_handle(objects, connection, server_context);
        PendingEventFut {
            dispatcher: self.dispatcher,
            fut:        Some(fut),
        }
    }
}
