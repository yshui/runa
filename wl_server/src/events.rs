use std::{
    cell::{Cell, RefCell},
    future::Future,
    hash::Hash,
    rc::{Rc, Weak},
};

use ahash::AHashSet as HashSet;
use event_listener::EventListener;

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
///
/// However, if the event you listen to happens infrequently, it is recommended to explicitly
/// remove the listener from the event source. Otherwise the source can accumulate a lot of dead
/// listeners, which might cause stutter when the event is eventually triggered.
#[derive(Clone, Debug)]
pub struct EventHandle(Weak<EventFlagsState>);

impl EventHandle {
    /// Set a event flag and notify listeners. Returns if the event handle is still alive.
    pub fn set(&self, slot: usize) -> bool {
        if let Some(state) = self.0.upgrade() {
            EventFlags(state).set(slot);
            true
        } else {
            tracing::debug!("EventHandle has been dropped");
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
    pub fn set(&self, flag: usize) {
        let mut flags = self.0.flags.get();
        flags.set(flag, true);

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

/// A event multiplexer.
///
/// It waits for events on the behave of registered handlers, and dispatches
/// them to the appropriate handler.
pub trait EventMux {
    fn event_handle(&self) -> EventHandle;
}

#[derive(Default, Debug)]
pub struct Listeners {
    listeners: RefCell<HashSet<(crate::events::EventHandle, usize)>>,
}

impl Listeners {
    pub fn notify(&self) {
        self.listeners.borrow_mut().retain(|(handle, slot)| {
            // Notify the listener, also check if the listen is still alive, removing all
            // the handles that has died.
            handle.set(*slot)
        });
    }
}

impl Listeners {
    pub fn add_listener(&self, handle: (crate::events::EventHandle, usize)) {
        self.listeners.borrow_mut().insert(handle);
    }

    pub fn remove_listener(&self, handle: (crate::events::EventHandle, usize)) -> bool {
        self.listeners.borrow_mut().remove(&handle)
    }
}

/// A trait for types wanting to have a event multiplexer dispatching events to
/// them.
pub trait EventHandler<Mux: EventMux + DispatchTo<Self>>: Sized {
    type Error;
    type Fut<'a>: Future<Output = Result<(), Self::Error>> + 'a
    where
        Mux: 'a;
    fn invoke(ctx: &mut Mux) -> Self::Fut<'_>;
}

/// Implemented by the event multiplexer, if it's able to dispatch events for
/// a event handler type.
///
/// The implementor should wait for events delievered via the handle returned by
/// [`EventMux::event_handle`], when an event is delievered to the slot number
/// returned by `DispatchTo::<Handler>::slot`, it should call
/// `<Handler as HasEvent>::invoke`.
///
/// You can use the [`event_multiplexer`] macro to generate a `dispatch_events` function along with
/// DispatchTo impls that does the calling of `invoke` for you.
pub trait DispatchTo<Handler: EventHandler<Self>>: EventMux + Sized {
    /// The slot number type `T` should use when adding listeners to an
    /// [`EventSource`].
    const SLOT: usize;
}

#[macro_export]
macro_rules! event_multiplexer {
    // Expand receivers into (slot, receiver) pairs
    (ctx: $ctx:ty, error: $error:ty, __expand; counter=$c:expr; expanded=($($is:expr, $ts:ty),*); $t0:ty $(, $t:ty)*) => {
        $crate::event_multiplexer!(ctx: $ctx, error: $error, __expand; counter=$c + 1; expanded=($c, $t0 $(, $is, $ts)*); $($t),*);
    };
    // Ditto
    (ctx: $ctx:ty, error: $error:ty, __expand; counter=$c:expr; expanded=($($is:expr, $ts:ty),*);) => {
        $crate::event_multiplexer!(ctx: $ctx, error: $error, __generate; $($is, $ts),*);
    };
    // Generate the DIspatchTo impl and dispatch_events function
    (ctx: $ctx:ty, error: $error:ty, __generate; $($slot:expr, $receiver:ty),*) => {
        const _: () = {
            $(
                impl $crate::events::DispatchTo<$receiver> for $ctx {
                    const SLOT: usize = $slot;
                }
            )*
            impl $ctx {
                async fn dispatch_events(&mut self, events: $crate::events::Flags) -> Result<(), $error> {
                    for bit in events.iter_ones() {
                        match bit {
                            $(
                                <Self as $crate::events::DispatchTo<$receiver>>::SLOT => {
                                    <$receiver as $crate::events::EventHandler<Self>>::invoke(self).await?;
                                }
                            )*
                            _ => unreachable!(),
                        }
                    }
                    Ok(())
                }
            }
        };
    };
    (ctx: $ctx:ty, error: $error:ty, receivers: [ $($receiver:ty),* $(,)? ] $(,)?) => {
        $crate::__private::assert_impl_all!($ctx: $crate::events::EventMux);
        $crate::event_multiplexer!(ctx: $ctx, error: $error, __expand; counter=0; expanded=(); $($receiver),*);
    };
    // == Normalize ==
    // Reorder arguments
    (ctx: $ctx:ty, receivers: [ $($receiver:ty),* $(,)? ], error: $error:ty $(,)?) => {
        $crate::event_multiplexer!(ctx: $ctx, error: $error, receivers: [ $($receiver),* ]);
    };
    // Ditto
    ($id:ident: $val:tt, $($id2:ident: $val2:tt),+$(,)?) => {
        $crate::event_multiplexer! {$($id2: $val2),+, $id: $val }
    }
}
