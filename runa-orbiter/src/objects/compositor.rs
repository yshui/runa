//! Types related to the compositor global
//!
//! The compositor global is responsible for managing the sets of surfaces a
//! client has. According to the wayland spec, each surface has a set of
//! double-buffered states: updates are made to the pending state first, and
//! applied to current state when `wl_surface.commit` is called.
use std::{future::Future, pin::Pin, rc::Rc};

use derive_where::derive_where;
use hashbrown::HashSet;
use runa_core::{
    client::traits::{
        Client, ClientParts, EventDispatcher, EventHandler, EventHandlerAction, Store,
    },
    error::{self, ProtocolError},
    events::EventSource,
    objects::wayland_object,
};
use runa_io::traits::WriteMessage;
use runa_wayland_protocols::wayland::{
    wl_compositor::v6 as wl_compositor, wl_output::v4 as wl_output, wl_region::v1 as wl_region,
    wl_subcompositor::v1 as wl_subcompositor, wl_subsurface::v1 as wl_subsurface,
    wl_surface::v6 as wl_surface,
};
use runa_wayland_types as wayland_types;

use crate::{
    shell::{
        self,
        buffers::HasBuffer,
        output::Output as ShellOutput,
        surface::{roles, OutputEvent},
        HasShell, Shell,
    },
    utils::{geometry::Point, WeakPtr},
};

mod states {
    use hashbrown::HashSet;
    use runa_core::events::{broadcast::Ring, EventSource};

    use crate::{
        shell::{
            output::Output as ShellOutput,
            surface::{KeyboardEvent, PointerEvent},
        },
        utils::WeakPtr,
    };

    #[derive(Default, Debug, Clone, Copy)]
    pub struct CompositorState {
        pub(crate) render_event_handler_started: bool,
    }

    #[derive(Debug)]
    pub struct SurfaceState<Token> {
        /// A queue of surface state tokens used for surface commits and
        /// destructions.
        pub(super) scratch_buffer:  Vec<Token>,
        /// A buffer used for handling output changed event for surfaces.
        pub(super) new_outputs:     HashSet<WeakPtr<ShellOutput>>,
        /// Shared event source for sending pointer events
        pub(super) pointer_events:  Ring<PointerEvent>,
        /// Shared event source for sending keyboard events
        pub(super) keyboard_events: Ring<KeyboardEvent>,
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

    impl<Token> SurfaceState<Token> {
        pub(crate) fn surface_count(&self) -> usize {
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
}

use states::*;

/// Implementation of the `wl_surface` interface.
#[derive_where(Debug)]
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

impl ProtocolError for SurfaceError {
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
    Ctx::Object: From<runa_core::objects::Callback>,
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

    fn frame(ctx: &mut Ctx, object_id: u32, callback: wayland_types::NewId) -> Self::FrameFut<'_> {
        async move {
            let ClientParts { objects, .. } = ctx.as_mut_parts();
            let surface = objects.get::<Self>(object_id).unwrap().inner.clone();
            let inserted = objects
                .try_insert_with(callback.0, || {
                    surface.add_frame_callback(callback.0);
                    runa_core::objects::Callback::default().into()
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
        buffer: wayland_types::Object,
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
            let buffer = ctx.objects().get::<crate::objects::Buffer<
                <Ctx::ServerContext as HasBuffer>::Buffer,
            >>(buffer.0)?;
            let mut state = this.inner.pending_mut();
            state.set_buffer_from_object(buffer);
            Ok(())
        }
    }

    fn damage(
        ctx: &mut Ctx,
        object_id: u32,
        _x: i32,
        _y: i32,
        _width: i32,
        _height: i32,
    ) -> Self::DamageFut<'_> {
        async move {
            // TODO: make use of the provided damage rectangle, instead of damage the whole
            // buffer
            let this = ctx.objects().get::<Self>(object_id).unwrap();
            let state = this.inner.pending();
            state.damage_buffer();
            Ok(())
        }
    }

    fn damage_buffer(
        ctx: &mut Ctx,
        object_id: u32,
        _x: i32,
        _y: i32,
        _width: i32,
        _height: i32,
    ) -> Self::DamageBufferFut<'_> {
        async move {
            // TODO: make use of the provided damage rectangle, instead of damage the whole
            // buffer
            let this = ctx.objects().get::<Self>(object_id).unwrap();
            let state = this.inner.pending();
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
                .map_err(error::Error::UnknownFatalError)?;

            Ok(())
        }
    }

    fn set_buffer_scale(ctx: &mut Ctx, object_id: u32, scale: i32) -> Self::SetBufferScaleFut<'_> {
        async move {
            let this = ctx.objects().get::<Self>(object_id).unwrap();
            let mut pending_mut = this.inner.pending_mut();
            pending_mut.set_buffer_scale(scale as u32 * 120);
            Ok(())
        }
    }

    fn set_input_region(
        _ctx: &mut Ctx,
        object_id: u32,
        region: wayland_types::Object,
    ) -> Self::SetInputRegionFut<'_> {
        async move {
            tracing::error!(object_id, ?region, "set_input_region not implemented");
            Ok(())
        }
    }

    fn set_opaque_region(
        _ctx: &mut Ctx,
        object_id: u32,
        region: wayland_types::Object,
    ) -> Self::SetOpaqueRegionFut<'_> {
        async move {
            tracing::error!(object_id, ?region, "set_opaque_region not implemented");
            Ok(())
        }
    }

    fn set_buffer_transform(
        _ctx: &mut Ctx,
        object_id: u32,
        transform: wl_output::enums::Transform,
    ) -> Self::SetBufferTransformFut<'_> {
        async move {
            tracing::error!(
                object_id,
                ?transform,
                "set_buffer_transform not implemented"
            );
            Err(error::Error::NotImplemented(
                "wl_surface.set_buffer_transform",
            ))
        }
    }

    fn offset(_ctx: &mut Ctx, object_id: u32, x: i32, y: i32) -> Self::OffsetFut<'_> {
        async move {
            tracing::error!(object_id, x, y, "offset not implemented");
            Err(error::Error::NotImplemented("wl_surface.offset"))
        }
    }

    fn destroy(ctx: &mut Ctx, object_id: u32) -> Self::DestroyFut<'_> {
        async move {
            let ClientParts {
                server_context,
                objects,
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

/// The reference implementation of the `wl_compositor` interface
#[derive(Debug, Default, Clone, Copy)]
pub struct Compositor;

#[wayland_object(state = "states::CompositorState")]
impl<Ctx: Client, Sh: Shell> wl_compositor::RequestDispatch<Ctx> for Compositor
where
    Ctx::ServerContext: HasShell<Shell = Sh>,
    Ctx::Object: From<Surface<Sh>> + From<Region>,
{
    type Error = error::Error;

    type CreateRegionFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type CreateSurfaceFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn create_surface(
        ctx: &mut Ctx,
        _object_id: u32,
        id: wayland_types::NewId,
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
                let surface = surface::Surface::new(
                    id,
                    &mut *shell,
                    surface_state.pointer_events.clone(),
                    surface_state.keyboard_events.clone(),
                );
                shell.post_commit(None, surface.current_key());

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
        id: wayland_types::NewId,
    ) -> Self::CreateRegionFut<'_> {
        async move {
            tracing::error!(object_id, ?id, "create_region semi-stub");
            let ClientParts {
                server_context,
                objects,
                event_dispatcher,
                ..
            } = ctx.as_mut_parts();

            objects
                .insert(id.0, Region)
                .map(|_| ())
                .map_err(|_| error::Error::IdExists(id.0))
        }
    }
}

#[derive(Debug)]
pub struct Region;

#[wayland_object]
impl<Ctx: Client> wl_region::RequestDispatch<Ctx> for Region
where
    Ctx::Object: From<Region>,
{
    type Error = error::Error;

    type AddFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;
    type SubtractFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn destroy(ctx: &mut Ctx, object_id: u32) -> Self::DestroyFut<'_> {
        async move {
            ctx.objects_mut().remove(object_id).unwrap();
            Ok(())
        }
    }

    fn subtract(
        _ctx: &mut Ctx,
        object_id: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Self::SubtractFut<'_> {
        async move {
            tracing::error!(object_id, x, y, width, height, "subtract not implemented");
            Ok(())
        }
    }

    fn add(
        _ctx: &mut Ctx,
        object_id: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Self::AddFut<'_> {
        async move {
            tracing::error!(object_id, x, y, width, height, "add not implemented");
            Ok(())
        }
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
        _server_context: &'ctx <Ctx as Client>::ServerContext,
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
                // This client has no bound output object, so just update the current outputs,
                // and we will be done.
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
                                output: wayland_types::Object(*id),
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
                                output: wayland_types::Object(*id),
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

/// Implementation of the `wl_subsurface` interface
#[derive_where(Debug)]
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

    fn set_sync(_ctx: &mut Ctx, object_id: u32) -> Self::SetSyncFut<'_> {
        async move {
            tracing::error!(object_id, "set_sync not implemented");
            Ok(())
        }
    }

    fn set_desync(_ctx: &mut Ctx, object_id: u32) -> Self::SetDesyncFut<'_> {
        async move {
            tracing::error!(object_id, "set_desync not implemented");
            Ok(())
        }
    }

    fn destroy(ctx: &mut Ctx, object_id: u32) -> Self::DestroyFut<'_> {
        async move {
            let subsurface = ctx.objects().get::<Self>(object_id).unwrap();
            let shell = ctx.server_context().shell();
            subsurface.0.deactivate_role(&mut shell.borrow_mut());
            ctx.objects_mut().remove(object_id).unwrap();
            Ok(())
        }
    }

    fn place_above(
        _ctx: &mut Ctx,
        object_id: u32,
        sibling: wayland_types::Object,
    ) -> Self::PlaceAboveFut<'_> {
        async move {
            tracing::error!(object_id, ?sibling, "place_above not implemented");
            Err(error::Error::NotImplemented("wl_subsurface.place_above"))
        }
    }

    fn place_below(
        _ctx: &mut Ctx,
        object_id: u32,
        sibling: wayland_types::Object,
    ) -> Self::PlaceBelowFut<'_> {
        async move {
            tracing::error!(object_id, ?sibling, "place_below not implemented");
            Err(error::Error::NotImplemented("wl_subsurface.place_below"))
        }
    }

    fn set_position(ctx: &mut Ctx, object_id: u32, x: i32, y: i32) -> Self::SetPositionFut<'_> {
        async move {
            let surface = ctx.objects().get::<Self>(object_id).unwrap().0.clone();
            let role = surface
                .role::<roles::Subsurface<<Ctx::ServerContext as HasShell>::Shell>>()
                .unwrap();
            let parent = role.parent().upgrade().unwrap();
            let mut parent_stack_pending = parent.pending_mut();
            parent_stack_pending
                .stack_mut()
                .get_mut(role.stack_index)
                .unwrap()
                .set_position(Point::new(x, y));
            Ok(())
        }
    }
}

/// Implementation of the `wl_subcompositor` interface
#[derive(Debug, Clone, Copy)]
pub struct Subcompositor;

/// Errors that can occur when handling subcompositor related requests
#[derive(Debug, Clone, Copy)]
enum Error {
    /// Bad surface used as parent
    BadSurface {
        /// The bad surface id
        bad_surface: u32,
        /// The ID of the object that caused this error
        object_id:   u32,
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

impl ProtocolError for Error {
    fn wayland_error(&self) -> Option<(u32, u32)> {
        match self {
            Error::BadSurface { object_id, .. } => Some((
                *object_id,
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
    type Error = error::Error;

    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetSubsurfaceFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn destroy(ctx: &mut Ctx, object_id: u32) -> Self::DestroyFut<'_> {
        async move {
            ctx.objects_mut().remove(object_id).unwrap();
            Ok(())
        }
    }

    fn get_subsurface(
        ctx: &mut Ctx,
        object_id: u32,
        id: wayland_types::NewId,
        surface: wayland_types::Object,
        parent: wayland_types::Object,
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
                .map_err(|_| {
                    Self::Error::custom(Error::BadSurface {
                        bad_surface: surface.0,
                        object_id,
                    })
                })?
                .inner
                .clone();
            let parent = ctx
                .objects()
                .get::<Surface<S>>(parent.0)
                .map_err(|_| {
                    Self::Error::custom(Error::BadSurface {
                        bad_surface: parent.0,
                        object_id,
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
                        bad_surface: surface_id,
                        object_id,
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
