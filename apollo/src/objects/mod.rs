use std::rc::{Rc, Weak};

use wl_protocol::wayland::{
    wl_buffer::v1 as wl_buffer, wl_display::v1 as wl_display, wl_output::v4 as wl_output,
};
use wl_server::{
    connection::{Client, Objects, State},
    events::DispatchTo,
    objects::{wayland_object, DISPLAY_ID},
};

use crate::shell::buffers::HasBuffer;

pub mod compositor;
pub mod shm;
pub mod xdg_shell;

#[derive(Debug)]
pub struct Buffer<B> {
    pub buffer: Rc<B>,
}

#[wayland_object]
impl<Ctx, B: 'static> wl_buffer::RequestDispatch<Ctx> for Buffer<B>
where
    Ctx: Client,
    Ctx::ServerContext: HasBuffer<Buffer = B>,
{
    type Error = wl_server::error::Error;

    type DestroyFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn destroy<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        ctx.objects().borrow_mut().remove(object_id);
        async move {
            ctx.send(DISPLAY_ID, wl_display::events::DeleteId { id: object_id })
                .await?;
            Ok(())
        }
    }
}

#[derive(Debug)]
pub struct Output(pub(crate) Weak<crate::shell::output::Output>);

#[wayland_object]
impl<Ctx> wl_output::RequestDispatch<Ctx> for Output
where
    Ctx: Client + DispatchTo<crate::globals::Output> + State<crate::globals::OutputState>,
{
    type Error = wl_server::error::Error;

    type ReleaseFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn release<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::ReleaseFut<'a> {
        ctx.objects().borrow_mut().remove(object_id);
        if let Some(output) = self.0.upgrade() {
            let handle = (
                ctx.event_handle(),
                <Ctx as DispatchTo<crate::globals::Output>>::SLOT,
            );
            output.remove_change_listener(&handle);
        }
        async move {
            ctx.send(DISPLAY_ID, wl_display::events::DeleteId { id: object_id })
                .await?;
            Ok(())
        }
    }
}
