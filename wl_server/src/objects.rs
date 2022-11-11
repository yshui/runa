//! These are a set of objects the client can acquire. These objects typically
//! implement the RequestDispatch trait of one of the wayland interfaces. As
//! well as a `InterfaceMeta` trait to provide information about the interface,
//! and allowing them to be cast into trait objects and stored together.

use std::{future::Future, rc::Weak};

pub use ::wl_common::interface_message_dispatch;
use ::wl_protocol::wayland::{
    wl_callback, wl_display::v1 as wl_display, wl_registry::v1 as wl_registry,
};
use hashbrown::{HashMap, HashSet};
use tracing::debug;

use crate::{
    connection::{self, Connection, Entry, EventStates as _, Objects},
    globals::GlobalMeta,
    provide_any::{Demand, Provider},
    server::{self, EventSource, Globals},
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
pub trait ObjectMeta: std::any::Any {
    /// Return the interface name of this object.
    fn interface(&self) -> &'static str;
    // TODO: maybe delete?
    fn provide<'a>(&'a self, _demand: &mut Demand<'a>) {}
}

impl Provider for dyn ObjectMeta {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        self.provide(demand)
    }
}

pub trait Object<Ctx>: ObjectMeta {
    /// Called when the object is removed from the object store.
    fn on_drop(&self, _ctx: &Ctx) {}
}

/// The object ID of the wl_display object
pub const DISPLAY_ID: u32 = 1;

/// Default wl_display implementation
#[derive(Debug, Clone, Copy)]
pub struct Display;

#[interface_message_dispatch]
impl<Ctx> wl_display::RequestDispatch<Ctx> for Display
where
    Ctx: Connection + connection::Evented<Ctx> + wl_common::Serial + std::fmt::Debug,
    Ctx::Context: EventSource,
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
            let server_context = ctx.server_context();
            let inserted = {
                let mut objects = ctx.objects().borrow_mut();
                let entry = objects.entry(registry.0);
                if entry.is_vacant() {
                    let (object, task) = server_context
                        .globals()
                        .map_by_interface(wl_registry::NAME, |registry_global| {
                            registry_global.bind(ctx, registry.0)
                        })
                        .expect("Required global wl_registry not found");
                    entry.or_insert_boxed(object);
                    Some(task)
                } else {
                    None
                }
            };
            if let Some(task) = inserted {
                if let Some(task) = task {
                    debug!("Sending global events");
                    task.await?;
                }
                Ok(())
            } else {
                Err(crate::error::Error::IdExists(registry.0))
            }
        }
    }
}

impl ObjectMeta for Display {
    fn interface(&self) -> &'static str {
        wl_display::NAME
    }
}
impl<Ctx> Object<Ctx> for Display {}

pub struct Registry<Ctx> {
    slot: u8,
    _ctx: std::marker::PhantomData<Ctx>,
}

impl<Ctx> Clone for Registry<Ctx> {
    fn clone(&self) -> Self {
        Self {
            slot: self.slot,
            _ctx: std::marker::PhantomData,
        }
    }
}

impl<Ctx> Registry<Ctx>
where
    Ctx: Connection + connection::Evented<Ctx>,
    Ctx::Context: EventSource,
{
    pub fn new(ctx: &Ctx, slot: u8) -> Self {
        let server_context = ctx.server_context();
        server_context.add_listener(ctx.event_handle());

        Self {
            slot,
            _ctx: std::marker::PhantomData,
        }
    }
}

/// Set of object_ids that are wl_registry objects, so we know which objects to
/// send the events from.
pub(crate) struct RegistryState<Ctx: Connection> {
    /// Known wl_registry objects. We will send delta global events to them.
    pub(crate) registry_objects:     HashSet<u32>,
    /// Newly created wl_registry objects. We will send all the globals to them
    /// then move them to `registry_objects`.
    pub(crate) new_registry_objects: Vec<u32>,
    /// List of globals known to this client context
    pub(crate) known_globals:        HashMap<u32, Weak<dyn GlobalMeta<Ctx::Context>>>,
}

impl<Ctx: Connection> RegistryState<Ctx> {
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

impl<Ctx: Connection> std::fmt::Debug for RegistryState<Ctx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct DebugMap<'a, Ctx: Connection>(&'a HashMap<u32, Weak<dyn GlobalMeta<Ctx::Context>>>);
        impl<Ctx: Connection> std::fmt::Debug for DebugMap<'_, Ctx> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_set()
                    .entries(self.0.iter().map(|(k, _)| k))
                    .finish()
            }
        }
        f.debug_struct("RegistryState")
            .field("registry_objects", &self.registry_objects)
            .field("new_registry_objects", &self.new_registry_objects)
            .field("known_globals", &DebugMap::<Ctx>(&self.known_globals))
            .finish()
    }
}

#[interface_message_dispatch]
impl<Ctx> wl_registry::RequestDispatch<Ctx> for Registry<Ctx>
where
    Ctx: Connection + connection::Evented<Ctx>,
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
            let bind_results = ctx
                .event_states()
                .with(self.slot, |state: &RegistryState<Ctx>| {
                    state
                        .known_globals
                        .get(&name)
                        .and_then(|g| g.upgrade())
                        .map(|g| g.bind(ctx, id.0))
                })
                .expect("registry state is of the wrong type")
                .expect("registry state not found");
            if let Some((object, task)) = bind_results {
                let inserted = {
                    let mut objects = ctx.objects().borrow_mut();
                    let entry = objects.entry(id.0);
                    if entry.is_vacant() {
                        entry.or_insert_boxed(object);
                        Some(task)
                    } else {
                        None
                    }
                };
                if let Some(task) = inserted {
                    if let Some(task) = task {
                        task.await?;
                    }
                    Ok(())
                } else {
                    Err(crate::error::Error::IdExists(id.0))
                }
            } else {
                Err(crate::error::Error::UnknownGlobal(name))
            }
        }
    }
}

impl<Ctx> ::std::fmt::Debug for Registry<Ctx> {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.debug_struct("Registry")
            .field("slot", &self.slot)
            .finish()
    }
}

impl<Ctx: 'static> ObjectMeta for Registry<Ctx> {
    fn interface(&self) -> &'static str {
        wl_registry::NAME
    }
}
impl<Ctx: Connection + connection::Evented<Ctx> + 'static> Object<Ctx> for Registry<Ctx>
where
    Ctx::Context: EventSource,
{
    fn on_drop(&self, ctx: &Ctx) {
        let server_context = ctx.server_context();
        server_context.remove_listener(ctx.event_handle());
    }
}

#[derive(Debug, Default)]
pub struct Callback;
impl<Ctx> Object<Ctx> for Callback {}
impl ObjectMeta for Callback {
    fn interface(&self) -> &'static str {
        wl_protocol::wayland::wl_callback::v1::NAME
    }
}

impl Callback {
    pub async fn fire(object_id: u32, data: u32, ctx: &mut impl Connection) -> std::io::Result<()> {
        ctx.send(
            object_id,
            wl_protocol::wayland::wl_callback::v1::events::Done {
                callback_data: data,
            },
        )
        .await?;
        let mut objects = ctx.objects().borrow_mut();
        objects.remove(ctx, object_id).unwrap();
        ctx.send(DISPLAY_ID, wl_display::events::DeleteId { id: object_id })
            .await?;
        Ok(())
    }
}
