//! These are a set of objects the client can acquire. These objects typically
//! implement the RequestDispatch trait of one of the wayland interfaces. As
//! well as a `InterfaceMeta` trait to provide information about the interface,
//! and allowing them to be cast into trait objects and stored together.

use std::{
    cell::RefCell,
    future::Future,
    rc::{Rc, Weak},
};

use ::wl_common::interface_message_dispatch;
use ::wl_protocol::wayland::{wl_callback, wl_display, wl_registry::v1 as wl_registry};
use hashbrown::{HashMap, HashSet};
use tracing::debug;

use crate::{
    connection::{self, Connection, Entry, Objects, EventStates as _},
    globals::Global,
    provide_any::{request_ref, Demand, Provider},
    server::{self, EventSource, Globals, Server},
};

pub trait DropObject<Ctx> {
    /// Called when the object is removed from the object store. Provide a `dyn
    /// DropObject` from your Provider implementation if you need to do
    /// something when the object is removed.
    fn drop_object(&self, ctx: &Ctx);
}

async fn send_id_in_use<Ctx>(ctx: &Ctx, object_id: u32) -> Result<(), Ctx::Error>
where
    Ctx: Connection,
{
    use std::ffi::CStr;
    let message = CStr::from_bytes_with_nul(b"invalid method for wl_callback\0").unwrap();
    ctx.send(
        DISPLAY_ID,
        wl_display::v1::Event::Error(wl_display::v1::events::Error {
            code:      wl_display::v1::enums::Error::InvalidMethod as u32,
            object_id: wl_types::Object(object_id),
            message:   wl_types::Str(message),
        }),
    )
    .await
}

/// This is the bottom type for all per client objects. This trait provides some
/// metadata regarding the object, as well as a way to cast objects into a
/// common dynamic type.
///
/// # Note
///
/// If a object is a proxy of a global, it has to recognize if the global's
/// lifetime has ended, and turn all message sent to it to no-ops. This can
/// often be achieved by holding a Weak reference to the global object.
pub trait InterfaceMeta {
    /// Return the interface name of this object.
    fn interface(&self) -> &'static str;
    /// Case self to &dyn Any
    fn provide<'a>(&'a self, demand: &mut Demand<'a>);
}

impl<'b> Provider for dyn InterfaceMeta + 'b {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        self.provide(demand)
    }
}

/// The object ID of the wl_display object
pub const DISPLAY_ID: u32 = 1;

/// Default wl_display implementation
#[derive(Debug)]
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
    Ctx: Connection
        + connection::Evented<Ctx>
        + wl_common::Serial
        + std::fmt::Debug,
    Ctx::Context: EventSource,
    crate::error::Error: From<Ctx::Error>,
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
            if ctx.objects().get(callback.0).is_some() {
                send_id_in_use(ctx, callback.0).await?;
            } else {
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
            }
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
            let inserted = ctx.objects().with_entry(registry.0, |entry| {
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
            });
            if let Some(task) = inserted {
                if let Some(task) = task {
                    debug!("Sending global events");
                    task.await?;
                }
            } else {
                send_id_in_use(ctx, registry.0).await?;
            }
            Ok(())
        }
    }
}

impl InterfaceMeta for Display {
    fn interface(&self) -> &'static str {
        wl_display::v1::NAME
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}

pub struct Registry<Ctx: Connection> {
    known_globals: RefCell<HashMap<u32, Weak<dyn Global<Ctx::Context>>>>,
}

impl<Ctx> Registry<Ctx>
where
    Ctx: Connection + connection::Evented<Ctx> + 'static,
    Ctx::Context: EventSource,
    crate::error::Error: From<<Ctx as Connection>::Error>,
{
    pub fn new(ctx: &Ctx) -> Self {
        let server_context = ctx.server_context();
        server_context.add_listener(ctx.event_handle());

        Self {
            known_globals: RefCell::new(HashMap::new()),
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
        let (deleted, added): (Vec<_>, Vec<_>) = ctx.event_states()
            .with(slot as u8, |state: &RegistryState| {
                debug!("{:?}", state);
                let deleted = state
                    .0
                    .iter()
                    .map(|id| {
                        let object = ctx.objects().get(*id).expect("inconsistent state of wl_registry");
                        let object: &Registry<Ctx> =
                            request_ref(&*object).expect("wl_registry object has wrong type");
                        let deleted: Vec<_> = object
                            .known_globals
                            .borrow_mut()
                            .drain_filter(|_, v| v.upgrade().is_none())
                            .collect();
                        (*id, deleted)
                    })
                    .collect();
                let added = state
                    .0
                    .iter()
                    .map(|id| {
                        let object = ctx.objects().get(*id).expect("inconsistent state of wl_registry");
                        let object: &Registry<Ctx> =
                            request_ref(&*object).expect("wl_registry object has wrong type");
                        let mut added = Vec::new();
                        ctx.server_context().globals().for_each(|id, global| {
                            if !object.known_globals.borrow().contains_key(&id) {
                                object
                                    .known_globals
                                    .borrow_mut()
                                    .insert(id, Rc::downgrade(global));
                                added.push((id, global.interface(), global.version()));
                            }
                        });
                        (*id, added)
                    })
                    .collect();
                (deleted, added)
            })
            .expect("registry state is of the wrong type")
            .expect("registry state not found");
        for (id, deleted) in deleted.into_iter() {
            for deleted_id in deleted {
                ctx.send(
                    id,
                    wl_registry::Event::GlobalRemove(wl_registry::events::GlobalRemove {
                        name: deleted_id.0,
                    }),
                )
                .await?;
            }
        }
        for (id, added) in added.into_iter() {
            for (added_id, interface, version) in added {
                let interface = std::ffi::CString::new(interface).unwrap();
                ctx.send(
                    id,
                    wl_registry::Event::Global(wl_registry::events::Global {
                        name: added_id,
                        interface: wl_types::Str(interface.as_c_str()),
                        version,
                    }),
                )
                .await?;
            }
        }
        Ok(())
    }
}

/// Set of object_ids that are wl_registry objects, so we know which objects to
/// send the events from.
#[derive(Debug)]
pub(crate) struct RegistryState(pub(crate) HashSet<u32>);

impl crate::provide_any::Provider for RegistryState {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}

#[interface_message_dispatch]
impl<Ctx> wl_registry::RequestDispatch<Ctx> for Registry<Ctx>
where
    crate::error::Error: From<Ctx::Error>,
    Ctx: Connection + connection::Evented<Ctx> + 'static,
{
    type Error = crate::error::Error;

    type BindFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn bind<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        _object_id: u32,
        name: u32,
        id: wl_types::NewId,
    ) -> Self::BindFut<'a> {
        use std::ffi::CStr;
        tracing::debug!("bind name:{name}, id:{id}");
        async move {
            // We use the weak references we are holding to bind the global, even though it
            // could be outdated compared to the ones in server's global store.
            // This is because the ones stored in the weak references represents
            // what the client knows about, if we have a dead weak reference, it
            // means the client tries to bind a global that has been removed - even if there
            // is a global with that ID on the server's global state, which means the ID was
            // reused.
            let global = self
                .known_globals
                .borrow()
                .get(&name)
                .and_then(|g| g.upgrade());
            if let Some(global) = global {
                let (object, task) = global.bind(ctx, id.0);
                let inserted = ctx.objects().with_entry(id.0, |entry| {
                    if entry.is_vacant() {
                        entry.or_insert_boxed(object);
                        Some(task)
                    } else {
                        None
                    }
                });
                if let Some(task) = inserted {
                    if let Some(task) = task {
                        task.await?;
                    }
                } else {
                    send_id_in_use(ctx, id.0).await?;
                }
            } else {
                let message = CStr::from_bytes_with_nul(b"unknown global\0").unwrap();
                ctx.send(
                    DISPLAY_ID,
                    wl_display::v1::Event::Error(wl_display::v1::events::Error {
                        code:      wl_display::v1::enums::Error::InvalidObject as u32,
                        object_id: wl_types::Object(id.0),
                        message:   wl_types::Str(message),
                    }),
                )
                .await?;
            }
            Ok(())
        }
    }
}

impl<Ctx: Connection> ::std::fmt::Debug for Registry<Ctx> {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        struct DebugMap<'a, T>(&'a HashMap<u32, Weak<dyn Global<T>>>);
        impl<'a, T> std::fmt::Debug for DebugMap<'a, T> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_map()
                    .entries(self.0.iter().map(|(k, _)| (k, "…")))
                    .finish()
            }
        }
        f.debug_struct("Registry")
            .field("known_globals", &DebugMap(&self.known_globals.borrow()))
            .finish()
    }
}
impl<Ctx: Connection + connection::Evented<Ctx>> DropObject<Ctx> for Registry<Ctx>
where
    Ctx::Context: EventSource,
{
    fn drop_object(&self, ctx: &Ctx) {
        let server_context = ctx.server_context();
        server_context.remove_listener(ctx.event_handle());
    }
}

impl<Ctx: Connection + connection::Evented<Ctx> + 'static> InterfaceMeta for Registry<Ctx>
where
    Ctx::Context: EventSource,
{
    fn interface(&self) -> &'static str {
        wl_registry::NAME
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand
            .provide_ref(self)
            .provide_ref(self as &dyn DropObject<Ctx>);
    }
}

pub struct StateSlots;

impl StateSlots {
    pub const COMPOSITOR: usize = 0;
}
