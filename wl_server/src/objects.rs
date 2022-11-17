//! These are a set of objects the client can acquire. These objects typically
//! implement the RequestDispatch trait of one of the wayland interfaces. As
//! well as a `InterfaceMeta` trait to provide information about the interface,
//! and allowing them to be cast into trait objects and stored together.

use std::{future::Future, rc::Rc};

pub use ::wl_common::interface_message_dispatch;
use ::wl_protocol::wayland::{
    wl_callback, wl_display::v1 as wl_display, wl_registry::v1 as wl_registry,
};
use tracing::debug;

use crate::{
    connection::{Connection, Objects, State},
    provide_any::{Demand, Provider},
    server::{self, Globals},
};

/// This is the bottom type for all per client objects. This trait provides some
/// metadata regarding the object, as well as a way to cast objects into a
/// common dynamic type.
///
/// # Note
///
/// If a object is a proxy of a global, it has to recognize if the global's
/// lifetime has ended, and turn all message sent to it to no-ops. This can
/// often be achieved by holding a Weak reference to the global object.
pub trait Object: std::any::Any {
    /// Return the interface name of this object.
    fn interface(&self) -> &'static str;
    /// A function that will be called when the client disconnects. It should free up allocated
    /// resources if any. This function should not try to send anything to the client, as it has
    /// already disconnected.
    ///
    /// The context object is passed as a `dyn Any` to make this function object safe.
    fn on_disconnect(&mut self, _ctx: &mut dyn std::any::Any) {}
    // TODO: maybe delete?
    fn provide<'a>(&'a self, _demand: &mut Demand<'a>) {}
}

impl std::fmt::Debug for dyn Object {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Object")
            .field("interface", &self.interface())
            .finish()
    }
}

impl Provider for dyn Object {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        self.provide(demand)
    }
}

/// The object ID of the wl_display object
pub const DISPLAY_ID: u32 = 1;

/// Default wl_display implementation
#[derive(Debug, Clone, Copy)]
pub struct Display;

#[interface_message_dispatch]
impl<Ctx> wl_display::RequestDispatch<Ctx> for Display
where
    Ctx: Connection + std::fmt::Debug,
{
    type Error = crate::error::Error;

    type GetRegistryFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a, Self: 'a;
    type SyncFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a, Self: 'a;

    fn sync<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        _object_id: u32,
        callback: wl_types::NewId,
    ) -> Self::SyncFut<'a> {
        async move {
            debug!("wl_display.sync {}", callback);
            if ctx.objects().borrow().get(callback.0).is_some() {
                return Err(crate::error::Error::IdExists(callback.0))
            }
            ctx.send(
                callback.0,
                wl_callback::v1::Event::Done(wl_callback::v1::events::Done {
                    // TODO: setup event serial
                    callback_data: 0,
                }),
            )
            .await?;
            ctx.send(
                DISPLAY_ID,
                wl_display::Event::DeleteId(wl_display::events::DeleteId { id: callback.0 }),
            )
            .await?;
            Ok(())
        }
    }

    fn get_registry<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        _object_id: u32,
        registry: wl_types::NewId,
    ) -> Self::GetRegistryFut<'a> {
        async move {
            use server::Server;
            debug!("wl_display.get_registry {}", registry);
            if ctx.objects().borrow().get(registry.0).is_none() {
                let global = {
                    let server_context = ctx.server_context();
                    let globals = server_context.globals().borrow();
                    let global = globals
                        .iter()
                        .find_map(|(_, g)| {
                            if g.interface() == wl_registry::NAME {
                                Some(g)
                            } else {
                                None
                            }
                        })
                        .expect("wl_registry not found")
                        .clone();
                    global
                };
                let object = global.bind(ctx, registry.0).await?;
                ctx.objects()
                    .borrow_mut()
                    .insert_boxed(registry.0, object)
                    .unwrap();
                Ok(())
            } else {
                Err(crate::error::Error::IdExists(registry.0))
            }
        }
    }
}

impl Object for Display {
    fn interface(&self) -> &'static str {
        wl_display::NAME
    }
}

#[derive(Clone, Debug)]
pub struct Registry;

mod private {
    use std::rc::Weak;

    use derivative::Derivative;
    use hashbrown::{HashMap, HashSet};

    use crate::globals::Global;
    /// Set of object_ids that are wl_registry objects, so we know which objects
    /// to send the events from.
    #[derive(Derivative)]
    #[derivative(Default(bound = ""), Debug(bound = "Ctx: 'static"))]
    pub struct RegistryState<Ctx> {
        /// Known wl_registry objects. We will send delta global events to them.
        pub(crate) registry_objects:     HashSet<u32>,
        /// Newly created wl_registry objects. We will send all the globals to
        /// them then move them to `registry_objects`.
        pub(crate) new_registry_objects: Vec<u32>,
        /// List of globals known to this client context
        pub(crate) known_globals:        HashMap<u32, Weak<dyn Global<Ctx>>>,
    }

    impl<Ctx> RegistryState<Ctx> {
        pub(crate) fn new(registry_object_id: u32) -> Self {
            Self {
                registry_objects:     HashSet::new(),
                new_registry_objects: vec![registry_object_id],
                known_globals:        HashMap::new(),
            }
        }

        pub(crate) fn add_registry_object(&mut self, id: u32) {
            self.new_registry_objects.push(id);
        }
    }
}

pub(crate) use private::RegistryState;

#[interface_message_dispatch]
impl<Ctx> wl_registry::RequestDispatch<Ctx> for Registry
where
    Ctx: Connection + State<RegistryState<Ctx>>,
{
    type Error = crate::error::Error;

    type BindFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn bind<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        _object_id: u32,
        name: u32,
        _interface: wl_types::Str,
        _version: u32,
        id: wl_types::NewId,
    ) -> Self::BindFut<'a> {
        tracing::debug!("bind name:{name}, id:{id}");
        async move {
            // We use the weak references we are holding to bind the global, even though it
            // could be outdated compared to the ones in server's global store.
            // This is because the ones stored in the weak references represents
            // what the client knows about - synchronized using Global and GlobalRemove
            // events. if we have a dead weak reference, it means the client tries
            // to bind a global that has been removed, but the GlobalRemove hasn't been
            // send, so it doesn't know about the removal yet. Sometimes there can even be
            // global with that ID on the server's global state, which means the ID
            // was reused.
            let state = ctx.state().expect("RegistryState not set");
            if ctx.objects().borrow().get(id.0).is_some() {
                return Err(crate::error::Error::IdExists(id.0))
            }
            let global = state.known_globals.get(&name).and_then(|g| g.upgrade());
            if let Some(global) = global {
                let object = global.bind(ctx, id.0).await?;
                ctx.objects()
                    .borrow_mut()
                    .insert_boxed(id.0, object)
                    .unwrap(); // can't fail
                Ok(())
            } else {
                Err(crate::error::Error::UnknownGlobal(name))
            }
        }
    }
}

impl Object for Registry {
    fn interface(&self) -> &'static str {
        wl_registry::NAME
    }
}

#[derive(Debug, Default)]
pub struct Callback;
impl Object for Callback {
    fn interface(&self) -> &'static str {
        wl_protocol::wayland::wl_callback::v1::NAME
    }
}

impl Callback {
    pub async fn fire(object_id: u32, data: u32, ctx: &impl Connection) -> std::io::Result<()> {
        ctx.send(
            object_id,
            wl_protocol::wayland::wl_callback::v1::events::Done {
                callback_data: data,
            },
        )
        .await?;
        let callback = ctx.objects().borrow().get(object_id).unwrap().clone();
        let interface = callback.interface();
        let _: Rc<Self> = Rc::downcast(callback).unwrap_or_else(|_| panic!("object is not callback, it's {}", interface));
        ctx.objects().borrow_mut().remove(object_id).unwrap();
        ctx.send(DISPLAY_ID, wl_display::events::DeleteId { id: object_id })
            .await?;
        Ok(())
    }
}
