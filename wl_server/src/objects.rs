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
use ::wl_protocol::wayland::{wl_callback, wl_display, wl_registry};
use hashbrown::{HashMap, HashSet};
use tracing::debug;

use crate::{
    connection::{self, Connection, Entry, InterfaceMeta, Objects},
    events,
    globals::Global,
    provide_any::{request_ref, Demand},
    server::{self, Globals},
    Server,
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
            object_id: wl_common::types::Object(object_id),
            message:   wl_common::types::Str(message),
        }),
    )
    .await
}

/// The object ID of the wl_display object
pub const DISPLAY_ID: u32 = 1;

/// Default wl_display implementation
#[derive(Debug)]
pub struct Display;

impl Display {
    pub const EVENT_SLOT: i32 = -1;

    pub fn init_server<Ctx>(server: &mut Ctx) -> Result<(), wl_common::Infallible> {
        Ok(())
    }

    pub async fn handle_event<Ctx>(_ctx: &Ctx) -> Result<(), wl_common::Infallible> {
        Ok(())
    }
}

#[interface_message_dispatch]
impl<Ctx> wl_display::v1::RequestDispatch<Ctx> for Display
where
    Ctx: Connection
        + connection::Objects
        + connection::Evented<Ctx>
        + wl_common::Serial
        + std::fmt::Debug,
    crate::error::Error: From<Ctx::Error>,
{
    type Error = crate::error::Error;

    type GetRegistryFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a, Self: 'a;
    type SyncFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a, Self: 'a;

    fn sync<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        _object_id: u32,
        callback: ::wl_common::types::NewId,
    ) -> Self::SyncFut<'a> {
        async move {
            debug!("wl_display.sync {}", callback);
            if Objects::get(ctx, callback.0).is_some() {
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
        registry: ::wl_common::types::NewId,
    ) -> Self::GetRegistryFut<'a> {
        use server::Server;
        debug!("wl_display.get_registry {}", registry);
        let inserted = ctx.with_entry(registry.0, |entry| {
            if entry.is_vacant() {
                let server_context = ctx.server_context();
                let object = server_context
                    .globals()
                    .map_by_interface("wl_registry", |registry| registry.bind(ctx))
                    .expect("Required global wl_registry not found");
                entry.or_insert_boxed(object);
                true
            } else {
                false
            }
        });
        if inserted {
            // Add this object_id to the list of registry objects bound.
            let state = ctx.with_state_mut(
                events::EventSlot::REGISTRY as u8,
                |state: &mut RegistryState| {
                    state.0.insert(registry.0);
                },
            );
            if state.is_err() {
                // This could be the first registry object, so the state might not be set.
                ctx.set_state(
                    events::EventSlot::REGISTRY as u8,
                    RegistryState(std::iter::once(registry.0).collect::<HashSet<u32>>()),
                )
                .unwrap();
            }
        }
        async move {
            if inserted {
                // Send the global events
                debug!("Sending global events");
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
    Ctx: Connection + connection::Evented<Ctx> + connection::Objects + 'static,
    <Ctx::Context as server::Server>::Globals: Clone,
    crate::error::Error: From<<Ctx as Connection>::Error>,
{
    pub const EVENT_SLOT: i32 = crate::events::EventSlot::REGISTRY;

    pub fn new(ctx: &Ctx) -> Self {
        use server::{EventSource, Server};
        let server_context = ctx.server_context();
        server_context.globals().add_listener(ctx.event_handle());
        Self {
            known_globals: RefCell::new(HashMap::new()),
        }
    }

    pub fn init_server(server: &mut Ctx::Context) -> Result<(), wl_common::Infallible> {
        use server::Server;
        server.globals().insert(crate::globals::Registry::default());
        Ok(())
    }

    pub async fn handle_event(ctx: &Ctx) -> Result<(), crate::error::Error> {
        use crate::server::Server;
        // Allocation: Global addition and removal should be rare.
        let (deleted, added): (Vec<_>, Vec<_>) = ctx
            .with_state(Self::EVENT_SLOT as u8, |state: &RegistryState| {
                debug!("{:?}", state);
                let deleted = state
                    .0
                    .iter()
                    .map(|id| {
                        let object = ctx.get(*id).expect("inconsistent state of wl_registry");
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
                        let object = ctx.get(*id).expect("inconsistent state of wl_registry");
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
            .expect("registry state not found")
            .expect("registry state is of the wrong type");
        for (id, deleted) in deleted.into_iter() {
            for deleted_id in deleted {
                ctx.send(
                    id,
                    wl_registry::v1::Event::GlobalRemove(wl_registry::v1::events::GlobalRemove {
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
                    wl_registry::v1::Event::Global(wl_registry::v1::events::Global {
                        name: added_id,
                        interface: wl_common::types::Str(interface.as_c_str()),
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
struct RegistryState(HashSet<u32>);

impl crate::provide_any::Provider for RegistryState {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}

#[interface_message_dispatch]
impl<Ctx> wl_registry::v1::RequestDispatch<Ctx> for Registry<Ctx>
where
    crate::error::Error: From<Ctx::Error>,
    Ctx: Connection + Objects + connection::Evented<Ctx> + 'static,
    <Ctx::Context as server::Server>::Globals: Clone,
{
    type Error = crate::error::Error;

    type BindFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn bind<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        name: u32,
        id: ::wl_common::types::NewId,
    ) -> Self::BindFut<'a> {
        use std::ffi::CStr;
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
                let object = global.bind(ctx);
                let inserted = ctx.with_entry(id.0, |entry| {
                    if entry.is_vacant() {
                        entry.or_insert_boxed(object);
                        true
                    } else {
                        false
                    }
                });
                if !inserted {
                    send_id_in_use(ctx, id.0).await?;
                } else {
                    if global.interface() == "wl_registry" {
                        // Add the new registry to the list of registries this client has bound
                        ctx.with_state_mut(Self::EVENT_SLOT as u8, |state: &mut RegistryState| {
                            state.0.insert(object_id);
                        })
                        .expect("registry state not found");
                    }
                }
            } else {
                let message = CStr::from_bytes_with_nul(b"unknown global\0").unwrap();
                ctx.send(
                    DISPLAY_ID,
                    wl_display::v1::Event::Error(wl_display::v1::events::Error {
                        code:      wl_display::v1::enums::Error::InvalidObject as u32,
                        object_id: wl_common::types::Object(id.0),
                        message:   wl_common::types::Str(message),
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
impl<Ctx: Connection + connection::Evented<Ctx>> DropObject<Ctx> for Registry<Ctx> {
    fn drop_object(&self, ctx: &Ctx) {
        use crate::server::{EventSource, Server};
        let server_context = ctx.server_context();
        server_context
            .globals()
            .remove_listener(&ctx.event_handle());
    }
}

impl<Ctx: Connection + connection::Evented<Ctx> + 'static> InterfaceMeta for Registry<Ctx> {
    fn interface(&self) -> &'static str {
        wl_registry::v1::NAME
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
        demand.provide_ref(self as &dyn DropObject<Ctx>);
    }
}
