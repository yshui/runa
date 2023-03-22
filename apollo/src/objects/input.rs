use std::future::Future;

use ordered_float::NotNan;
use wl_io::traits::WriteMessage;
use wl_protocol::wayland::{
    wl_pointer::v8 as wl_pointer, wl_seat::v8 as wl_seat, wl_surface::v5 as wl_surface,
};
use wl_server::{
    connection::traits::{
        Client, ClientParts, EventDispatcher, EventHandler, EventHandlerAction, Store,
    },
    error::Error,
    events::{broadcast::Receiver, EventSource},
    objects::wayland_object,
};
use wl_types::NewId;

use crate::{
    objects::compositor,
    shell::{surface::PointerEvent, HasShell, Shell},
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

impl wl_protocol::ProtocolError for SeatError {
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

#[wayland_object]
impl<Server, Sh, Ctx> wl_seat::RequestDispatch<Ctx> for Seat
where
    Server: crate::shell::Seat + HasShell<Shell = Sh>,
    Sh: Shell,
    Ctx: Client<ServerContext = Server>,
    <Ctx as Client>::Object: From<Pointer>,
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
            struct NewSurfaceHandler;
            impl<Ctx, Sh, Server> wl_server::connection::traits::EventHandler<Ctx> for NewSurfaceHandler
            where
                Ctx: Client<ServerContext = Server>,
                Sh: Shell,
                Server: HasShell<Shell = Sh>,
            {
                type Message = wl_server::connection::traits::StoreEvent;

                type Future<'ctx> = impl Future<
                        Output = Result<
                            EventHandlerAction,
                            Box<dyn std::error::Error + Send + Sync>,
                        >,
                    > + 'ctx;

                fn handle_event<'ctx>(
                    &'ctx mut self,
                    objects: &'ctx mut Ctx::ObjectStore,
                    connection: &'ctx mut Ctx::Connection,
                    server_context: &'ctx Ctx::ServerContext,
                    message: &'ctx mut Self::Message,
                ) -> Self::Future<'ctx> {
                    use wl_server::connection::traits::{GetError, StoreEventKind};
                    let Some(pointer_state) = objects.get_state::<Pointer>() else {
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
                                    "First surface created, setting up pointer event listeners"
                                );
                                let rx = surface_state.subscribe();
                                pointer_state.handle.replace(Some(rx));
                            }
                        },
                        _ => (),
                    }
                    futures_util::future::ok(EventHandlerAction::Keep)
                }
            }

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
                .map(|surface_state| surface_state.subscribe());
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
        async move { unimplemented!() }
    }

    fn get_keyboard(ctx: &mut Ctx, object_id: u32, id: NewId) -> Self::GetKeyboardFut<'_> {
        async move { unimplemented!() }
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
    type Error = wl_server::error::Error;

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
        surface: wl_types::Object,
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

async fn send_to_all_pointers<
    Ctx,
    M: wl_io::traits::ser::Serialize + Unpin + std::fmt::Debug + Copy,
>(
    objects: &mut Ctx::ObjectStore,
    connection: &mut Ctx::Connection,
    event: M,
) -> std::io::Result<()>
where
    Ctx: Client,
    <Ctx as Client>::ServerContext: HasShell,
{
    for (object_id, _) in objects.by_type::<Pointer>() {
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
                            surface: wl_types::Object(old),
                        },
                    )
                    .await?;
                }
                let coords = message.kind.coords();
                self.focus = Some((message.object_id, coords));
                send_to_all_pointers::<Ctx, _>(objects, connection, wl_pointer::events::Enter {
                    serial:    0,
                    surface:   wl_types::Object(message.object_id),
                    surface_x: wl_types::Fixed::from_num(coords.x.into_inner()),
                    surface_y: wl_types::Fixed::from_num(coords.y.into_inner()),
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
                                surface_x: wl_types::Fixed::from_num(coords.x.into_inner()),
                                surface_y: wl_types::Fixed::from_num(coords.y.into_inner()),
                            },
                        )
                        .await?;
                        *old_coords = coords;
                    },
                PointerEventKind::Button {
                    button,
                    state,
                    coords,
                } => {
                    if coords != *old_coords {
                        tracing::warn!(
                            "PointerEventKind::Button events should not have moved the pointer \
                             (new != old: {:?} != {:?})",
                            coords,
                            *old_coords
                        );
                        *old_coords = coords;
                    }
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
            }
            Ok(EventHandlerAction::Keep)
        }
    }
}
