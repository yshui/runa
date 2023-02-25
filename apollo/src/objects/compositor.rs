//! Proxy for the compositor global
//!
//! The compositor global is responsible for managing the sets of surfaces a
//! client has. According to the wayland spec, each surface has a set of
//! double-buffered states: updates are made to the pending state first, and
//! applied to current state when `wl_surface.commit` is called.
//!
//! Another core interface, wl_subsurface, add some persistent data structure
//! flavor to the mix. A subsurface can be in synchronized mode, and state
//! commit will create a new "version" of the surface tree. And visibility of
//! changes can be propagated from bottom up through commits on the
//! parent, grand-parent, etc.
//!
//! We deal with this requirement with COW (copy-on-write) techniques. Details
//! are documented in the types' document.
use std::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{ready, Poll},
};

use derivative::Derivative;
use hashbrown::{HashMap, HashSet};
use wl_protocol::wayland::{
    wl_buffer::v1 as wl_buffer, wl_compositor::v5 as wl_compositor, wl_display::v1 as wl_display,
    wl_output::v4 as wl_output, wl_subcompositor::v1 as wl_subcompositor,
    wl_subsurface::v1 as wl_subsurface, wl_surface::v5 as wl_surface,
};
use wl_server::{
    connection::traits::{
        Client, EventHandler, EventHandlerAction, LockableStore, Store, WriteMessage,
    },
    error,
    events::EventSource,
    objects::{wayland_object, ObjectMeta, DISPLAY_ID},
};

use crate::{
    shell::{
        self,
        buffers::HasBuffer,
        output::Output as ShellOutput,
        surface::{roles, OutputEvent},
        HasShell, Shell, ShellOf,
    },
    utils::{geometry::Point, WeakPtr},
};

/// A set of buffers that are shared among all the surfaces of a client, used by
/// the event handling tasks.
#[derive(Debug, Default)]
pub(crate) struct SharedSurfaceBuffers {
    /// The set of outputs that overlap with the surface, used by the
    /// output_changed event handler.
    pub(crate) new_outputs: HashSet<WeakPtr<shell::output::Output>>,
    pub(crate) deleted:     Vec<u32>,
    pub(crate) added:       Vec<u32>,
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Surface<Shell: shell::Shell> {
    pub(crate) inner: Rc<crate::shell::surface::Surface<Shell>>,
    /// A queue of surface state tokens used for surface commits and
    /// destructions.
    scratch_buffer:   Rc<RefCell<Vec<Shell::Token>>>,
}

fn deallocate_surface<Ctx: Client>(
    this: &mut Surface<<Ctx::ServerContext as HasShell>::Shell>,
    ctx: &mut Ctx,
) where
    Ctx::ServerContext: HasShell,
{
    let server_context = ctx.server_context().clone();
    let mut shell = server_context.shell().borrow_mut();
    this.inner
        .destroy(&mut shell, &mut this.scratch_buffer.borrow_mut());

    // Disconnected, no point sending DeleteId for those callbacks
    this.inner.frame_callbacks().borrow_mut().clear();
}

#[derive(Debug)]
struct SurfaceError {
    kind:      wl_surface::enums::Error,
    object_id: u32,
}

impl std::fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use wl_surface::enums::Error::*;
        match self.kind {
            InvalidScale => write!(f, "invalid scale"),
            InvalidTransform => {
                write!(f, "invalid transform")
            },
            InvalidOffset => write!(f, "invalid offset"),
            InvalidSize => write!(f, "invalid size"),
        }
    }
}

impl std::error::Error for SurfaceError {}

impl wl_protocol::ProtocolError for SurfaceError {
    fn wayland_error(&self) -> Option<(u32, u32)> {
        Some((self.object_id, self.kind as u32))
    }

    fn fatal(&self) -> bool {
        true
    }
}

#[wayland_object(on_disconnect = "deallocate_surface")]
impl<Ctx: Client, S: shell::Shell> wl_surface::RequestDispatch<Ctx> for Surface<S>
where
    Ctx::ServerContext: HasShell<Shell = S> + HasBuffer<Buffer = S::Buffer>,
    Ctx::Object: From<wl_server::objects::Callback>,
{
    type Error = error::Error;

    type AttachFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type CommitFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type DamageBufferFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type DamageFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type FrameFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type OffsetFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type SetBufferScaleFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type SetBufferTransformFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type SetInputRegionFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type SetOpaqueRegionFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn frame(ctx: &mut Ctx, object_id: u32, callback: wl_types::NewId) -> Self::FrameFut<'_> {
        async move {
            let objects = ctx.objects();
            let mut objects = objects.lock().await;
            let surface = objects
                .get(object_id)
                .unwrap()
                .cast::<Self>()
                .unwrap()
                .inner
                .clone();
            let inserted = objects.try_insert_with(callback.0, || {
                let mut shell = ctx.server_context().shell().borrow_mut();
                let state = surface.pending_mut(&mut shell);
                state.add_frame_callback(callback.0);
                wl_server::objects::Callback::default().into()
            });
            if !inserted {
                Err(error::Error::IdExists(callback.0))
            } else {
                Ok(())
            }
        }
    }

    fn attach(
        ctx: &mut Ctx,
        object_id: u32,
        buffer: wl_types::Object,
        x: i32,
        y: i32,
    ) -> Self::AttachFut<'_> {
        async move {
            if x != 0 || y != 0 {
                return Err(error::Error::custom(SurfaceError {
                    object_id,
                    kind: wl_surface::enums::Error::InvalidOffset,
                }))
            }
            let objects = ctx.objects();
            let objects = objects.lock().await;
            let this = objects.get(object_id).unwrap().cast::<Self>().unwrap();
            let buffer_id = buffer.0;
            let Some(buffer) = objects.get(buffer.0) else { return Err(error::Error::UnknownObject(buffer_id)); };
            if buffer.interface() != wl_buffer::NAME {
                return Err(error::Error::InvalidObject(buffer_id))
            }
            let mut shell = ctx.server_context().shell().borrow_mut();
            let state = this.inner.pending_mut(&mut shell);
            let buffer = buffer
                .cast::<crate::objects::Buffer<<Ctx::ServerContext as HasBuffer>::Buffer>>()
                .unwrap();
            state.set_buffer(Some(buffer.buffer.clone()));
            Ok(())
        }
    }

    fn damage(
        ctx: &mut Ctx,
        object_id: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Self::DamageFut<'_> {
        async move {
            let objects = ctx.objects();
            let objects = objects.lock().await;
            let this = objects.get(object_id).unwrap().cast::<Self>().unwrap();
            let mut shell = ctx.server_context().shell().borrow_mut();
            let state = this.inner.pending_mut(&mut shell);
            state.damage_buffer();
            Ok(())
        }
    }

    fn damage_buffer(
        ctx: &mut Ctx,
        object_id: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Self::DamageBufferFut<'_> {
        async move {
            let objects = ctx.objects();
            let objects = objects.lock().await;
            let this = objects.get(object_id).unwrap().cast::<Self>().unwrap();
            let mut shell = ctx.server_context().shell().borrow_mut();
            let state = this.inner.pending_mut(&mut shell);
            state.damage_buffer();
            Ok(())
        }
    }

    fn commit(ctx: &mut Ctx, object_id: u32) -> Self::CommitFut<'_> {
        async move {
            let objects = ctx.objects();
            let objects = objects.lock().await;
            let this = objects.get(object_id).unwrap().cast::<Self>().unwrap();

            use crate::shell::buffers::Buffer;
            let server_context = ctx.server_context().clone();
            let released_buffer = {
                let mut shell = server_context.shell().borrow_mut();
                let old_buffer = this.inner.current(&shell).buffer().cloned();
                this.inner
                    .commit(&mut shell, &mut this.scratch_buffer.borrow_mut())
                    .map_err(wl_server::error::Error::UnknownFatalError)?;

                let new_buffer = this.inner.current(&shell).buffer().cloned();
                drop(shell);

                // TODO: this released_buffer calculation is wrong.
                if let Some(old_buffer) = old_buffer {
                    if new_buffer.map_or(true, |new_buffer| !Rc::ptr_eq(&old_buffer, &new_buffer)) {
                        Some(old_buffer.object_id())
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(released_buffer) = released_buffer {
                let mut conn = ctx.connection().clone();
                conn.send(released_buffer, wl_buffer::events::Release {})
                    .await?;
            }
            Ok(())
        }
    }

    fn set_buffer_scale(ctx: &mut Ctx, object_id: u32, scale: i32) -> Self::SetBufferScaleFut<'_> {
        async move {
            let objects = ctx.objects();
            let objects = objects.lock().await;
            let this = objects.get(object_id).unwrap().cast::<Self>().unwrap();
            let mut shell = ctx.server_context().shell().borrow_mut();
            let pending_mut = this.inner.pending_mut(&mut shell);

            pending_mut.set_buffer_scale(scale as u32 * 120);
            Ok(())
        }
    }

    fn set_input_region(
        ctx: &mut Ctx,
        object_id: u32,
        region: wl_types::Object,
    ) -> Self::SetInputRegionFut<'_> {
        async move { unimplemented!() }
    }

    fn set_opaque_region(
        ctx: &mut Ctx,
        object_id: u32,
        region: wl_types::Object,
    ) -> Self::SetOpaqueRegionFut<'_> {
        async move { unimplemented!() }
    }

    fn set_buffer_transform(
        ctx: &mut Ctx,
        object_id: u32,
        transform: wl_output::enums::Transform,
    ) -> Self::SetBufferTransformFut<'_> {
        async move { unimplemented!() }
    }

    fn offset(ctx: &mut Ctx, object_id: u32, x: i32, y: i32) -> Self::OffsetFut<'_> {
        async move { unimplemented!() }
    }

    fn destroy(ctx: &mut Ctx, object_id: u32) -> Self::DestroyFut<'_> {
        async move {
            let server_context = ctx.server_context().clone();
            let objects = ctx.objects();
            let mut objects = objects.lock().await;
            let mut conn = ctx.connection().clone();
            let this = objects.remove(object_id).unwrap();
            let this: &Self = this.cast().unwrap();

            this.inner.destroy(
                &mut server_context.shell().borrow_mut(),
                &mut this.scratch_buffer.borrow_mut(),
            );

            let mut frame_callbacks =
                std::mem::take(&mut *this.inner.frame_callbacks().borrow_mut());
            for frame_callback in frame_callbacks.drain(..) {
                conn.send(DISPLAY_ID, wl_display::events::DeleteId {
                    id: frame_callback,
                })
                .await?;
            }

            conn.send(DISPLAY_ID, wl_display::events::DeleteId { id: object_id })
                .await?;
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Default)]
struct CompositorInner {
    shared_surface_buffers: Rc<RefCell<SharedSurfaceBuffers>>,
}

/// The reference implementation of wl_compositor
#[derive(Debug, Default, Clone)]
pub struct Compositor {
    inner: Option<CompositorInner>,
}

impl Compositor {
    pub fn new() -> Self {
        Self {
            inner: Some(CompositorInner {
                shared_surface_buffers: Default::default(),
            }),
        }
    }

    pub fn is_complete(&self) -> bool {
        self.inner.is_some()
    }
}

#[wayland_object]
impl<Ctx: Client> wl_compositor::RequestDispatch<Ctx> for Compositor
where
    Ctx::ServerContext: HasShell,
    Ctx::Object: From<Surface<<Ctx::ServerContext as HasShell>::Shell>>,
{
    type Error = error::Error;

    type CreateRegionFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type CreateSurfaceFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn create_surface(
        ctx: &mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
    ) -> Self::CreateSurfaceFut<'_> {
        use crate::shell::surface;
        async move {
            let objects = ctx.objects();
            let mut objects = objects.lock().await;
            if objects.get(id.0).is_none() {
                // Add the id to the list of surfaces
                let shell = ctx.server_context().shell();
                let mut shell = shell.borrow_mut();
                let surface = Rc::new(surface::Surface::new(id));
                let current = shell.allocate(surface::SurfaceState::new(surface.clone()));
                let current_state = shell.get_mut(current);
                current_state.stack_index = Some(current_state.stack_mut().push_back(
                    crate::shell::surface::SurfaceStackEntry {
                        token:    current,
                        position: Point::new(0, 0),
                    },
                ));
                surface.set_current(current);
                surface.set_pending(current);
                shell.post_commit(None, current);

                // Copy the shared scratch_buffer from an existing surface object
                let scratch_buffer = objects
                    .by_type::<Surface<ShellOf<Ctx::ServerContext>>>()
                    .next()
                    .map(|(_, obj)| obj.scratch_buffer.clone())
                    .unwrap_or_default();
                let shared_surface_buffers = objects
                    .get(object_id)
                    .expect("missing object")
                    .cast::<Self>()
                    .expect("wrong type")
                    .inner
                    .as_ref()
                    .expect("missing shared buffers")
                    .shared_surface_buffers
                    .clone();
                tracing::debug!("id {} is surface {:p}", id.0, surface);
                let rx = <_ as EventSource<OutputEvent>>::subscribe(&*surface);
                objects
                    .insert(id.0, Surface {
                        inner: surface,
                        scratch_buffer,
                    })
                    .unwrap();
                let all_outputs = objects
                    .by_type::<super::Output>()
                    .map(|(_, obj)| obj.all_outputs.clone().unwrap())
                    .next();
                drop(objects); // unlock
                drop(shell);
                ctx.add_event_handler(rx, OutputChangedEventHandler {
                    current_outputs: Default::default(),
                    object_id: id.0,
                    shared_surface_buffers,
                    in_progress: false,
                    all_outputs,
                });
                Ok(())
            } else {
                Err(error::Error::IdExists(id.0))
            }
        }
    }

    fn create_region(
        ctx: &mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
    ) -> Self::CreateRegionFut<'_> {
        async { unimplemented!() }
    }
}

type AllOutputs = HashMap<WeakPtr<ShellOutput>, HashSet<u32>>;
struct OutputChangedEventHandler {
    /// Object id of the surface
    object_id:              u32,
    /// Current outputs that overlap with the surface
    current_outputs:        HashSet<WeakPtr<ShellOutput>>,
    /// A set of buffers shared between all surfaces for calculating output
    /// differences. To avoid allocating multiple buffers.
    shared_surface_buffers: Rc<RefCell<SharedSurfaceBuffers>>,
    /// All bound output objects of this client context. None if the client has
    /// no output objects bound.
    all_outputs:            Option<Rc<RefCell<AllOutputs>>>,
    /// Whether the messages have been generated and sending is in progress
    in_progress:            bool,
}

impl<Ctx: Client> EventHandler<Ctx> for OutputChangedEventHandler {
    type Message = OutputEvent;

    fn poll_handle_event(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        objects: &mut <Ctx as Client>::ObjectStore,
        connection: &mut <Ctx as Client>::Connection,
        server_context: &<Ctx as Client>::ServerContext,
        message: &mut Self::Message,
    ) -> std::task::Poll<
        Result<EventHandlerAction, Box<dyn std::error::Error + std::marker::Send + Sync + 'static>>,
    > {
        let Self {
            object_id,
            current_outputs,
            shared_surface_buffers,
            all_outputs,
            in_progress,
        } = self.get_mut();

        let mut connection = Pin::new(connection);
        let objects = objects.try_lock().unwrap();

        // Usually we would check if the object is still alive here, and stop the event
        // handler if it is not. But when a surface object dies, its event
        // stream will terminate, and the event handler will be stopped
        // automatically in that case.

        let mut buffers = shared_surface_buffers.borrow_mut();
        let SharedSurfaceBuffers {
            new_outputs,
            deleted: deleted_buffer,
            added: added_buffer,
        } = &mut *buffers;
        // Calculate the difference between the current outputs and the new outputs,
        // store message that will be sent to the client in the buffers. Do this
        // only once per message.
        if !*in_progress {
            let mut message = message.0.borrow_mut();
            new_outputs.extend(message.iter().cloned());
            message.clear();

            if all_outputs.is_none() {
                *all_outputs = objects
                    .by_type::<crate::objects::Output>()
                    .map(|(_, obj)| obj.all_outputs.clone().unwrap())
                    .next();
            }
            let Some(all_outputs) = all_outputs.as_ref() else {
                // This client has no bound output object, so just update the current outputs, and we
                // will be done.
                std::mem::swap(current_outputs, new_outputs);
                new_outputs.clear();
                return Poll::Ready(Ok(EventHandlerAction::Keep));
            };

            // Otherwise calculate the difference between the current outputs and the new
            // outputs
            let all_outputs = all_outputs.borrow();
            for deleted in current_outputs.difference(new_outputs) {
                if let Some(ids) = all_outputs.get(deleted) {
                    deleted_buffer.extend(ids.iter().copied());
                }
            }
            for added in new_outputs.difference(current_outputs) {
                if let Some(id) = all_outputs.get(added) {
                    added_buffer.extend(id.iter().copied());
                }
            }
            std::mem::swap(current_outputs, new_outputs);
            new_outputs.clear();
            *in_progress = true;
        }

        while let Some(id) = deleted_buffer.last() {
            let message = wl_surface::events::Leave {
                output: wl_types::Object(*id),
            };
            ready!(connection.as_mut().poll_reserve(cx, &message))?;
            connection.as_mut().start_send(*object_id, message);
            deleted_buffer.pop();
        }
        while let Some(id) = added_buffer.last() {
            let message = wl_surface::events::Enter {
                output: wl_types::Object(*id),
            };
            ready!(connection.as_mut().poll_reserve(cx, &message))?;
            connection.as_mut().start_send(*object_id, message);
            added_buffer.pop();
        }
        *in_progress = false;
        Poll::Ready(Ok(EventHandlerAction::Keep))
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Subsurface<S: Shell>(Rc<crate::shell::surface::Surface<S>>);

#[wayland_object]
impl<Ctx: Client, S: Shell> wl_subsurface::RequestDispatch<Ctx> for Subsurface<S>
where
    Ctx::ServerContext: HasShell<Shell = S>,
{
    type Error = error::Error;

    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type PlaceAboveFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type PlaceBelowFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetDesyncFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetPositionFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetSyncFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn set_sync(ctx: &mut Ctx, object_id: u32) -> Self::SetSyncFut<'_> {
        async move { unimplemented!() }
    }

    fn set_desync(ctx: &mut Ctx, object_id: u32) -> Self::SetDesyncFut<'_> {
        async move { unimplemented!() }
    }

    fn destroy(ctx: &mut Ctx, object_id: u32) -> Self::DestroyFut<'_> {
        async move { unimplemented!() }
    }

    fn place_above(
        ctx: &mut Ctx,
        object_id: u32,
        sibling: wl_types::Object,
    ) -> Self::PlaceAboveFut<'_> {
        async move { unimplemented!() }
    }

    fn place_below(
        ctx: &mut Ctx,
        object_id: u32,
        sibling: wl_types::Object,
    ) -> Self::PlaceBelowFut<'_> {
        async move { unimplemented!() }
    }

    fn set_position(ctx: &mut Ctx, object_id: u32, x: i32, y: i32) -> Self::SetPositionFut<'_> {
        async move {
            let objects = ctx.objects().lock().await;
            let mut shell = ctx.server_context().shell().borrow_mut();
            let this = objects.get(object_id).unwrap().cast::<Self>().unwrap();
            let role = this
                .0
                .role::<roles::Subsurface<<Ctx::ServerContext as HasShell>::Shell>>()
                .unwrap();
            let parent = role.parent().upgrade().unwrap().pending_mut(&mut shell);
            parent
                .stack_mut()
                .get_mut(role.stack_index)
                .unwrap()
                .position = Point::new(x, y);
            Ok(())
        }
    }
}

#[derive(Debug)]
pub struct Subcompositor;

#[derive(Debug)]
pub enum Error {
    BadSurface {
        bad_surface:       u32,
        subsurface_object: u32,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::BadSurface { bad_surface, .. } => write!(f, "Bad surface id {bad_surface}"),
        }
    }
}

impl std::error::Error for Error {}

impl wl_protocol::ProtocolError for Error {
    fn wayland_error(&self) -> Option<(u32, u32)> {
        match self {
            Error::BadSurface {
                bad_surface,
                subsurface_object,
            } => Some((
                *subsurface_object,
                wl_subcompositor::enums::Error::BadSurface as u32,
            )),
        }
    }

    fn fatal(&self) -> bool {
        true
    }
}

#[wayland_object]
impl<Ctx: Client> wl_subcompositor::RequestDispatch<Ctx> for Subcompositor
where
    Ctx::ServerContext: HasShell,
    Ctx::Object: From<Subsurface<<Ctx::ServerContext as HasShell>::Shell>>,
{
    type Error = wl_server::error::Error;

    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetSubsurfaceFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn destroy(ctx: &mut Ctx, object_id: u32) -> Self::DestroyFut<'_> {
        async move { unimplemented!() }
    }

    fn get_subsurface(
        ctx: &mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
        surface: wl_types::Object,
        parent: wl_types::Object,
    ) -> Self::GetSubsurfaceFut<'_> {
        tracing::debug!(
            "get_subsurface, id: {:?}, surface: {:?}, parent: {:?}",
            id,
            surface,
            parent
        );
        async move {
            let shell = ctx.server_context().shell();
            let objects = ctx.objects();
            let mut objects = objects.lock().await;
            let mut shell = shell.borrow_mut();
            let surface_id = surface.0;
            let surface = objects
                .get(surface.0)
                .and_then(|r| {
                    r.cast()
                        .map(|sur: &Surface<ShellOf<Ctx::ServerContext>>| sur.inner.clone())
                })
                .ok_or_else(|| {
                    Self::Error::custom(Error::BadSurface {
                        bad_surface:       surface.0,
                        subsurface_object: object_id,
                    })
                })?;
            let parent = objects
                .get(parent.0)
                .and_then(|r| {
                    r.cast()
                        .map(|sur: &Surface<ShellOf<Ctx::ServerContext>>| sur.inner.clone())
                })
                .ok_or_else(|| {
                    Self::Error::custom(Error::BadSurface {
                        bad_surface:       parent.0,
                        subsurface_object: object_id,
                    })
                })?;
            if objects.get(id.0).is_none() {
                if !crate::shell::surface::roles::Subsurface::attach(
                    parent,
                    surface.clone(),
                    &mut shell,
                ) {
                    Err(Self::Error::custom(Error::BadSurface {
                        bad_surface:       surface_id,
                        subsurface_object: object_id,
                    }))
                } else {
                    objects.insert(id.0, Subsurface(surface)).unwrap();
                    Ok(())
                }
            } else {
                Err(Self::Error::IdExists(id.0))
            }
        }
    }
}
