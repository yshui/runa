//! Per-client state
//!
//! A wayland compositor needs to maintain a set of states for each
//! connected wayland client. For example, each client could have a set of bound
//! objects. This module defines a common interface for such state types, so a
//! crate that implements different wayland protcol interfaces can rely on this
//! common interface and be generic over whatever state type the compositor
//! author chooses to use.
//!
//! See [`traits`] for definition of the interface. The rest of this module
//! provides some reference implementations of these interfaces.
//!
//! The documentation on [`traits::Client`] should give you a good idea of how
//! most of the stuffs fit together.
//!
//! See also the crate level documentation for how `Client` fits into the
//! overall picture.

/// Wayland divides the ID space into a client portion and a server portion. The
/// client is only allowed to allocate IDs up to this number, above this number
/// are reserved for the the server.
const CLIENT_MAX_ID: u32 = 0xfeffffff;

pub mod event_dispatcher;
pub mod store;
pub mod traits;

use std::{os::fd::RawFd, pin::Pin};

#[doc(inline)]
pub use event_dispatcher::EventDispatcher;
use runa_io::traits::ReadMessage;
#[doc(inline)]
pub use store::Store;

/// Reference implementation of the [`traits::Client::dispatch`] method.
///
/// This function reads a message from `reader`, looks up the corresponding
/// object, and then calls the object's `dispatch` method to handle the message.
/// It then checks if the `dispatch` method returned an error, and if so, sends
/// the error to the client. The returned future resolves to a boolean, which
/// indicates whether the client should be disconnected.
///
/// You can simply call this function as the implementation of your own
/// `dispatch` method.
///
/// Notice there is a type bound requiring `Ctx::Object` to accept a `(&[u8],
/// &[RawFd])` as its request type. This would be the case if you use
/// [`#[derive(Object)`](crate::objects::Object) to generate you object type. If
/// not, you are supposed to deserialize a wayland message from a
/// `(&[u8], &[RawFd])` tuple yourself and then dispatch the deserialized
/// message properly.
pub async fn dispatch_to<'a, Ctx, R>(ctx: &'a mut Ctx, mut reader: Pin<&'a mut R>) -> bool
where
    R: ReadMessage,
    Ctx: traits::Client,
    Ctx::Object: for<'b> crate::objects::Object<Ctx, Request<'b> = (&'b [u8], &'b [RawFd])>,
{
    use runa_io::traits::{RawMessage, WriteMessage};
    use runa_wayland_protocols::wayland::wl_display::v1 as wl_display;
    use runa_wayland_types as types;
    use traits::Store;

    use crate::{error::ProtocolError, objects::AnyObject};
    let RawMessage {
        object_id,
        len,
        data,
        fds,
    } = match R::next_message(reader.as_mut()).await {
        Ok(v) => v,
        // I/O error, no point sending the error to the client
        Err(_) => return true,
    };
    let (ret, bytes_read, fds_read) = <<Ctx as traits::Client>::Object as crate::objects::Object<
        Ctx,
    >>::dispatch(ctx, object_id, (data, fds))
    .await;
    let (mut fatal, error) = match ret {
        Ok(_) => (false, None),
        Err(e) => (
            e.fatal(),
            e.wayland_error()
                .map(|(object_id, error_code)| (object_id, error_code, e.to_string())),
        ),
    };
    if let Some((object_id, error_code, msg)) = error {
        // We are going to disconnect the client so we don't care about the
        // error.
        fatal |= ctx
            .connection_mut()
            .send(crate::objects::DISPLAY_ID, wl_display::events::Error {
                object_id: types::Object(object_id),
                code:      error_code,
                message:   types::Str(msg.as_bytes()),
            })
            .await
            .is_err();
    }
    if !fatal {
        if bytes_read != len {
            let len_opcode = u32::from_ne_bytes(data[0..4].try_into().unwrap());
            let opcode = len_opcode & 0xffff;
            tracing::error!(
                "unparsed bytes in buffer, read ({bytes_read}) != received ({len}). object_id: \
                 {}@{object_id}, opcode: {opcode}",
                ctx.objects()
                    .get::<Ctx::Object>(object_id)
                    .map(|o| o.interface())
                    .unwrap_or("unknown")
            );
            fatal = true;
        }
        reader.consume(bytes_read, fds_read);
    }
    fatal
}

#[doc(hidden)] // not ready for use yet
pub mod serial {
    use hashbrown::HashMap;

    #[derive(Debug)]
    pub struct EventSerial<D> {
        serials:     HashMap<u32, (D, std::time::Instant)>,
        last_serial: u32,
        expire:      std::time::Duration,
    }

    /// A serial allocator for event serials. Serials are automatically
    /// forgotten after a set amount of time.
    impl<D> EventSerial<D> {
        pub fn new(expire: std::time::Duration) -> Self {
            Self {
                serials: HashMap::new(),
                last_serial: 0,
                expire,
            }
        }

        fn clean_up(&mut self) {
            let now = std::time::Instant::now();
            self.serials.retain(|_, (_, t)| *t + self.expire > now);
        }
    }

    impl<D: 'static> crate::Serial for EventSerial<D> {
        type Data = D;
        type Iter<'a> = <&'a Self as IntoIterator>::IntoIter;

        fn next_serial(&mut self, data: Self::Data) -> u32 {
            self.last_serial += 1;

            self.clean_up();
            if self
                .serials
                .insert(self.last_serial, (data, std::time::Instant::now()))
                .is_some()
            {
                panic!(
                    "serial {} already in use, expiration duration too long?",
                    self.last_serial
                );
            }
            self.last_serial
        }

        fn get(&self, serial: u32) -> Option<&Self::Data> {
            let now = std::time::Instant::now();
            self.serials.get(&serial).and_then(|(d, timestamp)| {
                if *timestamp + self.expire > now {
                    Some(d)
                } else {
                    None
                }
            })
        }

        fn expire(&mut self, serial: u32) -> bool {
            self.clean_up();
            self.serials.remove(&serial).is_some()
        }

        fn iter(&self) -> Self::Iter<'_> {
            self.into_iter()
        }
    }
    impl<'a, D: 'a> IntoIterator for &'a EventSerial<D> {
        type Item = (u32, &'a D);

        type IntoIter = impl Iterator<Item = Self::Item> + 'a where Self: 'a;

        fn into_iter(self) -> Self::IntoIter {
            self.serials.iter().map(|(k, (d, _))| (*k, d))
        }
    }
}
