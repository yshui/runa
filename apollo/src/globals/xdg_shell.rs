//! Globals defined in the xdg_shell protocol.

use std::future::Future;

use runa_core::{
    client::traits::Client,
    globals::{Bind, MonoGlobal},
};
use runa_wayland_protocols::stable::xdg_shell::xdg_wm_base::v5 as xdg_wm_base;

/// Implementation of the `xdg_wm_base` global.
#[derive(Debug, Clone, Copy)]
pub struct WmBase;

impl MonoGlobal for WmBase {
    type Object = crate::objects::xdg_shell::WmBase;

    const INTERFACE: &'static str = xdg_wm_base::NAME;
    const MAYBE_DEFAULT: Option<Self> = Some(Self);
    const VERSION: u32 = xdg_wm_base::VERSION;

    #[inline]
    fn new_object() -> Self::Object {
        crate::objects::xdg_shell::WmBase
    }
}
impl<Ctx: Client> Bind<Ctx> for WmBase {
    type BindFut<'a> = impl Future<Output = std::io::Result<()>> + 'a;

    fn bind<'a>(&'a self, _client: &'a mut Ctx, _object_id: u32) -> Self::BindFut<'a> {
        // TODO: setup event
        futures_util::future::ok(())
    }
}
