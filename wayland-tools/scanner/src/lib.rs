
#[doc(inline)]
pub use macros::generate_protocol;
#[doc(inline)]
pub use codegen::generate_protocol;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("Unknown opcode {0}")]
    UnknownOpcode(u32),
}

pub mod io {
    use std::os::unix::io::RawFd;

    pub use futures_lite::AsyncBufRead;
    pub use runa_io_traits::*;
    #[inline]
    pub fn pop_fd(fds: &mut &[RawFd]) -> RawFd {
        assert!(!fds.is_empty(), "Not enough fds in buffer");
        // Safety: we checked that the slice is long enough
        let ret = unsafe { *fds.get_unchecked(0) };
        *fds = &fds[1..];
        ret
    }

    #[inline]
    /// Pop `len` bytes from the buffer and returns it. Advances the buffer by
    /// `len` aligned up to 4 bytes.
    pub fn pop_bytes<'a>(bytes: &mut &'a [u8], len: usize) -> &'a [u8] {
        use std::slice::from_raw_parts;
        let blen = bytes.len();
        let len_aligned = (len + 3) & !3;
        assert!(
            blen >= len_aligned,
            "Not enough bytes in buffer, has {blen}, asking for {len_aligned}"
        );
        let ptr = bytes.as_ptr();
        // Safety: we checked that the slice is long enough
        let (ret, rest) = unsafe {
            (
                from_raw_parts(ptr, len),
                from_raw_parts(ptr.add(len_aligned), blen - len_aligned),
            )
        };
        *bytes = rest;
        ret
    }

    #[inline]
    pub fn pop_i32(bytes: &mut &[u8]) -> i32 {
        let slice = pop_bytes(bytes, 4);
        // Safety: slice is guaranteed to be 4 bytes long
        i32::from_ne_bytes(unsafe { *(slice.as_ptr() as *const [u8; 4]) })
    }

    #[inline]
    pub fn pop_u32(bytes: &mut &[u8]) -> u32 {
        let slice = pop_bytes(bytes, 4);
        // Safety: slice is guaranteed to be 4 bytes long
        u32::from_ne_bytes(unsafe { *(slice.as_ptr() as *const [u8; 4]) })
    }
}

pub use bitflags::bitflags;
pub use bytes::{BufMut, BytesMut};
pub use futures_lite::ready;
pub use num_enum;
pub use runa_wayland_types as types;

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
