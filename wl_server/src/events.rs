use std::pin::Pin;
use std::rc::Rc;
use std::cell::Cell;
use event_listener::EventListener;
use futures_lite::Future;

/// A bitflag that wakes up listeners when set.
///
/// Each listener is supposed to only react to a single bit of the flag. Although the inner
/// mechanism used can handle N-to-N notification, this is only intended for one-to-one single
/// direction communication, i.e. from server context to per-client context.
#[derive(Clone)]
pub struct EventFlags {
    event: Rc<event_listener::Event>,
    flags: Rc<Cell<u64>>,
}

impl EventFlags {
    /// Set the n-th bit of the flag, and wake up the listener.
    pub fn set(&self, flag: u8) {
        self.flags.set(self.flags.get() | (1u64 << flag));
        self.event.notify(1);
    }
    pub fn listen(&self) -> EventListener {
        self.event.listen()
    }
}

pub struct EventSlots;

/// Each event listener is assigned a static slot in the event flags.
impl EventSlots {
    pub const REGISTRY: u32 = 0;
}

pub trait EventHandler<Ctx> {
    // Allocation: event handlers are supposed for events that happens relatively rarely.
    // frequent events, like mouse moves are sent directly from the server context.
    fn handle_event(&self, ctx: &mut Ctx) -> Pin<Box<dyn Future<Output = ()>>>;
}
