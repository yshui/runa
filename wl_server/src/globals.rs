use std::{future::Future, pin::Pin, rc::Rc};

use wl_common::{Infallible, Serial};
use wl_io::traits::de::Deserializer;
use wl_protocol::wayland::{wl_display, wl_registry::v1 as wl_registry};

use crate::{
    connection::{Connection, EventStates, Evented},
    global_dispatch,
    objects::InterfaceMeta,
    provide_any::Demand,
    server::{EventSource, Server},
};

type PinnedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

pub trait GlobalMeta<S: Server + ?Sized> {
    fn interface(&self) -> &'static str;
    fn version(&self) -> u32;
    /// Called when the global is bound to a client, return the client side
    /// object, and optionally an I/O task to be completed after the object is
    /// inserted into the client's object store
    fn bind<'b, 'c>(
        &self,
        client: &'b S::Connection,
        object_id: u32,
    ) -> (
        Box<dyn InterfaceMeta<S::Connection>>,
        Option<PinnedFuture<'c, std::io::Result<()>>>,
    )
    where
        'b: 'c;
    fn provide<'a>(&'a self, demand: &mut Demand<'a>);
}

/// Maximum number of events that can be associated to globals
pub const MAX_EVENT_SLOT: usize = 64;

/// Allocate an event slot for a global.
pub fn allocate_event_slot() -> usize {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT_EVENT_SLOT: AtomicUsize = AtomicUsize::new(0);
    let ret = NEXT_EVENT_SLOT.fetch_add(1, Ordering::Relaxed);
    assert!(ret <= MAX_EVENT_SLOT, "Too many event slots allocated");
    ret
}

/// Dispatching messages related to a global.
pub trait Global<Ctx> {
    type Error;
    type HandleEventsError;
    type Fut<'a, D>: Future<Output = Result<bool, Self::Error>> + 'a
    where
        Ctx: 'a,
        D: Deserializer<'a> + 'a;
    type HandleEventsFut<'a>: Future<Output = Result<(), Self::HandleEventsError>> + 'a
    where
        Ctx: 'a;
    /// A value that can be used to create an instance of `Self`.
    const INIT: Self;
    /// Dispatch message from `reader` to object `obj`.
    ///
    /// Returns a future that resolves to a `Ok(true)` if the message is
    /// successfully dispatched, `Ok(false)` if the object's interface is not
    /// recognized by the global, and `Err` if the interface is recognized but
    /// an error occurs while dispatching it.
    fn dispatch<'a, D: Deserializer<'a>>(
        obj: &'a dyn InterfaceMeta<Ctx>,
        ctx: &'a mut Ctx,
        object_id: u32,
        reader: D,
    ) -> Self::Fut<'a, D>;
    fn handle_events<'a>(ctx: &'a mut Ctx, slot: usize) -> Option<Self::HandleEventsFut<'a>>;
}

impl<'b, S: Server> crate::provide_any::Provider for dyn GlobalMeta<S> + 'b {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        self.provide(demand)
    }
}

#[derive(Debug, Default)]
pub struct Display;

impl<S> GlobalMeta<S> for Display
where
    S: Server,
{
    fn interface(&self) -> &'static str {
        wl_display::v1::NAME
    }

    fn version(&self) -> u32 {
        wl_display::v1::VERSION
    }

    fn bind<'b, 'c>(
        &self,
        _client: &'b <S as Server>::Connection,
        _object_id: u32,
    ) -> (
        Box<dyn InterfaceMeta<S::Connection>>,
        Option<PinnedFuture<'c, std::io::Result<()>>>,
    )
    where
        'b: 'c,
    {
        (Box::new(crate::objects::Display), None)
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}
impl<Ctx> Global<Ctx> for Display
where
    Ctx: std::fmt::Debug + Serial + Evented<Ctx> + Connection,
{
    type Error = crate::error::Error;
    type HandleEventsError = Infallible;
    type HandleEventsFut<'a> = futures_util::future::Pending<Result<(), Self::HandleEventsError>>;

    const INIT: Self = Display;

    global_dispatch! {
        "wl_display" => crate::objects::Display,
    }

    fn handle_events<'a>(_ctx: &'a mut Ctx, _slot: usize) -> Option<Self::HandleEventsFut<'a>> {
        None
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

lazy_static::lazy_static! {
    pub static ref REGISTRY_EVENT_SLOT: usize = allocate_event_slot();
}

impl<S> GlobalMeta<S> for Registry
where
    S: Server + EventSource + 'static,
    S::Connection: Evented<S::Connection>,
{
    fn interface(&self) -> &'static str {
        wl_registry::NAME
    }

    fn version(&self) -> u32 {
        wl_registry::VERSION
    }

    fn bind<'b, 'c>(
        &self,
        client: &'b S::Connection,
        object_id: u32,
    ) -> (
        Box<dyn InterfaceMeta<S::Connection>>,
        Option<PinnedFuture<'c, std::io::Result<()>>>,
    )
    where
        'b: 'c,
    {
        use crate::objects::RegistryState;
        let slot = *REGISTRY_EVENT_SLOT;
        // Registry::new would add a listener into the Server EventSource. If you want
        // to implement the Registry global yourself, you need to remember to do
        // this yourself, somewhere.
        (
            Box::new(crate::objects::Registry::<S::Connection>::new(
                client, slot as u8,
            )),
            // Send the client a registry event if the proxy is inserted, the event handler will
            // send the list of existing globals to the client.
            // Also set up the registry state in the client context.
            Some(Box::pin(async move {
                // Add this object_id to the list of registry objects bound.
                let state = client
                    .event_states()
                    .with_mut(slot as u8, |state: &mut RegistryState<S::Connection>| {
                        state.add_registry_object(object_id);
                    })
                    .expect("Registry slot doesn't contain a RegistryState");
                if state.is_none() {
                    // This could be the first registry object, so the state might not be set.
                    tracing::debug!("Creating new registry state");
                    client
                        .event_states()
                        .set(slot as u8, RegistryState::<S::Connection>::new(object_id))
                        .unwrap();
                }
                client.event_handle().set(slot as u8);
                Ok(())
            })),
        )
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }
}

impl<Ctx> Global<Ctx> for Registry
where
    Ctx: Connection + Evented<Ctx>,
    Ctx::Context: EventSource,
{
    type Error = crate::error::Error;
    type HandleEventsError = crate::error::Error;

    type HandleEventsFut<'a> = impl Future<Output = Result<(), Self::HandleEventsError>> + 'a;

    const INIT: Self = Registry;

    global_dispatch! {
        "wl_registry" => crate::objects::Registry<Ctx>,
    }

    fn handle_events<'a>(ctx: &'a mut Ctx, slot: usize) -> Option<Self::HandleEventsFut<'a>> {
        use crate::{objects::RegistryState, server::Globals};
        tracing::debug!("Handling registry event {slot}");
        if slot != *REGISTRY_EVENT_SLOT {
            return None
        }
        let ctx = &*ctx;
        // Allocation: Global addition and removal should be rare.
        Some(
            ctx.event_states()
                .with_mut(slot as u8, |state: &mut RegistryState<Ctx>| {
                    tracing::debug!("{:?}", state);
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
                                    let interface =
                                        std::ffi::CString::new(global.interface()).unwrap();
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
                .expect("registry state not found"),
        )
    }
}

/// Generate a `fn dispatch` implementation for GlobalDispatch
#[macro_export]
macro_rules! global_dispatch {
    ( $($iface:literal => $obj:ty),*$(,)? ) => {
        type Fut<'a, D> = impl std::future::Future<Output = Result<bool, Self::Error>> + 'a
        where
            Ctx: 'a,
            D: $crate::__private::Deserializer<'a> + 'a;
        fn dispatch<'a, D: $crate::__private::Deserializer<'a>>(
            obj: &'a dyn $crate::objects::InterfaceMeta<Ctx>,
            ctx: &'a mut Ctx,
            object_id: u32,
            reader: D,
        ) -> Self::Fut<'a, D> {
            async move {
                match obj.interface() {
                    $(
                        $iface => {
                            let obj: &$obj = $crate::provide_any::request_ref(obj).unwrap();
                            $crate::__private::InterfaceMessageDispatch::
                                dispatch(obj, ctx, object_id, reader)
                                .await
                                .map(|()| true)
                        }
                    )*
                    _ => Ok(false),
                }
            }
        }
    };
}
