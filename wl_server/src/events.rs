use std::{cell::Cell, hash::Hash, pin::Pin, rc::Rc};

use event_listener::EventListener;
use futures_lite::Future;

pub type Flags = bitvec::array::BitArray<[u64; 1], bitvec::order::LocalBits>;

#[derive(Debug, Default)]
struct EventFlagsState {
    flags: Cell<Flags>,
    event: event_listener::Event,
}

/// A bitflag that wakes up listeners when set.
///
/// Each listener is supposed to only react to a single bit of the flag.
/// Although the inner mechanism used can handle N-to-N notification, this is
/// only intended for one-to-one single direction communication, i.e. from
/// server context to per-client context.
#[derive(Clone, Debug, Default)]
#[repr(transparent)]
pub struct EventFlags(Rc<EventFlagsState>);

impl EventFlags {
    /// Set the n-th bit of the flag, and wake up the listener.
    pub fn set(&self, flag: u8) {
        let mut flags = self.0.flags.get();
        flags.set(flag as usize, true);

        self.0.flags.set(flags);
        self.0.event.notify(1);
    }
 
    /// Get the current set bits and reset the flags.
    pub fn reset(&self) -> Flags {
        self.0.flags.replace(bitvec::array::BitArray::ZERO)
    }

    pub fn listen(&self) -> EventListener {
        self.0.event.listen()
    }
}

impl PartialEq for EventFlags {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for EventFlags {}

impl Hash for EventFlags {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Rc::as_ptr(&self.0).hash(state);
    }
}

pub struct EventSlot;

/// Each event listener is assigned a static slot in the event flags.
impl EventSlot {
    pub const REGISTRY: i32 = 0;
}

pub trait EventHandler<Ctx> {
    // Allocation: event handlers are supposed for events that happens relatively
    // rarely. frequent events, like mouse moves are sent directly from the
    // server context.
    fn handle_event(&self, ctx: &mut Ctx) -> Pin<Box<dyn Future<Output = ()>>>;
}
