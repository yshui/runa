use std::future::Future;

use hashbrown::{HashMap, HashSet};
use runa_io::traits::WriteMessage;
use runa_wayland_protocols::wayland::{wl_buffer::v1 as wl_buffer, wl_output::v4 as wl_output};
use runa_core::{
    client::{
        event_handlers::{Abortable, AutoAbortHandle},
        traits::{Client, EventDispatcher, EventHandler, EventHandlerAction, Store},
    },
    error::Error,
    objects::wayland_object,
};

use crate::{
    shell::{
        buffers::{AttachableBuffer, BufferEvent, BufferLike, HasBuffer},
        output::Output as ShellOutput,
        HasShell,
    },
    utils::WeakPtr,
};

pub mod compositor;
pub mod input;
pub mod shm;
pub mod xdg_shell;

#[derive(Debug)]
pub struct Buffer<B> {
    pub(crate) buffer:    AttachableBuffer<B>,
    _event_handler_abort: AutoAbortHandle,
}

impl<B: BufferLike> Buffer<B> {
    pub fn new<Ctx: Client, B2: Into<B>, E: EventDispatcher<Ctx>>(
        buffer: B2,
        event_dispatcher: &mut E,
    ) -> Self {
        struct BufferEventHandler {
            object_id: u32,
        }
        impl<Ctx: Client> EventHandler<Ctx> for BufferEventHandler {
            type Message = BufferEvent;

            type Future<'ctx> = impl Future<
                    Output = Result<EventHandlerAction, Box<dyn std::error::Error + Send + Sync>>,
                > + 'ctx;

            fn handle_event<'ctx>(
                &'ctx mut self,
                objects: &'ctx mut Ctx::ObjectStore,
                connection: &'ctx mut Ctx::Connection,
                server_context: &'ctx Ctx::ServerContext,
                message: &'ctx mut Self::Message,
            ) -> Self::Future<'ctx> {
                async move {
                    connection
                        .send(self.object_id, wl_buffer::events::Release {})
                        .await?;
                    Ok(EventHandlerAction::Keep)
                }
            }
        }
        let buffer = buffer.into();
        let object_id = buffer.object_id();
        let event_handler = BufferEventHandler { object_id };
        let (event_handler, abort_handle) = Abortable::new(event_handler);
        let ret = Self {
            buffer:               AttachableBuffer::new(buffer),
            _event_handler_abort: abort_handle.auto_abort(),
        };
        let rx = ret.buffer.inner.subscribe();
        event_dispatcher.add_event_handler(rx, event_handler);
        ret
    }

    pub fn buffer(&self) -> &B {
        &self.buffer.inner
    }
}

#[wayland_object]
impl<Ctx, B: 'static> wl_buffer::RequestDispatch<Ctx> for Buffer<B>
where
    Ctx: Client,
    Ctx::ServerContext: HasBuffer<Buffer = B>,
{
    type Error = Error;

    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

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
    type Error = Error;

    type ReleaseFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn release(ctx: &mut Ctx, object_id: u32) -> Self::ReleaseFut<'_> {
        async move {
            use runa_core::objects::AnyObject;
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
