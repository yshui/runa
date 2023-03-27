//! Traits and type related to wayland globals

use std::{future::Future, pin::Pin};

use runa_io::traits::WriteMessage;
use runa_wayland_protocols::wayland::{wl_display, wl_registry::v1 as wl_registry};

use crate::{
    client::traits::{
        Client, ClientParts, EventDispatcher, EventHandler, EventHandlerAction, StoreUpdate,
    },
    events::EventSource,
    objects::DISPLAY_ID,
    server::traits::{GlobalStore, GlobalsUpdate, Server},
};

/// A monomorphic global
///
/// i.e. a global with a concrete type. This is analogous to the
/// [`MonoObject`](crate::objects::MonoObject) trait for objects.
pub trait MonoGlobal: Sized {
    /// Type of the proxy object for this global.
    type Object;

    /// The interface name of this global.
    const INTERFACE: &'static str;
    /// The version number of this global.
    const VERSION: u32;
    /// A value that can be used to create an instance of `Self`. Or None if
    /// there is no compile time default value.
    const MAYBE_DEFAULT: Option<Self>;
    /// Create a new proxy object of this global.
    fn new_object() -> Self::Object;
}

/// Bind a global to a client
///
/// TODO: authorization support, i.e. some globals should only be accessible to
/// authorized clients.
pub trait Bind<Ctx> {
    /// Type of future returned by [`bind`](Self::bind).
    type BindFut<'a>: Future<Output = std::io::Result<()>> + 'a
    where
        Ctx: 'a,
        Self: 'a;

    /// Setup a proxy of this global, the proxy object has already been inserted
    /// into the client's object store, with the given object id.
    ///
    /// If Err() is returned, an protocol error will be sent to the client, and
    /// the client will be disconnected.
    fn bind<'a>(&'a self, client: &'a mut Ctx, object_id: u32) -> Self::BindFut<'a>;
}

/// A polymorphic global
///
/// This is analogous to the [`AnyObject`](crate::objects::AnyObject) trait for
/// objects, as well as the [`Any`](std::any::Any) trait.
///
/// Typically a polymorphic global type will be a enum of all the global types
/// used in the compositor. If you have such an enum, the
/// [`globals!`](crate::globals!) macro will generate this trait impl for you.
///
/// Unlike `AnyObject`, this trait doesn't have a `cast_mut` method because
/// globals are shared so it's not possible to get a unique reference to them.
pub trait AnyGlobal {
    /// Type of the proxy object for this global. Since this global is
    /// polymorphic, the type of the proxy object is also polymorphic. This
    /// should match the object type you have in you
    /// [`Client`](crate::client::traits::Client) trait implementation.
    type Object: crate::objects::AnyObject;

    /// Create a proxy of this global, called when the client tries to bound
    /// the global. The new proxy object will be incomplete until [`Bind::bind`]
    /// is called with that object.
    fn new_object(&self) -> Self::Object;

    /// The interface name of this global.
    fn interface(&self) -> &'static str;

    /// The version number of this global.
    fn version(&self) -> u32;

    /// Cast this polymorphic global to a more concrete type.
    fn cast<T: 'static>(&self) -> Option<&T>
    where
        Self: 'static + Sized;
}

/// Implementation of the `wl_display` global
#[derive(Debug, Clone, Copy, Default)]
pub struct Display;

impl MonoGlobal for Display {
    type Object = crate::objects::Display;

    const INTERFACE: &'static str = wl_display::v1::NAME;
    const MAYBE_DEFAULT: Option<Self> = Some(Self);
    const VERSION: u32 = wl_display::v1::VERSION;

    #[inline]
    fn new_object() -> Self::Object {
        crate::objects::Display { initialized: false }
    }
}

impl<Ctx: Client> Bind<Ctx> for Display {
    type BindFut<'a > = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a, Ctx: 'a;

    fn bind<'a>(&'a self, client: &'a mut Ctx, object_id: u32) -> Self::BindFut<'a> {
        use crate::client::traits::Store;
        let ClientParts {
            objects,
            event_dispatcher,
            ..
        } = client.as_mut_parts();
        objects
            .get_mut::<<Self as MonoGlobal>::Object>(object_id)
            .unwrap()
            .initialized = true;

        struct DisplayEventHandler;
        impl<Ctx: Client> EventHandler<Ctx> for DisplayEventHandler {
            type Message = StoreUpdate;

            type Future<'ctx> = impl Future<
                    Output = Result<
                        EventHandlerAction,
                        Box<dyn std::error::Error + std::marker::Send + Sync + 'static>,
                    >,
                > + 'ctx;

            fn handle_event<'ctx>(
                &'ctx mut self,
                _objects: &'ctx mut <Ctx as Client>::ObjectStore,
                connection: &'ctx mut <Ctx as Client>::Connection,
                _server_context: &'ctx <Ctx as Client>::ServerContext,
                message: &'ctx mut Self::Message,
            ) -> Self::Future<'ctx> {
                async move {
                    use crate::client::traits::StoreUpdateKind::*;
                    if matches!(message.kind, Removed { .. } | Replaced { .. }) {
                        // Replaced can really only happen for server created objects, because
                        // we haven't sent the DeleteId event yet, so the client can't have
                        // reused that ID.
                        // For server create objects, it possible that the client destroyed
                        // that object and we immediately created another one with that ID,
                        // this should be the only case for us to get a Replaced event.
                        connection
                            .send(DISPLAY_ID, wl_display::v1::events::DeleteId {
                                id: message.object_id,
                            })
                            .await?;
                    }
                    Ok(EventHandlerAction::Keep)
                }
            }
        }
        event_dispatcher.add_event_handler(objects.subscribe(), DisplayEventHandler);
        futures_util::future::ok(())
    }
}

/// The registry singleton. This is an interface only object. The actual list of
/// globals is stored in the `Globals` implementation of the server context.
///
/// If you use this as your registry global implementation, you must also use
/// [`crate::objects::Registry`] as your client side registry proxy
/// implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct Registry;

impl MonoGlobal for Registry {
    type Object = crate::objects::Registry;

    const INTERFACE: &'static str = wl_registry::NAME;
    const MAYBE_DEFAULT: Option<Self> = Some(Self);
    const VERSION: u32 = wl_registry::VERSION;

    #[inline]
    fn new_object() -> Self::Object {
        crate::objects::Registry(None)
    }
}

impl<Ctx: Client> Bind<Ctx> for Registry {
    type BindFut<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a, Ctx: 'a;

    fn bind<'a>(&'a self, client: &'a mut Ctx, object_id: u32) -> Self::BindFut<'a> {
        async move {
            let ClientParts {
                server_context,
                connection,
                event_dispatcher,
                ..
            } = client.as_mut_parts();
            let rx = server_context.globals().borrow().subscribe();
            // Send existing globals
            let globals: Vec<_> = server_context
                .globals()
                .borrow()
                .iter()
                .map(|(id, global)| (id, global.clone()))
                .collect();

            for (id, global) in globals {
                let interface = global.interface();
                let version = global.version();
                connection
                    .send(object_id, wl_registry::events::Global {
                        name: id,
                        interface: interface.as_bytes().into(),
                        version,
                    })
                    .await
                    .unwrap()
            }
            connection.flush().await.unwrap();
            event_dispatcher.add_event_handler(rx, RegistryEventHandler {
                registry_id: object_id,
            });
            Ok(())
        }
    }
}

struct RegistryEventHandler {
    registry_id: u32,
}

impl<Ctx: Client> EventHandler<Ctx> for RegistryEventHandler {
    type Message = GlobalsUpdate<<Ctx::ServerContext as Server>::Global>;

    type Future<'ctx> = impl Future<
            Output = Result<
                EventHandlerAction,
                Box<dyn std::error::Error + std::marker::Send + Sync + 'static>,
            >,
        > + 'ctx;

    fn handle_event<'ctx>(
        &'ctx mut self,
        _objects: &'ctx mut Ctx::ObjectStore,
        connection: &'ctx mut Ctx::Connection,
        _server_context: &'ctx Ctx::ServerContext,
        message: &'ctx mut Self::Message,
    ) -> Self::Future<'ctx> {
        async move {
            let mut connection = Pin::new(connection);
            match message {
                GlobalsUpdate::Removed(name) => {
                    connection
                        .send(self.registry_id, wl_registry::events::GlobalRemove {
                            name: *name,
                        })
                        .await?;
                },
                GlobalsUpdate::Added(name, global) => {
                    let interface = global.interface();
                    let version = global.version();
                    connection
                        .send(self.registry_id, wl_registry::events::Global {
                            name: *name,
                            interface: interface.as_bytes().into(),
                            version,
                        })
                        .await?;
                },
            }

            // Client can't drop the registry object, so we will never stop this listener
            Ok(EventHandlerAction::Keep)
        }
    }
}
