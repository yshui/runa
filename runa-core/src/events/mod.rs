//! Traits and types related to event
//!
//! The [`EventSource`] trait is the core of how events are delivered. And it's
//! quite simple. Types that can emit events, will implement this trait. And a
//! stream of events can be obtained from that type by calling
//! [`EventSource::subscribe`]. This stream of events can then be registered
//! with [`Client::EventDispatcher`](crate::client::traits::Client::EventDispatcher)
//! together with an event handler. And the event dispatcher will call the
//! handler whenever a new event is received from the stream. Note as it is the
//! compositor author who will implement the `Client` trait, so they are the
//! ones who are responsible for calling event handlers.
//!
//! This crate and other `runa` related crates will provide some event sources.
//! For example, [`Store`](crate::client::store::Store) is an event source that
//! emits events whenever there is an object being inserted or removed from the
//! store. But `runa` crates will also expect downstream compositor authors to
//! implement their own event sources. You will see `EventSource` trait bounds
//! when that is the case.

/// Event source
///
/// An event source is something you can get a stream of events from.
pub trait EventSource<Event> {
    /// Type of event stream you get from this event source.
    type Source: futures_util::stream::Stream<Item = Event> + 'static;

    /// Get a stream of events from the event source.
    fn subscribe(&self) -> Self::Source;
}

// TODO: this is equivalent to `async_broadcast` with `set_overflow(true)` and
// `set_capacity(1)`. Contemplate if we should just use `async_broadcast`
// directly.
pub mod single_state;
pub mod aggregate;
pub mod broadcast;
