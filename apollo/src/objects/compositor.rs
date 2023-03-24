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
use std::{future::Future, pin::Pin, rc::Rc};

use derivative::Derivative;
use hashbrown::HashSet;
use wl_io::traits::WriteMessage;
use wl_protocol::wayland::{
    wl_compositor::v6 as wl_compositor, wl_output::v4 as wl_output,
    wl_subcompositor::v1 as wl_subcompositor, wl_subsurface::v1 as wl_subsurface,
    wl_surface::v6 as wl_surface,
};
use wl_server::{
    connection::traits::{
        Client, ClientParts, EventDispatcher, EventHandler, EventHandlerAction, Store,
    },
    error,
    events::{broadcast::Ring, EventSource},
    objects::wayland_object,
};

use crate::{
    shell::{
        self,
        buffers::HasBuffer,
        output::Output as ShellOutput,
        surface::{roles, KeyboardEvent, OutputEvent, PointerEvent},
        HasShell, Shell,
    },
    utils::{geometry::Point, WeakPtr},
};

#[derive(Debug)]
pub struct SurfaceState<Token> {
    /// A queue of surface state tokens used for surface commits and
    /// destructions.
    scratch_buffer:  Vec<Token>,
    /// A buffer used for handling output changed event for surfaces.
    new_outputs:     HashSet<WeakPtr<ShellOutput>>,
    /// Shared event source for sending pointer events
    pointer_events:  Ring<PointerEvent>,
    /// Shared event source for sending keyboard events
    keyboard_events: Ring<KeyboardEvent>,
}

impl<Token> EventSource<PointerEvent> for SurfaceState<Token> {
    type Source = <Ring<PointerEvent> as EventSource<PointerEvent>>::Source;

    fn subscribe(&self) -> Self::Source {
        self.pointer_events.subscribe()
    }
}

impl<Token> EventSource<KeyboardEvent> for SurfaceState<Token> {
    type Source = <Ring<KeyboardEvent> as EventSource<KeyboardEvent>>::Source;

    fn subscribe(&self) -> Self::Source {
        self.keyboard_events.subscribe()
    }
}

//impl<S: Shell> EventSource<KeyboardEvent> for Surface<S> {
//    type Source = <broadcast::Broadcast<KeyboardEvent> as
// EventSource<KeyboardEvent>>::Source;
//
//    fn subscribe(&self) -> Self::Source {
//        self.keyboard_event.subscribe()
//    }
//}

impl<Token> SurfaceState<Token> {
    pub fn surface_count(&self) -> usize {
        self.pointer_events.sender_count() - 1
    }
}

impl<Token> Default for SurfaceState<Token> {
    fn default() -> Self {
        Self {
            scratch_buffer:  Default::default(),
            new_outputs:     Default::default(),
            pointer_events:  Ring::new(120),
            keyboard_events: Ring::new(120),
        }
    }
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Surface<Shell: shell::Shell> {
    pub(crate) inner: Rc<crate::shell::surface::Surface<Shell>>,
}

fn deallocate_surface<S: Shell, ServerCtx: HasShell<Shell = S>>(
    this: &mut Surface<S>,
    server_context: &mut ServerCtx,
    state: &mut SurfaceState<S::Token>,
) {
    let mut shell = server_context.shell().borrow_mut();
    this.inner.destroy(&mut shell, &mut state.scratch_buffer);

    // Disconnected, no point sending anything for those callbacks
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
            InvalidScale => write!(f, "buffer scale value is invalid for surface"),
            InvalidTransform => {
                write!(f, "buffer transform value is invalid for surface")
            },
            InvalidOffset => write!(f, "buffer offset is invalid"),
            InvalidSize => write!(f, "buffer size is invalid"),
            DefunctRoleObject => write!(f, "surface was destroyed before its role object"),
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

#[wayland_object(on_disconnect = "deallocate_surface", state = "SurfaceState<S::Token>")]
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
            let ClientParts {
                objects,
                server_context,
                ..
            } = ctx.as_mut_parts();
            let surface = objects.get::<Self>(object_id).unwrap().inner.clone();
            let inserted = objects
                .try_insert_with(callback.0, || {
                    let mut shell = server_context.shell().borrow_mut();
                    let state = surface.pending_mut(&mut shell);
                    state.add_frame_callback(callback.0);
                    wl_server::objects::Callback::default().into()
                })
                .is_some();
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
            let this = ctx.objects().get::<Self>(object_id).unwrap();
            let buffer_id = buffer.0;
            let buffer = ctx.objects().get::<crate::objects::Buffer<
                <Ctx::ServerContext as HasBuffer>::Buffer,
            >>(buffer.0)?;
            let mut shell = ctx.server_context().shell().borrow_mut();
            let state = this.inner.pending_mut(&mut shell);
            state.set_buffer_from_object(buffer);
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
            let this = ctx.objects().get::<Self>(object_id).unwrap();
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
            let this = ctx.objects().get::<Self>(object_id).unwrap();
            let mut shell = ctx.server_context().shell().borrow_mut();
            let state = this.inner.pending_mut(&mut shell);
            state.damage_buffer();
            Ok(())
        }
    }

    fn commit(ctx: &mut Ctx, object_id: u32) -> Self::CommitFut<'_> {
        async move {
            let ClientParts {
                objects,
                server_context,
                ..
            } = ctx.as_mut_parts();
            let (this, state) = objects.get_with_state_mut::<Self>(object_id).unwrap();

            let mut shell = server_context.shell().borrow_mut();
            this.inner
                .commit(&mut shell, &mut state.scratch_buffer)
                .map_err(wl_server::error::Error::UnknownFatalError)?;

            Ok(())
        }
    }

    fn set_buffer_scale(ctx: &mut Ctx, object_id: u32, scale: i32) -> Self::SetBufferScaleFut<'_> {
        async move {
            let this = ctx.objects().get::<Self>(object_id).unwrap();
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
            let ClientParts {
                server_context,
                objects,
                connection,
                ..
            } = ctx.as_mut_parts();
            let (this, state) = objects.get_with_state_mut::<Self>(object_id).unwrap();

            if this.inner.role_is_active() {
                return Err(error::Error::custom(SurfaceError {
                    object_id,
                    kind: wl_surface::enums::Error::DefunctRoleObject,
                }))
            }

            this.inner.destroy(
                &mut server_context.shell().borrow_mut(),
                &mut state.scratch_buffer,
            );

            let mut frame_callbacks =
                std::mem::take(&mut *this.inner.frame_callbacks().borrow_mut());
            for frame_callback in frame_callbacks.drain(..) {
                objects.remove(frame_callback).unwrap();
            }

            objects.remove(object_id).unwrap();
            Ok(())
        }
    }
}

/// The reference implementation of wl_compositor
#[derive(Debug, Default, Clone)]
pub struct Compositor;

#[derive(Default, Debug)]
pub struct CompositorState {
    pub(crate) render_event_handler_started: bool,
}

#[wayland_object(state = "CompositorState")]
impl<Ctx: Client, Sh: Shell> wl_compositor::RequestDispatch<Ctx> for Compositor
where
    Ctx::ServerContext: HasShell<Shell = Sh>,
    Ctx::Object: From<Surface<Sh>>,
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
            let ClientParts {
                server_context,
                objects,
                event_dispatcher,
                ..
            } = ctx.as_mut_parts();
            let create_surface_object = |surface_state: &mut SurfaceState<Sh::Token>| {
                let shell = server_context.shell();
                let mut shell = shell.borrow_mut();
                let surface = Rc::new(surface::Surface::new(
                    id,
                    surface_state.pointer_events.clone(),
                    surface_state.keyboard_events.clone(),
                ));
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

                tracing::debug!("id {} is surface {:p}", id.0, surface);
                let rx = <_ as EventSource<OutputEvent>>::subscribe(&*surface);

                event_dispatcher.add_event_handler(rx, OutputChangedEventHandler {
                    current_outputs: Default::default(),
                    object_id:       id.0,
                });
                Surface { inner: surface }
            };
            if objects
                .try_insert_with_state(id.0, create_surface_object)
                .is_some()
            {
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

struct OutputChangedEventHandler {
    /// Object id of the surface
    object_id:       u32,
    /// Current outputs that overlap with the surface
    current_outputs: HashSet<WeakPtr<ShellOutput>>,
}

impl<S: Shell, Ctx: Client> EventHandler<Ctx> for OutputChangedEventHandler
where
    Ctx::ServerContext: HasShell<Shell = S>,
{
    type Message = OutputEvent;

    type Future<'ctx> = impl Future<
            Output = Result<
                EventHandlerAction,
                Box<dyn std::error::Error + std::marker::Send + Sync + 'static>,
            >,
        > + 'ctx;

    fn handle_event<'ctx>(
        &'ctx mut self,
        objects: &'ctx mut <Ctx as Client>::ObjectStore,
        connection: &'ctx mut <Ctx as Client>::Connection,
        server_context: &'ctx <Ctx as Client>::ServerContext,
        message: &'ctx mut Self::Message,
    ) -> Self::Future<'ctx> {
        async move {
            let Self {
                object_id,
                current_outputs,
            } = self;

            let mut connection = Pin::new(connection);

            let surface_state = objects.get_state_mut::<Surface<S>>().unwrap();

            {
                let message = message.0.borrow();
                surface_state.new_outputs.clone_from(&message);
            }

            // Usually we would check if the object is still alive here, and stop the event
            // handler if it is not. But when a surface object dies, its event
            // stream will terminate, and the event handler will be stopped
            // automatically in that case.

            let Some(output_state) = objects.get_state::<crate::objects::Output>() else {
                // This client has no bound output object, so just update the current outputs, and we
                // will be done.
                let surface_state = objects.get_state_mut::<Surface<S>>().unwrap();
                std::mem::swap(current_outputs, &mut surface_state.new_outputs);
                return Ok(EventHandlerAction::Keep);
            };

            // Otherwise calculate the difference between the current outputs and the new
            // outputs
            let surface_state = objects.get_state::<Surface<S>>().unwrap();
            let new_outputs = &surface_state.new_outputs;
            let all_outputs = &output_state.all_outputs;
            for deleted in current_outputs.difference(new_outputs) {
                if let Some(ids) = all_outputs.get(deleted) {
                    for id in ids {
                        connection
                            .as_mut()
                            .send(*object_id, wl_surface::events::Leave {
                                output: wl_types::Object(*id),
                            })
                            .await?;
                    }
                }
            }
            for added in new_outputs.difference(current_outputs) {
                if let Some(ids) = all_outputs.get(added) {
                    for id in ids {
                        connection
                            .as_mut()
                            .send(*object_id, wl_surface::events::Enter {
                                output: wl_types::Object(*id),
                            })
                            .await?;
                    }
                }
            }

            let surface_state = objects.get_state_mut::<Surface<S>>().unwrap();
            std::mem::swap(current_outputs, &mut surface_state.new_outputs);
            Ok(EventHandlerAction::Keep)
        }
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
            let surface = ctx.objects().get::<Self>(object_id).unwrap().0.clone();
            let mut shell = ctx.server_context().shell().borrow_mut();
            let role = surface
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
impl<S: Shell, Ctx: Client> wl_subcompositor::RequestDispatch<Ctx> for Subcompositor
where
    Ctx::ServerContext: HasShell<Shell = S>,
    Ctx::Object: From<Subsurface<S>>,
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
            let surface_id = surface.0;
            let surface = ctx
                .objects()
                .get::<Surface<S>>(surface.0)
                .map_err(|e| {
                    Self::Error::custom(Error::BadSurface {
                        bad_surface:       surface.0,
                        subsurface_object: object_id,
                    })
                })?
                .inner
                .clone();
            let parent = ctx
                .objects()
                .get::<Surface<S>>(parent.0)
                .map_err(|e| {
                    Self::Error::custom(Error::BadSurface {
                        bad_surface:       parent.0,
                        subsurface_object: object_id,
                    })
                })?
                .inner
                .clone();
            if !ctx.objects().contains(id.0) {
                let shell = ctx.server_context().shell();
                let mut shell = shell.borrow_mut();
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
                    drop(shell);
                    ctx.objects_mut().insert(id.0, Subsurface(surface)).unwrap();
                    Ok(())
                }
            } else {
                Err(Self::Error::IdExists(id.0))
            }
        }
    }
}
