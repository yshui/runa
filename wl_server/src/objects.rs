//! These are a set of objects the client can acquire. These objects typically
//! implement the RequestDispatch trait of one of the wayland interfaces. As
//! well as a `InterfaceMeta` trait to provide information about the interface,
//! and allowing them to be cast into trait objects and stored together.

use std::{
    cell::RefCell,
    future::Future,
    rc::{Rc, Weak},
};

pub use ::wl_common::interface_message_dispatch;
use ::wl_protocol::wayland::{wl_callback, wl_display, wl_registry::v1 as wl_registry};
use hashbrown::{HashMap, HashSet};
use tracing::debug;

use crate::{
    connection::{self, Connection, Entry, EventStates as _, Objects},
    globals::Global,
    provide_any::{request_ref, Demand, Provider},
    server::{self, EventSource, Globals, Server},
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
pub trait InterfaceMeta<Ctx>: 'static {
    /// Return the interface name of this object.
    fn interface(&self) -> &'static str;
    /// Case self to &dyn Any
    fn provide<'a>(&'a self, demand: &mut Demand<'a>);
    /// Called when the object is removed from the object store.
    fn on_drop(&self, _ctx: &Ctx) {
    }
}

impl<Ctx: 'static> Provider for dyn InterfaceMeta<Ctx> {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        self.provide(demand)
    }
}

/// The object ID of the wl_display object
pub const DISPLAY_ID: u32 = 1;

/// Default wl_display implementation
#[derive(Debug, Clone, Copy)]
pub struct Display;

impl Display {
    pub fn init_server<Ctx: server::ServerBuilder>(
        _server: &mut Ctx,
    ) -> Result<(), wl_common::Infallible> {
        Ok(())
    }

    pub async fn handle_events<Ctx>(
        _ctx: &Ctx,
        _slot: usize,
        _event: &'static str,
    ) -> Result<(), wl_common::Infallible> {
        Ok(())
    }
}

#[interface_message_dispatch]
impl<Ctx> wl_display::v1::RequestDispatch<Ctx> for Display
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
                wl_display::v1::Event::DeleteId(wl_display::v1::events::DeleteId {
                    id: callback.0,
                }),
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

impl<Ctx> InterfaceMeta<Ctx> for Display {
    fn interface(&self) -> &'static str {
        wl_display::v1::NAME
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}

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
    Ctx: Connection + connection::Evented<Ctx> + 'static,
    Ctx::Context: EventSource,
{
    pub fn new(ctx: &Ctx) -> Self {
        let server_context = ctx.server_context();
        server_context.add_listener(ctx.event_handle());

        Self {
            slot: server_context
                .slots()
                .iter()
                .position(|&s| s == wl_registry::NAME)
                .unwrap() as u8,
            _ctx: std::marker::PhantomData,
        }
    }

    pub fn init_server(
        server: &mut <Ctx::Context as server::Server>::Builder,
    ) -> Result<(), wl_common::Infallible> {
        use server::ServerBuilder;
        server
            .global(crate::globals::Registry::default())
            .event_slot(wl_registry::NAME);
        Ok(())
    }

    pub async fn handle_events(
        ctx: &Ctx,
        slot: usize,
        event: &'static str,
    ) -> Result<(), crate::error::Error> {
        tracing::debug!("Handling registry event {slot} {event}");
        if event != wl_registry::NAME {
            return Ok(())
        }
        // Allocation: Global addition and removal should be rare.
        ctx.event_states()
            .with_mut(slot as u8, |state: &mut RegistryState<Ctx>| {
                debug!("{:?}", state);
                let deleted: Vec<_> = state
                    .known_globals
                    .drain_filter(|_, v| v.upgrade().is_none())
                    .collect();
                let mut added = Vec::new();
                ctx.server_context().globals().for_each(|id, global| {
                    if !state.known_globals.contains_key(&id) {
                        state.known_globals.insert(id, Rc::downgrade(global));
                        added.push((
                            id,
                            std::ffi::CString::new(global.interface()).unwrap(),
                            global.version(),
                        ));
                    }
                });
                let registry_objects: Vec<_> = state.registry_objects.iter().copied().collect();
                let send_new = if !state.new_registry_objects.is_empty() {
                    // Only clone known_globals is we have new registry objects
                    let new_registry_objects = state.new_registry_objects.clone();
                    let known_globals = state.known_globals.clone();
                    state
                        .registry_objects
                        .extend(state.new_registry_objects.drain(..));
                    Some((new_registry_objects, known_globals))
                } else {
                    None
                };
                async move {
                    for registry_id in registry_objects.into_iter() {
                        for (id, interface, version) in added.iter() {
                            ctx.send(registry_id, wl_registry::events::Global {
                                name:      *id,
                                interface: wl_types::Str(interface.as_c_str()),
                                version:   *version,
                            })
                            .await?;
                        }
                        for (id, _) in deleted.iter() {
                            ctx.send(registry_id, wl_registry::events::GlobalRemove {
                                name: *id,
                            })
                            .await?;
                        }
                    }
                    if let Some((new_registry_objects, known_globals)) = send_new {
                        for registry_id in new_registry_objects.into_iter() {
                            for (id, global) in known_globals.iter() {
                                let global = global.upgrade().unwrap();
                                let interface = std::ffi::CString::new(global.interface()).unwrap();
                                ctx.send(registry_id, wl_registry::events::Global {
                                    name:      *id,
                                    interface: wl_types::Str(interface.as_c_str()),
                                    version:   global.version(),
                                })
                                .await?;
                            }
                        }
                    }
                    Ok(())
                }
            })
            .expect("registry state is of the wrong type")
            .expect("registry state not found")
            .await
    }
}

/// Set of object_ids that are wl_registry objects, so we know which objects to
/// send the events from.
pub(crate) struct RegistryState<Ctx: Connection> {
    /// Known wl_registry objects. We will send delta global events to them.
    registry_objects:     HashSet<u32>,
    /// Newly created wl_registry objects. We will send all the globals to them
    /// then move them to `registry_objects`.
    new_registry_objects: Vec<u32>,
    /// List of globals known to this client context
    known_globals:        HashMap<u32, Weak<dyn Global<Ctx::Context>>>,
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
        struct DebugMap<'a, Ctx: Connection>(&'a HashMap<u32, Weak<dyn Global<Ctx::Context>>>);
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

impl<Ctx: Connection> crate::provide_any::Provider for RegistryState<Ctx> {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
    fn provide_mut<'a>(&'a mut self, demand: &mut Demand<'a>) {
        demand.provide_mut(self);
    }
}

#[interface_message_dispatch]
impl<Ctx> wl_registry::RequestDispatch<Ctx> for Registry<Ctx>
where
    Ctx: Connection + connection::Evented<Ctx> + 'static,
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

impl<Ctx: Connection + connection::Evented<Ctx> + 'static> InterfaceMeta<Ctx> for Registry<Ctx>
where
    Ctx::Context: EventSource,
{
    fn interface(&self) -> &'static str {
        wl_registry::NAME
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand
            .provide_ref(self);
    }
    fn on_drop(&self, ctx: &Ctx) {
        let server_context = ctx.server_context();
        server_context.remove_listener(ctx.event_handle());
    }
}

pub struct StateSlots;

impl StateSlots {
    pub const COMPOSITOR: usize = 0;
}
