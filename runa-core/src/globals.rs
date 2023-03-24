use std::{future::Future, pin::Pin};

use runa_io::traits::WriteMessage;
use runa_wayland_protocols::wayland::{wl_display, wl_registry::v1 as wl_registry};

use crate::{
    connection::traits::{Client, ClientParts, EventDispatcher, EventHandler, EventHandlerAction},
    events::EventSource,
    objects::DISPLAY_ID,
    server::{GlobalsUpdate, Server},
};

pub trait GlobalMeta {
    type Object;
    /// Create a proxy of this global, called when the client tries to bound
    /// the global.
    fn new_object(&self) -> Self::Object;
    fn interface(&self) -> &'static str;
    fn version(&self) -> u32;
}

pub trait Bind<Ctx>: GlobalMeta {
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

/// A value that can be initialized with a constant
pub trait MaybeConstInit: Sized {
    /// A value that can be used to create an instance of `Self`.
    const INIT: Option<Self>;
}

pub trait Global<Ctx>: MaybeConstInit + Bind<Ctx> {
    fn cast<T: 'static>(&self) -> Option<&T>
    where
        Self: 'static + Sized,
    {
        (self as &dyn std::any::Any).downcast_ref()
    }
}

/// Implement Global trait using the default implementation
/// This could be a blanket impl, but that would deprive the user of the ability
/// to have specialized impl for their own types. Not until we have
/// specialization in Rust...
#[macro_export]
macro_rules! impl_global_for {
    ($ty:ty $(where $($tt:tt)*)?) => {
        impl<Ctx> $crate::globals::Global<Ctx> for $ty
        where $ty: MaybeConstInit + $crate::globals::Bind<Ctx>,
              $($($tt)*)?
        {
        }
    };
}

#[derive(Debug, Default)]
pub struct Display;
impl_global_for!(Display);

impl MaybeConstInit for Display {
    const INIT: Option<Self> = Some(Display);
}

impl GlobalMeta for Display {
    type Object = crate::objects::Display;

    fn interface(&self) -> &'static str {
        wl_display::v1::NAME
    }

    fn version(&self) -> u32 {
        wl_display::v1::VERSION
    }

    fn new_object(&self) -> Self::Object {
        crate::objects::Display::default()
    }
}

impl<Ctx: Client> Bind<Ctx> for Display {
    type BindFut<'a > = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a, Ctx: 'a;

    fn bind<'a>(&'a self, client: &'a mut Ctx, object_id: u32) -> Self::BindFut<'a> {
        use crate::connection::traits::Store;
        let ClientParts {
            objects,
            event_dispatcher,
            ..
        } = client.as_mut_parts();
        objects
            .get_mut::<Self::Object>(object_id)
            .unwrap()
            .initialized = true;

        struct DisplayEventHandler;
        impl<Ctx: Client> EventHandler<Ctx> for DisplayEventHandler {
            type Message = crate::connection::traits::StoreEvent;

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
                    use crate::connection::traits::StoreEventKind::*;
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
#[derive(Debug, Default)]
pub struct Registry;
impl_global_for!(Registry);

impl MaybeConstInit for Registry {
    const INIT: Option<Self> = Some(Registry);
}

impl GlobalMeta for Registry {
    type Object = crate::objects::Registry;

    fn interface(&self) -> &'static str {
        wl_registry::NAME
    }

    fn version(&self) -> u32 {
        wl_registry::VERSION
    }

    fn new_object(&self) -> Self::Object {
        crate::objects::Registry(None)
    }
}

impl<Ctx: Client> Bind<Ctx> for Registry {
    type BindFut<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a, Ctx: 'a;

    fn bind<'a>(&'a self, client: &'a mut Ctx, object_id: u32) -> Self::BindFut<'a> {
        async move {
            use crate::server::Globals;
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
