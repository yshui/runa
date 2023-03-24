use std::future::Future;

use super::traits;
use crate::utils::one_shot_signal;

/// A wrapper around an event handler that allows itself to be aborted
/// via an [`AbortHandle`];
#[derive(Debug)]
pub struct Abortable<E> {
    event_handler: E,
    stop_signal:   Option<one_shot_signal::Receiver>,
}

impl<E> Abortable<E> {
    /// Wrap an event handler so it may be aborted.
    pub fn new(event_handler: E) -> (Self, AbortHandle) {
        let (tx, rx) = one_shot_signal::new_pair();
        let abort_handle = AbortHandle { inner: tx };
        let abortable = Abortable {
            event_handler,
            stop_signal: Some(rx),
        };
        (abortable, abort_handle)
    }
}

impl<Ctx: traits::Client, E: traits::EventHandler<Ctx>> traits::EventHandler<Ctx> for Abortable<E> {
    type Message = E::Message;

    type Future<'ctx> = impl Future<
            Output = Result<
                super::traits::EventHandlerAction,
                Box<dyn std::error::Error + Sync + Send>,
            >,
        > + 'ctx;

    fn handle_event<'ctx>(
        &'ctx mut self,
        objects: &'ctx mut <Ctx as traits::Client>::ObjectStore,
        connection: &'ctx mut <Ctx as traits::Client>::Connection,
        server_context: &'ctx <Ctx as traits::Client>::ServerContext,
        message: &'ctx mut Self::Message,
    ) -> Self::Future<'ctx> {
        async move {
            use futures_util::{select, FutureExt};
            let mut stop_signal = self.stop_signal.take().unwrap();
            select! {
                () = stop_signal => {
                    Ok(super::traits::EventHandlerAction::Stop)
                },
                res = self.event_handler.handle_event(objects, connection, server_context, message).fuse() => {
                    self.stop_signal = Some(stop_signal);
                    res
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct AbortHandle {
    inner: one_shot_signal::Sender,
}

impl AbortHandle {
    /// Abort the event handler associated with this abort handle.
    /// This will cause the event handler to be stopped the next time
    /// it is polled.
    pub fn abort(&self) {
        self.inner.send();
    }

    /// Turn this abort handle into an [`AutoAbortHandle`], which will
    /// automatically abort the event handler when it is dropped.
    pub fn auto_abort(self) -> AutoAbortHandle {
        AutoAbortHandle { inner: self.inner }
    }
}

/// An abort handle that will automatically abort the event handler
/// when dropped.
#[derive(Debug)]
pub struct AutoAbortHandle {
    inner: one_shot_signal::Sender,
}

impl Drop for AutoAbortHandle {
    fn drop(&mut self) {
        self.inner.send();
    }
}
