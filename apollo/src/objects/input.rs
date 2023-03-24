use std::future::Future;

use ordered_float::NotNan;
use runa_core::{
    connection::traits::{
        Client, ClientParts, EventDispatcher, EventHandler, EventHandlerAction, Store,
    },
    error::{Error, ProtocolError},
    events::{broadcast::Receiver, EventSource},
    objects::wayland_object,
};
use runa_io::traits::WriteMessage;
use runa_wayland_types::{Fixed, NewId, Object as WaylandObject};
use runa_wayland_protocols::wayland::{
    wl_keyboard::v9 as wl_keyboard, wl_pointer::v9 as wl_pointer, wl_seat::v9 as wl_seat,
    wl_surface::v6 as wl_surface,
};

use crate::{
    objects::compositor,
    shell::{
        surface::{KeyboardActivity, KeyboardEvent, PointerEvent},
        HasShell, Shell,
    },
    utils::{
        geometry::{coords, Point},
        stream::{self, Replace, Replaceable},
    },
};
#[derive(Default, Debug)]
pub struct Seat;

#[derive(thiserror::Error, Debug)]
pub enum SeatError {
    #[error("Seat does not have the capability {1:?}")]
    MissingCapability(u32, wl_seat::enums::Capability),
}

impl ProtocolError for SeatError {
    fn fatal(&self) -> bool {
        true
    }

    fn wayland_error(&self) -> Option<(u32, u32)> {
        match self {
            Self::MissingCapability(object_id, _) =>
                Some((*object_id, wl_seat::enums::Error::MissingCapability as u32)),
        }
    }
}

/// Macro for create a event handler that handles StoreEvent, and when it
/// detects the first surface object is inserted into the store, it will
/// subscribe to `$event` from the surface, and register the event source in
/// `$receiver`'s singleton state.
///
/// The defined type will be named `NewSurfaceHandler`.
macro_rules! def_new_surface_handler {
    ($event:ty, $receiver:ty) => {
        struct NewSurfaceHandler;
        impl<Ctx, Sh, Server> runa_core::connection::traits::EventHandler<Ctx> for NewSurfaceHandler
        where
            Ctx: Client<ServerContext = Server>,
            Sh: Shell,
            Server: HasShell<Shell = Sh>,
        {
            type Message = runa_core::connection::traits::StoreEvent;

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
                use runa_core::connection::traits::{GetError, StoreEventKind};
                let Some(receiver_state) = objects.get_state::<$receiver>() else {
                                        return futures_util::future::ok(EventHandlerAction::Stop)
                                    };
                match message.kind {
                    StoreEventKind::Inserted {
                        interface: wl_surface::NAME,
                    } |
                    StoreEventKind::Replaced {
                        new_interface: wl_surface::NAME,
                        ..
                    } => {
                        // Check what's been inserted is indeed a Surface object
                        use crate::objects::compositor::Surface;
                        let surface_state =
                            match objects.get_with_state::<Surface<Sh>>(message.object_id) {
                                Ok((_, state)) => state,
                                Err(GetError::IdNotFound(_)) => unreachable!(),
                                Err(GetError::TypeMismatch(_)) =>
                                    return futures_util::future::ok(EventHandlerAction::Keep),
                            };

                        if surface_state.surface_count() == 1 {
                            // This is the first surface object inserted, setup event
                            // listeners
                            tracing::debug!(
                                "First surface created, setting up event listeners for {}", stringify!($event)
                            );
                            let rx = <_ as EventSource<$event>>::subscribe(surface_state);
                            receiver_state.handle.replace(Some(rx));
                        }
                    },
                    _ => (),
                }
                futures_util::future::ok(EventHandlerAction::Keep)
            }
        }
    };
}

#[wayland_object]
impl<Server, Sh, Ctx> wl_seat::RequestDispatch<Ctx> for Seat
where
    Server: crate::shell::Seat + HasShell<Shell = Sh>,
    Sh: Shell,
    Ctx: Client<ServerContext = Server>,
    <Ctx as Client>::Object: From<Pointer> + From<Keyboard>,
{
    type Error = Error;

    type GetKeyboardFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetPointerFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type GetTouchFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type ReleaseFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn release(ctx: &mut Ctx, object_id: u32) -> Self::ReleaseFut<'_> {
        async move { Ok(()) }
    }

    fn get_pointer(ctx: &mut Ctx, object_id: u32, id: NewId) -> Self::GetPointerFut<'_> {
        async move {
            let cap = ctx.server_context().capabilities();
            if !cap.contains(wl_seat::enums::Capability::POINTER) {
                return Err(Error::custom(SeatError::MissingCapability(
                    object_id,
                    wl_seat::enums::Capability::POINTER,
                )))
            }

            def_new_surface_handler!(PointerEvent, Pointer);

            let ClientParts {
                objects,
                event_dispatcher,
                ..
            } = ctx.as_mut_parts();

            // Subscribe to pointer events if there are surface objects in the store,
            // otherwise the NewSurfaceHandler will add one when the first
            // surface object is created.
            let pointer_rx = objects
                .get_state::<compositor::Surface<Sh>>()
                .map(<_ as EventSource<PointerEvent>>::subscribe);
            let (_, state) = objects
                .insert_with_state(id.0, Pointer)
                .map_err(|_| Error::IdExists(id.0))?;
            if let Some(event_source) = state.event_source.take() {
                // This is our first pointer object, setup listeners
                tracing::debug!("First pointer object, add event handler");
                state.handle.replace(pointer_rx);
                event_dispatcher
                    .add_event_handler(event_source, PointerEventHandler { focus: None });
                event_dispatcher.add_event_handler(objects.subscribe(), NewSurfaceHandler);
            }

            Ok(())
        }
    }

    fn get_touch(ctx: &mut Ctx, object_id: u32, id: NewId) -> Self::GetTouchFut<'_> {
        async move {
            let cap = ctx.server_context().capabilities();
            if !cap.contains(wl_seat::enums::Capability::TOUCH) {
                return Err(Error::custom(SeatError::MissingCapability(
                    object_id,
                    wl_seat::enums::Capability::TOUCH,
                )))
            }
            Ok(())
        }
    }

    fn get_keyboard(ctx: &mut Ctx, object_id: u32, id: NewId) -> Self::GetKeyboardFut<'_> {
        async move {
            // TODO: add keymap and repeat_info event support
            let cap = ctx.server_context().capabilities();
            if !cap.contains(wl_seat::enums::Capability::KEYBOARD) {
                return Err(Error::custom(SeatError::MissingCapability(
                    object_id,
                    wl_seat::enums::Capability::KEYBOARD,
                )))
            }
            def_new_surface_handler!(KeyboardEvent, Keyboard);

            let ClientParts {
                objects,
                event_dispatcher,
                server_context,
                connection,
                ..
            } = ctx.as_mut_parts();

            // Subscribe to keyboard events if there are surface objects in the store,
            // otherwise the NewSurfaceHandler will add one when the first
            // surface object is created.
            let keyboard_rx = objects
                .get_state::<compositor::Surface<Sh>>()
                .map(<_ as EventSource<KeyboardEvent>>::subscribe);
            let (_, state) = objects
                .insert_with_state(id.0, Keyboard)
                .map_err(|_| Error::IdExists(id.0))?;

            let repeat_info = server_context.repeat_info();
            let keymap = server_context.keymap();
            connection
                .send(id.0, wl_keyboard::events::Keymap {
                    format: keymap.format,
                    fd:     keymap.fd.try_clone().unwrap().into(),
                    size:   keymap.size,
                })
                .await?;
            connection
                .send(id.0, wl_keyboard::events::RepeatInfo {
                    rate:  repeat_info.rate,
                    delay: repeat_info.delay,
                })
                .await?;
            if let Some(event_source) = state.event_source.take() {
                // This is our first pointer object, setup listeners
                tracing::debug!("First keyboard object, add event handler");
                state.handle.replace(keyboard_rx);
                event_dispatcher
                    .add_event_handler(event_source, KeyboardEventHandler { focus: None });
                event_dispatcher.add_event_handler(objects.subscribe(), NewSurfaceHandler);
                event_dispatcher
                    .add_event_handler(server_context.subscribe(), KeyboardConfigHandler);
            }
            Ok(())
        }
    }
}

pub struct KeyboardState {
    /// Handle for starting/stopping listening to keyboard events. We use a
    /// Replace here because we need to start/stop the listener in another
    /// event handler, i.e. without access to the event_dispatcher. This is the
    /// same for [`PointerState`].
    handle:       Replace<Receiver<KeyboardEvent>>,
    /// Event source paired with the Replace in `handle`. This is only ever no
    /// None when the first Keyboard object is just inserted.
    /// [`Seat::get_keyboard`] will take this out and register it
    /// with the event dispatcher. Same for [`PointerState`].
    event_source: Option<Replaceable<Receiver<KeyboardEvent>>>,
}

impl Default for KeyboardState {
    fn default() -> Self {
        let (event_source, handle) = stream::replaceable();
        Self {
            handle,
            event_source: Some(event_source),
        }
    }
}

impl Drop for KeyboardState {
    fn drop(&mut self) {
        // Last keyboard object is dropped, stop listening for keyboard events
        self.handle.replace(None);
    }
}

#[derive(Debug, Default)]
pub struct Keyboard;

#[wayland_object(state = "KeyboardState")]
impl<Ctx: Client> wl_keyboard::RequestDispatch<Ctx> for Keyboard {
    type Error = Error;

    type ReleaseFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn release(ctx: &mut Ctx, object_id: u32) -> Self::ReleaseFut<'_> {
        async move {
            ctx.objects_mut().remove(object_id).unwrap();
            Ok(())
        }
    }
}

struct KeyboardConfigHandler;

impl<Ctx> EventHandler<Ctx> for KeyboardConfigHandler
where
    Ctx: Client,
{
    type Message = crate::shell::SeatEvent;

    type Future<'ctx> = impl Future<Output = Result<EventHandlerAction, Box<dyn std::error::Error + Send + Sync>>>
        + 'ctx;

    fn handle_event<'ctx>(
        &'ctx mut self,
        objects: &'ctx mut <Ctx as Client>::ObjectStore,
        connection: &'ctx mut <Ctx as Client>::Connection,
        server_context: &'ctx <Ctx as Client>::ServerContext,
        message: &'ctx mut Self::Message,
    ) -> Self::Future<'ctx> {
        use crate::shell::SeatEvent::*;
        async move {
            if objects.ids_by_type::<Keyboard>().next().is_none() {
                // No keyboard objects left, stop listening for seat events
                return Ok(EventHandlerAction::Stop)
            }
            match message {
                KeymapChanged(keymap) =>
                    for id in objects.ids_by_type::<Keyboard>() {
                        connection
                            .send(id, wl_keyboard::events::Keymap {
                                format: keymap.format,
                                fd:     keymap.fd.try_clone().unwrap().into(),
                                size:   keymap.size,
                            })
                            .await?;
                    },
                RepeatInfoChanged(repeat_info) => {
                    send_to_all_keyboards::<Ctx, _>(
                        objects,
                        connection,
                        wl_keyboard::events::RepeatInfo {
                            rate:  repeat_info.rate,
                            delay: repeat_info.delay,
                        },
                    )
                    .await?;
                },
            }
            Ok(EventHandlerAction::Keep)
        }
    }
}

struct KeyboardEventHandler {
    focus: Option<(u32, crate::shell::surface::KeyboardState)>,
}

async fn send_to_all_keyboards<Ctx, M>(
    objects: &mut <Ctx as Client>::ObjectStore,
    connection: &mut <Ctx as Client>::Connection,
    message: M,
) -> std::io::Result<()>
where
    Ctx: Client,
    M: runa_io::traits::ser::Serialize + Clone + Unpin + std::fmt::Debug,
{
    for id in objects.ids_by_type::<Keyboard>() {
        connection.send(id, message.clone()).await?;
    }
    Ok(())
}

impl<Ctx, Server, Sh> EventHandler<Ctx> for KeyboardEventHandler
where
    Ctx: Client<ServerContext = Server>,
    Server: HasShell<Shell = Sh>,
    Sh: Shell,
{
    type Message = KeyboardEvent;

    type Future<'ctx> = impl Future<Output = Result<EventHandlerAction, Box<dyn std::error::Error + Send + Sync>>> + 'ctx where Ctx: 'ctx;

    fn handle_event<'ctx>(
        &'ctx mut self,
        objects: &'ctx mut <Ctx as Client>::ObjectStore,
        connection: &'ctx mut <Ctx as Client>::Connection,
        server_context: &'ctx <Ctx as Client>::ServerContext,
        message: &'ctx mut Self::Message,
    ) -> Self::Future<'ctx> {
        async move {
            match &mut message.activity {
                KeyboardActivity::Leave =>
                    if let Some((id, _)) = self.focus.take() {
                        if id != message.object_id {
                            tracing::error!(
                                "Bug in the compositor: leaving a surface that's not been \
                                 previously focused"
                            );
                        } else {
                            send_to_all_keyboards::<Ctx, _>(
                                objects,
                                connection,
                                wl_keyboard::events::Leave {
                                    serial:  0,
                                    surface: WaylandObject(message.object_id),
                                },
                            )
                            .await?;
                        }
                    },
                KeyboardActivity::Key(state) => {
                    if self
                        .focus
                        .as_ref()
                        .map(|(id, _)| *id != message.object_id)
                        .unwrap_or(true)
                    {
                        // Focus changed, send leave event to old focus
                        if let Some((id, _)) = self.focus.take() {
                            send_to_all_keyboards::<Ctx, _>(
                                objects,
                                connection,
                                wl_keyboard::events::Leave {
                                    serial:  0,
                                    surface: WaylandObject(id),
                                },
                            )
                            .await?;
                        }
                        // Send enter event to new focus
                        send_to_all_keyboards::<Ctx, _>(
                            objects,
                            connection,
                            wl_keyboard::events::Enter {
                                serial:  0,
                                surface: WaylandObject(message.object_id),
                                keys:    &state.keys,
                            },
                        )
                        .await?;
                        send_to_all_keyboards::<Ctx, _>(
                            objects,
                            connection,
                            wl_keyboard::events::Modifiers {
                                serial:         0,
                                mods_depressed: state.depressed_modifiers,
                                mods_latched:   state.latched_modifiers,
                                mods_locked:    state.locked_modifiers,
                                group:          state.effective_layout,
                            },
                        )
                        .await?;
                        self.focus = Some((message.object_id, std::mem::take(state)));
                    } else {
                        let time = message.time;
                        let old_state = self.focus.as_mut().map(|(_, state)| state).unwrap();
                        for new_key in &state.keys {
                            if !old_state.keys.contains(new_key) {
                                send_to_all_keyboards::<Ctx, _>(
                                    objects,
                                    connection,
                                    wl_keyboard::events::Key {
                                        serial: 0,
                                        time,
                                        key: *new_key as u32,
                                        state: wl_keyboard::enums::KeyState::Pressed,
                                    },
                                )
                                .await?;
                            }
                        }
                        for old_key in &old_state.keys {
                            if !state.keys.contains(old_key) {
                                send_to_all_keyboards::<Ctx, _>(
                                    objects,
                                    connection,
                                    wl_keyboard::events::Key {
                                        serial: 0,
                                        time,
                                        key: *old_key as u32,
                                        state: wl_keyboard::enums::KeyState::Released,
                                    },
                                )
                                .await?;
                            }
                        }
                        if state.effective_layout != old_state.effective_layout ||
                            state.locked_modifiers != old_state.locked_modifiers ||
                            state.latched_modifiers != old_state.latched_modifiers ||
                            state.depressed_modifiers != old_state.depressed_modifiers
                        {
                            send_to_all_keyboards::<Ctx, _>(
                                objects,
                                connection,
                                wl_keyboard::events::Modifiers {
                                    serial:         0,
                                    mods_depressed: state.depressed_modifiers,
                                    mods_latched:   state.latched_modifiers,
                                    mods_locked:    state.locked_modifiers,
                                    group:          state.effective_layout,
                                },
                            )
                            .await?;
                        }
                        *old_state = std::mem::take(state);
                    }
                },
            }
            Ok(EventHandlerAction::Keep)
        }
    }
}

pub struct PointerState {
    handle:       Replace<Receiver<PointerEvent>>,
    event_source: Option<Replaceable<Receiver<PointerEvent>>>,
}

impl Default for PointerState {
    fn default() -> Self {
        let (event_source, handle) = stream::replaceable();
        Self {
            handle,
            event_source: Some(event_source),
        }
    }
}

impl Drop for PointerState {
    fn drop(&mut self) {
        // Stop listening for pointer events when the last pointer object is dropped
        self.handle.replace(None);
    }
}

#[derive(Debug)]
pub struct Pointer;

#[wayland_object(state = "PointerState")]
impl<Ctx> wl_pointer::RequestDispatch<Ctx> for Pointer
where
    Ctx: Client,
{
    type Error = runa_core::error::Error;

    type ReleaseFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SetCursorFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn release(ctx: &mut Ctx, object_id: u32) -> Self::ReleaseFut<'_> {
        async move {
            ctx.objects_mut().remove(object_id).unwrap();
            Ok(())
        }
    }

    fn set_cursor(
        ctx: &mut Ctx,
        object_id: u32,
        serial: u32,
        surface: WaylandObject,
        hotspot_x: i32,
        hotspot_y: i32,
    ) -> Self::SetCursorFut<'_> {
        async move {
            tracing::warn!("set_cursor unimplemented");
            Ok(())
        }
    }
}

#[derive(Default)]
struct PointerEventHandler {
    focus: Option<(u32, Point<NotNan<f32>, coords::Surface>)>,
}

async fn send_to_all_pointers<Ctx, M>(
    objects: &mut Ctx::ObjectStore,
    connection: &mut Ctx::Connection,
    event: M,
) -> std::io::Result<()>
where
    Ctx: Client,
    <Ctx as Client>::ServerContext: HasShell,
    M: runa_io::traits::ser::Serialize + Unpin + std::fmt::Debug + Copy,
{
    for object_id in objects.ids_by_type::<Pointer>() {
        connection.send(object_id, event).await?;
    }
    Ok(())
}

impl<Ctx, Server, Sh> EventHandler<Ctx> for PointerEventHandler
where
    Ctx: Client<ServerContext = Server>,
    Server: HasShell<Shell = Sh>,
    Sh: Shell,
{
    type Message = PointerEvent;

    type Future<'ctx> = impl Future<Output = Result<EventHandlerAction, Box<dyn std::error::Error + Send + Sync>>> + 'ctx where Ctx: 'ctx;

    fn handle_event<'ctx>(
        &'ctx mut self,
        objects: &'ctx mut <Ctx as Client>::ObjectStore,
        connection: &'ctx mut <Ctx as Client>::Connection,
        server_context: &'ctx <Ctx as Client>::ServerContext,
        message: &'ctx mut Self::Message,
    ) -> Self::Future<'ctx> {
        async move {
            use crate::shell::surface::PointerEventKind;
            if self
                .focus
                .map(|old| old.0 != message.object_id)
                .unwrap_or(true)
            {
                // focus changed
                if let Some((old, _)) = self.focus {
                    send_to_all_pointers::<Ctx, _>(
                        objects,
                        connection,
                        wl_pointer::events::Leave {
                            serial:  0,
                            surface: WaylandObject(old),
                        },
                    )
                    .await?;
                }
                self.focus = None;
                let PointerEventKind::Motion { coords, .. } = message.kind else {
                    tracing::error!("Bug in the compositor: first pointer event on a \
                                     surface is not a motion event, ignored. (event is {message:?})");
                    return Ok(EventHandlerAction::Keep);
                };
                self.focus = Some((message.object_id, coords));
                send_to_all_pointers::<Ctx, _>(objects, connection, wl_pointer::events::Enter {
                    serial:    0,
                    surface:   WaylandObject(message.object_id),
                    surface_x: Fixed::from_num(coords.x.into_inner()),
                    surface_y: Fixed::from_num(coords.y.into_inner()),
                })
                .await?;
            }
            let (_, old_coords) = self.focus.as_mut().unwrap();
            match message.kind {
                PointerEventKind::Motion { coords } =>
                    if coords != *old_coords {
                        send_to_all_pointers::<Ctx, _>(
                            objects,
                            connection,
                            wl_pointer::events::Motion {
                                time:      message.time,
                                surface_x: Fixed::from_num(coords.x.into_inner()),
                                surface_y: Fixed::from_num(coords.y.into_inner()),
                            },
                        )
                        .await?;
                        *old_coords = coords;
                    },
                PointerEventKind::Button { button, state } => {
                    send_to_all_pointers::<Ctx, _>(
                        objects,
                        connection,
                        wl_pointer::events::Button {
                            serial: 0,
                            time: message.time,
                            button,
                            state,
                        },
                    )
                    .await?;
                },
                PointerEventKind::Leave => {
                    send_to_all_pointers::<Ctx, _>(
                        objects,
                        connection,
                        wl_pointer::events::Leave {
                            serial:  0,
                            surface: WaylandObject(message.object_id),
                        },
                    )
                    .await?;
                    self.focus = None;
                },
            }
            Ok(EventHandlerAction::Keep)
        }
    }
}
