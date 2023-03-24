use std::future::Future;

use runa_wayland_protocols::stable::xdg_shell::xdg_wm_base::v5 as xdg_wm_base;
use runa_core::{
    client::traits::Client,
    globals::{Bind, GlobalMeta, MaybeConstInit},
    impl_global_for,
};

#[derive(Debug)]
pub struct WmBase;
impl_global_for!(WmBase);

impl MaybeConstInit for WmBase {
    const INIT: Option<Self> = Some(Self);
}

impl GlobalMeta for WmBase {
    type Object = crate::objects::xdg_shell::WmBase;

    fn new_object(&self) -> Self::Object {
        crate::objects::xdg_shell::WmBase
    }

    fn interface(&self) -> &'static str {
        xdg_wm_base::NAME
    }

    fn version(&self) -> u32 {
        xdg_wm_base::VERSION
    }
}
impl<Ctx: Client> Bind<Ctx> for WmBase {
    type BindFut<'a> = impl Future<Output = std::io::Result<()>> + 'a;

    fn bind<'a>(&'a self, client: &'a mut Ctx, _object_id: u32) -> Self::BindFut<'a> {
        // TODO: setup event
        futures_util::future::ok(())
    }
}
