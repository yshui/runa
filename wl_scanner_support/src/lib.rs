#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Deserializer(#[from] serde::de::value::Error),
    #[error("Unknown opcode {0}")]
    UnknownOpcode(u32),
}

pub mod io {
    pub use futures_lite::AsyncBufRead;
    pub use wl_io::*;
}

pub trait ProtocolError: std::error::Error + Send + Sync + 'static {
    /// Returns an object id and a wayland error code associated with this
    /// error, in that order. If not None, an error event will be sent to
    /// the client.
    fn wayland_error(&self) -> Option<(u32, u32)>;
    /// Returns whether this error is fatal. If true, the client should be
    /// disconnected.
    fn fatal(&self) -> bool;
}

pub use bitflags::bitflags;
pub use futures_lite::ready;
pub use num_enum;
pub use serde;
pub use wl_types as wayland_types;

pub mod future {
    use std::{pin::Pin, task::Poll};
    pub struct PollMapFn<'a, F, O, T> {
        inner:   Option<Pin<&'a mut T>>,
        f:       F,
        _marker: ::std::marker::PhantomData<O>,
    }
    impl<'a, F, O, T> ::std::future::Future for PollMapFn<'a, F, O, T>
    where
        F: Fn(Pin<&'a mut T>, &mut ::std::task::Context<'_>) -> Poll<O> + Unpin,
        O: Unpin,
    {
        type Output = O;

        fn poll(
            mut self: ::std::pin::Pin<&mut Self>,
            cx: &mut ::std::task::Context<'_>,
        ) -> ::std::task::Poll<Self::Output> {
            use std::ptr::NonNull;
            let inner = self
                .inner
                .take()
                .expect("PollMapFn polled after completion");
            unsafe {
                // Safety:
                // 1. We don't move the reader using the &mut we get from
                // Pin::get_mut_unchecked. 2. We know Fn(Pin<&'de mut D>, cx) ->
                // T is implemented for F, so we know the    lifetime going from
                // 'de to T is sound. 3. In case of Ready, we won't touch
                // raw_reader ever again here, and inner is left    empty.
                // 4. In case of Pending, we know the inner is no longer borrowed (contract of
                // the    poll_map_fn function), so we can get our Pin<&mut
                // inner> back safely. 5. self.f must be called with Pin created
                // from raw_inner to satisfy stacked    borrow rules.
                let mut raw_inner = NonNull::from(inner.get_unchecked_mut());
                match (self.f)(Pin::new_unchecked(raw_inner.as_mut()), cx) {
                    Poll::Ready(v) => Poll::Ready(v),
                    Poll::Pending => {
                        self.inner = Some(Pin::new_unchecked(raw_inner.as_mut()));
                        Poll::Pending
                    },
                }
            }
        }
    }

    // Safety: F can't keep a hold of `inner` after it returns.
    // Rust's type system is limited so there is no easy way to enforce this via
    // types.
    //
    // Hypothetically we can have:
    //   F: for<'a> Fn(Pin<&'a mut T>, &mut Context<'_>) -> Poll<O> + Unpin
    // This way F should be valid for T with any lifetime, therefore it won't be
    // able to store it somewhere with a concrete lifetime. But then O won't be
    // able to borrow from &'a mut T. We need O to be a higher kind type to do
    // this. Like:   F: for<'a> Fn(Pin<&'a mut T>, &mut Context<'_>) ->
    // Poll<O<'a>> + Unpin We can emulate this with a trait with GAT, but it has
    // bad UX:   trait HKT { type O<'a>; }
    //   OFamily: HKT
    //   F: for<'a> Fn(Pin<&'a mut T>, &mut Context<'_>) -> Poll<OFamily::O<'a>> +
    // Unpin We need to define a OFamily for each closure.
    pub unsafe fn poll_map_fn<'a, F, O, T>(inner: Pin<&'a mut T>, f: F) -> PollMapFn<'a, F, O, T>
    where
        F: Fn(Pin<&'a mut T>, &mut ::std::task::Context<'_>) -> Poll<O> + Unpin,
    {
        PollMapFn {
            inner: Some(inner),
            f,
            _marker: ::std::marker::PhantomData,
        }
    }
}
