use std::rc::Rc;

use wl_protocol::wayland::{
    wl_buffer::v1 as wl_buffer, wl_display::v1 as wl_display, wl_output::v4 as wl_output,
};
use wl_server::{
    connection::traits::{Client, LockableStore, Store, WriteMessage},
    objects::{wayland_object, DISPLAY_ID},
};

use crate::{
    shell::{buffers::HasBuffer, output::Output as ShellOutput, HasShell},
    utils::WeakPtr,
};

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

    fn destroy(ctx: &mut Ctx, object_id: u32) -> Self::DestroyFut<'_> {
        async move {
            let objects = ctx.objects();
            let mut objects = objects.lock().await;
            objects.remove(object_id);
            ctx.connection()
                .send(DISPLAY_ID, wl_display::events::DeleteId { id: object_id })
                .await?;
            Ok(())
        }
    }
}

#[derive(Debug)]
pub struct Output {
    pub(crate) output:     Option<WeakPtr<ShellOutput>>,
    pub(crate) event_task: Option<futures_util::future::AbortHandle>,
}
impl Drop for Output {
    fn drop(&mut self) {
        if let Some(handle) = self.event_task.take() {
            handle.abort();
        }
    }
}

#[wayland_object]
impl<Ctx> wl_output::RequestDispatch<Ctx> for Output
where
    Ctx::ServerContext: HasShell,
    Ctx: Client,
{
    type Error = wl_server::error::Error;

    type ReleaseFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn release(ctx: &mut Ctx, object_id: u32) -> Self::ReleaseFut<'_> {
        async move {
            let objects = ctx.objects();
            let mut objects = objects.lock().await;
            let _ = objects.remove(object_id).unwrap();
            ctx.connection()
                .send(DISPLAY_ID, wl_display::events::DeleteId { id: object_id })
                .await?;
            Ok(())
        }
    }
}
