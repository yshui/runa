use std::rc::Rc;

use wl_common::interface_message_dispatch;
use wl_protocol::wayland::{wl_buffer::v1 as wl_buffer, wl_display::v1 as wl_display};
use wl_server::{
    connection::Connection,
    objects::{Object, DISPLAY_ID},
};

use crate::shell::buffers::HasBuffer;

pub mod compositor;
pub mod shm;
pub mod xdg_shell;

pub struct Buffer<B> {
    pub buffer: Rc<B>,
}
impl<B: 'static> Object for Buffer<B> {
    fn interface(&self) -> &'static str {
        wl_buffer::NAME
    }
}

#[interface_message_dispatch]
impl<Ctx, B: 'static> wl_buffer::RequestDispatch<Ctx> for Buffer<B>
where
    Ctx: Connection,
    Ctx::Context: HasBuffer<Buffer = B>,
{
    type Error = wl_server::error::Error;

    type DestroyFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn destroy<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        use wl_server::connection::Objects;
        async move {
            ctx.objects().borrow_mut().remove(object_id);
            ctx.send(DISPLAY_ID, wl_display::events::DeleteId { id: object_id })
                .await?;
            Ok(())
        }
    }
}
