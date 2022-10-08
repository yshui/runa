use std::{cell::Cell, hash::Hash, pin::Pin, rc::{Rc, Weak}};

use event_listener::EventListener;
use futures_lite::Future;

pub type Flags = bitvec::array::BitArray<[u64; 1], bitvec::order::LocalBits>;

#[derive(Debug, Default)]
struct EventFlagsState {
    flags: Cell<Flags>,
    event: event_listener::Event,
}

/// A handle to a [`EventFlags`]. This holds a weak reference to a
/// [`EventFlags`] and can be used to notify the listeners of the EventFlags if
/// it is still alive. This is useful because the holder of the EventFlags could
/// simply terminate without needing to deregister from event sources.
#[derive(Clone, Debug)]
pub struct EventHandle(Weak<EventFlagsState>);

impl EventHandle {
    pub fn set(&self, slot: u8) -> bool {
        if let Some(state) = self.0.upgrade() {
            EventFlags(state).set(slot);
            true
        } else {
            false
        }
    }
}

/// A bitflag that wakes up listeners when set.
///
/// Each listener is supposed to only react to a single bit of the flag.
/// Although the inner mechanism used can handle N-to-N notification, this is
/// only intended for one-to-one single direction communication, i.e. from
/// server context to per-client context.
#[derive(Debug, Default)]
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

    pub fn as_handle(&self) -> EventHandle {
        EventHandle(Rc::downgrade(&self.0))
    }
}

impl PartialEq for EventHandle {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for EventHandle {}

impl Hash for EventHandle {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Weak::as_ptr(&self.0).hash(state);
    }
}
