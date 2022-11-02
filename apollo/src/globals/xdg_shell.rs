use std::{cell::RefCell, future::Pending, marker::PhantomData};

use wl_common::Infallible;
use wl_protocol::stable::xdg_shell::xdg_wm_base::v5 as xdg_wm_base;
use wl_server::{
    connection::Connection,
    global_dispatch,
    globals::{Global, GlobalMeta},
    server::Server,
    Extra,
};

use crate::shell::Shell;
pub struct WmBase<S>(PhantomData<S>);
impl<S> Default for WmBase<S> {
    fn default() -> Self {
        Self(PhantomData)
    }
}
impl<S> std::fmt::Debug for WmBase<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WmBase").finish()
    }
}

impl<Ctx, S: Shell> Global<Ctx> for WmBase<S>
where
    Ctx: Connection,
    Ctx::Context: Extra<RefCell<S>>,
{
    type Error = wl_server::error::Error;
    type HandleEventsError = Infallible;
    type HandleEventsFut<'a> = Pending<Result<(), Self::HandleEventsError>> where Ctx: 'a;

    const INIT: Self = Self(PhantomData);

    global_dispatch! {
        "xdg_wm_base" => crate::objects::xdg_shell::WmBase<S>,
    }

    fn handle_events<'a>(_ctx: &'a mut Ctx, _slot: usize) -> Option<Self::HandleEventsFut<'a>> {
        None
    }
}

type PinnedFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;
impl<Sh: Shell, S: Server> GlobalMeta<S> for WmBase<Sh> {
    fn interface(&self) -> &'static str {
        xdg_wm_base::NAME
    }

    fn version(&self) -> u32 {
        xdg_wm_base::VERSION
    }

    fn provide<'a>(&'a self, demand: &mut wl_server::provide_any::Demand<'a>) {
        demand.provide_ref(self);
    }

    fn bind<'b, 'c>(
        &self,
        client: &'b <S as Server>::Connection,
        object_id: u32,
    ) -> (
        Box<dyn wl_server::objects::InterfaceMeta<S::Connection>>,
        Option<PinnedFuture<'c, std::io::Result<()>>>,
    )
    where
        'b: 'c,
    {
        (
            Box::new(crate::objects::xdg_shell::WmBase::<Sh>::default()),
            None,
        )
    }
}
