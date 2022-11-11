use std::future::Pending;

use wl_common::Infallible;
use wl_protocol::stable::xdg_shell::xdg_wm_base::v5 as xdg_wm_base;
use wl_server::{
    connection::Connection,
    global_dispatch,
    globals::{Global, GlobalMeta},
    server::Server,
};

use crate::shell::HasShell;
#[derive(Debug)]
pub struct WmBase;

impl<Ctx> Global<Ctx> for WmBase
where
    Ctx: Connection,
    Ctx::Context: HasShell,
{
    type Error = wl_server::error::Error;
    type HandleEventsError = Infallible;
    type HandleEventsFut<'a> = Pending<Result<(), Self::HandleEventsError>> where Ctx: 'a;

    const INIT: Self = Self;

    global_dispatch! {
        "xdg_wm_base" => crate::objects::xdg_shell::WmBase,
        "xdg_surface" => crate::objects::xdg_shell::Surface<Ctx>,
        "xdg_toplevel" => crate::objects::xdg_shell::TopLevel<Ctx>,
    }

    fn handle_events<'a>(_ctx: &'a mut Ctx, _slot: usize) -> Option<Self::HandleEventsFut<'a>> {
        None
    }
}

type PinnedFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;
impl<S: Server> GlobalMeta<S> for WmBase {
    fn interface(&self) -> &'static str {
        xdg_wm_base::NAME
    }

    fn version(&self) -> u32 {
        xdg_wm_base::VERSION
    }

    fn bind<'b, 'c>(
        &self,
        _client: &'b <S as Server>::Connection,
        _object_id: u32,
    ) -> (
        Box<dyn wl_server::objects::Object<S::Connection>>,
        Option<PinnedFuture<'c, std::io::Result<()>>>,
    )
    where
        'b: 'c,
    {
        (
            Box::new(crate::objects::xdg_shell::WmBase),
            None,
        )
    }
}
