use std::rc::Rc;

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

#[derive(Debug, Default)]
pub struct OutputState {
    /// A map of shell outputs to their set of object ids
    pub(crate) all_outputs: HashMap<WeakPtr<ShellOutput>, HashSet<u32>>,
}

#[derive(Debug)]
pub struct Output {
    /// The corresponding shell output object.
    pub(crate) output:              WeakPtr<ShellOutput>,
    pub(crate) event_handler_abort: Option<AutoAbortHandle>,
}

#[wayland_object(state = "OutputState")]
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
            let objects = ctx.objects_mut();
            let object = objects.remove(object_id).unwrap();
            let object = object.cast::<Self>().unwrap();

            // Remove ourself from all_outputs
            let Some(state) = objects.get_state_mut::<Self>() else {
                // No bound output objects left anymore
                return Ok(());
            };

            use hashbrown::hash_map::Entry;
            let Entry::Occupied(mut all_outputs_entry) = state.all_outputs.entry(object.output.clone()) else {
                panic!("Output object not found in all_outputs");
            };

            let ids = all_outputs_entry.get_mut();
            ids.remove(&object_id);
            if ids.is_empty() {
                all_outputs_entry.remove();
            }

            Ok(())
        }
    }
}
