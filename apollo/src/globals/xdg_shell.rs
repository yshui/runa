use wl_protocol::stable::xdg_shell::xdg_wm_base::v5 as xdg_wm_base;
use wl_server::{globals::Global, server::Server};
#[derive(Default, Debug)]
pub struct WmBase;

type PinnedFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;
impl<S: Server> Global<S> for WmBase {
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
        Box<dyn wl_server::objects::InterfaceMeta>,
        Option<PinnedFuture<'c, std::io::Result<()>>>,
    )
    where
        'b: 'c,
    {
        (Box::new(crate::objects::xdg_shell::WmBase::default()), None)
    }
}
