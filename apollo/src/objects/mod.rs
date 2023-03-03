use std::{cell::RefCell, rc::Rc};

use hashbrown::{HashMap, HashSet};
use wl_protocol::wayland::{wl_buffer::v1 as wl_buffer, wl_output::v4 as wl_output};
use wl_server::{
    connection::{
        event_handler::AutoAbortHandle,
        traits::{Client, Store},
    },
    objects::wayland_object,
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
        ctx.objects_mut().remove(object_id);
        futures_util::future::ok(())
    }
}

#[derive(Debug)]
pub struct Output {
    /// The corresponding shell output object.
    pub(crate) output:              WeakPtr<ShellOutput>,
    /// A map of all outputs to their object ids in the client owning this
    /// output object.
    pub(crate) all_outputs: Option<Rc<RefCell<HashMap<WeakPtr<ShellOutput>, HashSet<u32>>>>>,
    pub(crate) event_handler_abort: Option<AutoAbortHandle>,
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
            use wl_server::objects::AnyObject;

            // Remove ourself from all_outputs
            let mut object = ctx.objects_mut().remove(object_id).unwrap();
            let object = object.cast_mut::<Self>().unwrap();
            let all_outputs = object.all_outputs.take().unwrap();
            let mut all_outputs = all_outputs.borrow_mut();
            let all_outputs_entry = all_outputs.get_mut(&object.output).unwrap();
            all_outputs_entry.remove(&object_id);
            if all_outputs_entry.is_empty() {
                all_outputs.remove(&object.output);
            }

            Ok(())
        }
    }
}
