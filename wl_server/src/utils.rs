pub(crate) mod one_shot_signal {
    //! A simple signal that can only be sent once.
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        task::Waker,
    };
    #[derive(Debug, Default)]
    struct Inner {
        set:   Cell<bool>,
        waker: RefCell<Option<Waker>>,
    }

    #[derive(Debug, Default)]
    pub(crate) struct Sender {
        inner: Rc<Inner>,
    }

    #[derive(Debug, Default)]
    pub(crate) struct Receiver {
        inner: Option<Rc<Inner>>,
    }

    impl std::future::Future for Receiver {
        type Output = ();

        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            if let Some(inner) = self.inner.as_mut() {
                if inner.set.get() {
                    self.inner.take();
                    std::task::Poll::Ready(())
                } else {
                    if inner
                        .waker
                        .borrow()
                        .as_ref()
                        .map(|waker| !waker.will_wake(cx.waker()))
                        .unwrap_or(true)
                    {
                        if let Some(old_waker) = std::mem::replace(
                            &mut *inner.waker.borrow_mut(),
                            Some(cx.waker().clone()),
                        ) {
                            old_waker.wake();
                        }
                    }
                    std::task::Poll::Pending
                }
            } else {
                std::task::Poll::Ready(())
            }
        }
    }

    impl futures_core::FusedFuture for Receiver {
        fn is_terminated(&self) -> bool {
            self.inner.is_none()
        }
    }

    impl Sender {
        pub fn send(&self) {
            self.inner.set.set(true);
            if let Some(waker) = self.inner.waker.borrow_mut().take() {
                waker.wake();
            }
        }
    }

    pub(crate) fn new_pair() -> (Sender, Receiver) {
        let inner = Rc::new(Inner::default());
        (
            Sender {
                inner: inner.clone(),
            },
            Receiver { inner: Some(inner) },
        )
    }
}
