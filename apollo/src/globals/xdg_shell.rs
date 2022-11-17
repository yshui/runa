use wl_protocol::stable::xdg_shell::xdg_wm_base::v5 as xdg_wm_base;
use wl_server::{
    connection::Connection,
    global_dispatch,
    globals::{Global, GlobalDispatch, GlobalMeta},
    objects::Object,
};

use crate::shell::HasShell;
#[derive(Debug)]
pub struct WmBase;

impl<Ctx> GlobalDispatch<Ctx> for WmBase
where
    Ctx: Connection,
    Ctx::Context: HasShell,
{
    type Error = wl_server::error::Error;

    const INIT: Self = Self;

    global_dispatch! {
        "xdg_wm_base" => crate::objects::xdg_shell::WmBase,
        "xdg_surface" => crate::objects::xdg_shell::Surface<Ctx>,
        "xdg_toplevel" => crate::objects::xdg_shell::TopLevel<Ctx>,
    }
}

type PinnedFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;
impl GlobalMeta for WmBase {
    fn interface(&self) -> &'static str {
        xdg_wm_base::NAME
    }

    fn version(&self) -> u32 {
        xdg_wm_base::VERSION
    }
}
impl<Ctx> Global<Ctx> for WmBase {
    fn bind<'b, 'c>(
        &self,
        _client: &'b mut Ctx,
        _object_id: u32,
    ) -> PinnedFuture<'c, std::io::Result<Box<dyn Object>>>
    where
        'b: 'c,
    {
        Box::pin(futures_util::future::ok(
            Box::new(crate::objects::xdg_shell::WmBase) as _,
        ))
    }
}
