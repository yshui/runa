//! These are a set of objects the client can acquire. These objects typically
//! implement the RequestDispatch trait of one of the wayland interfaces. As
//! well as a `InterfaceMeta` trait to provide information about the interface,
//! and allowing them to be cast into trait objects and stored together.

use ::wl_common::interface_message_dispatch;
use ::wl_protocol::wayland::wl_display;
use tracing::debug;

use crate::{connection::{InterfaceMeta, Entry}, server::Globals};

/// Default wl_display implementation
#[derive(Debug)]
pub struct Display;

#[interface_message_dispatch]
impl<Ctx: std::fmt::Debug> wl_display::v1::RequestDispatch<Ctx> for Display
where
    Ctx: crate::connection::Connection + crate::connection::Objects + crate::connection::Serial,
    Ctx::Context: crate::server::Globals + crate::server::Server<Connection = Ctx>,
{
    type Error = crate::error::Error;

    type GetRegistryFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a, Self: 'a;
    type SyncFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a, Self: 'a;

    fn sync<'a>(&'a self, ctx: &'a Ctx, callback: ::wl_common::types::NewId) -> Self::SyncFut<'a> {
        async move {
            debug!("wl_display.sync {}", callback);
            Ok(())
        }
    }

    fn get_registry<'a>(
        &'a self,
        ctx: &'a Ctx,
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
            } else {
                // Send invalid id error
            }
            Ok(())
        }
    }
}

impl InterfaceMeta for Display {
    fn interface(&self) -> &'static str {
        "wl_display"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn global(&self) -> Option<u32> {
        // wl_display is always global 1.
        Some(1)
    }

    fn global_remove(&self) {
        unreachable!("wl_display cannot be removed");
    }
}
