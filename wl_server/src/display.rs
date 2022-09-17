use ::wl_common::interface_message_dispatch;
use ::wl_protocol::wayland::wl_display;
use crate::object_store::InterfaceMeta;
use tracing::debug;

/// Default wl_display implementation
#[derive(Debug)]
pub struct Display;

#[interface_message_dispatch]
impl<Ctx: std::fmt::Debug> wl_display::v1::RequestDispatch<Ctx> for Display {
    type Error = crate::error::Error;
    type SyncFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a, Self: 'a;
    type GetRegistryFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a, Self: 'a;
    fn sync<'a>(
        &'a self,
        ctx: &'a Ctx,
        callback: ::wl_common::types::NewId,
    ) -> Self::SyncFut<'a> {
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
        async move {
            debug!("wl_display.get_registry {}", registry);
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
}
