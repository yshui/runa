//! These are a set of objects the client can acquire. These objects typically
//! implement the RequestDispatch trait of one of the wayland interfaces. As
//! well as a `InterfaceMeta` trait to provide information about the interface,
//! and allowing them to be cast into trait objects and stored together.

use std::future::Future;

use ::wl_common::interface_message_dispatch;
use ::wl_protocol::wayland::{wl_callback, wl_display};
use tracing::debug;

use crate::{
    connection::{Entry, InterfaceMeta, Objects},
    server::{Globals, Server},
};

async fn send_id_in_use<Ctx>(ctx: &Ctx, object_id: u32) -> Result<(), Ctx::Error>
where
    Ctx: crate::connection::Connection,
{
    use std::ffi::CStr;
    let message = CStr::from_bytes_with_nul(b"invalid method for wl_callback\0").unwrap();
    ctx.send(
        DISPLAY_ID,
        wl_display::v1::Event::Error(wl_display::v1::events::Error {
            code:      wl_display::v1::enums::Error::InvalidMethod as u32,
            object_id: wl_common::types::Object(object_id),
            message:   wl_common::types::Str(message),
        }),
    )
    .await
}

#[derive(Debug)]
pub struct Callback;

impl<Ctx> wl_common::InterfaceMessageDispatch<Ctx> for Callback
where
    Ctx: crate::connection::Connection,
    crate::error::Error: From<Ctx::Error>,
{
    type Error = crate::error::Error;

    type Fut<'a, R> = impl Future<Output = Result<(), Self::Error>> where R: 'a + wl_io::AsyncBufReadWithFd, Ctx: 'a;

    fn dispatch<'a, R>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        _reader: &mut wl_io::de::Deserializer<'a, R>,
    ) -> Self::Fut<'a, R>
    where
        R: wl_io::AsyncBufReadWithFd,
    {
        use std::ffi::CStr;

        use futures_util::TryFutureExt;
        let message = CStr::from_bytes_with_nul(b"invalid method for wl_callback\0").unwrap();
        ctx.send(
            DISPLAY_ID,
            wl_display::v1::Event::Error(wl_display::v1::events::Error {
                code:      wl_display::v1::enums::Error::InvalidMethod as u32,
                object_id: wl_common::types::Object(object_id),
                message:   wl_common::types::Str(message),
            }),
        )
        .map_err(Into::into)
    }
}

impl InterfaceMeta for Callback {
    fn interface(&self) -> &'static str {
        wl_callback::v1::NAME
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The object ID of the wl_display object
pub const DISPLAY_ID: u32 = 1;

/// Default wl_display implementation
#[derive(Debug)]
pub struct Display;

#[interface_message_dispatch]
impl<Ctx> wl_display::v1::RequestDispatch<Ctx> for Display
where
    Ctx: crate::connection::Connection
        + crate::connection::Objects
        + wl_common::Serial
        + std::fmt::Debug,
    Ctx::Context: crate::server::Globals<Ctx::Context> + crate::server::Server<Connection = Ctx>,
    crate::error::Error: From<Ctx::Error>,
{
    type Error = crate::error::Error;

    type GetRegistryFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a, Self: 'a;
    type SyncFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a, Self: 'a;

    fn sync<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        callback: ::wl_common::types::NewId,
    ) -> Self::SyncFut<'a> {
        async move {
            debug!("wl_display.sync {}", callback);
            if Objects::get(ctx, callback.0).is_some() {
                send_id_in_use(ctx, callback.0).await?;
            } else {
                ctx.send(
                    callback.0,
                    wl_callback::v1::Event::Done(wl_callback::v1::events::Done {
                        // TODO: setup event serial
                        callback_data: 0,
                    }),
                )
                .await?;
                ctx.send(
                    DISPLAY_ID,
                    wl_display::v1::Event::DeleteId(wl_display::v1::events::DeleteId {
                        id: callback.0,
                    }),
                )
                .await?;
            }
            Ok(())
        }
    }

    fn get_registry<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        registry: ::wl_common::types::NewId,
    ) -> Self::GetRegistryFut<'a> {
        debug!("wl_display.get_registry {}", registry);
        let inserted = ctx.with_entry(registry.0, |entry| {
            if entry.is_vacant() {
                let server_context = ctx.server_context();
                let object = server_context.bind_registry(ctx);
                entry.or_insert_boxed(object);
                true
            } else {
                false
            }
        });
        async move {
            if inserted {
                // Send the global events
                debug!("Sending global events");
            } else {
                send_id_in_use(ctx, registry.0).await?;
            }
            Ok(())
        }
    }
}

impl InterfaceMeta for Display {
    fn interface(&self) -> &'static str {
        wl_display::v1::NAME
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
