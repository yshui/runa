use std::{future::Future, pin::Pin, rc::Rc};

use wl_io::traits::de::Deserializer;
use wl_protocol::wayland::{wl_display, wl_registry::v1 as wl_registry};

use crate::{
    connection::{Connection, State},
    events::{DispatchTo, EventHandler, EventMux},
    global_dispatch,
    objects::{Object, RegistryState},
    server::Server,
};

type PinnedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

pub trait GlobalMeta: std::any::Any {
    fn interface(&self) -> &'static str;
    fn version(&self) -> u32;
}

impl std::fmt::Debug for dyn GlobalMeta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobalMeta")
            .field("interface", &self.interface())
            .field("version", &self.version())
            .finish()
    }
}

pub trait Global<Ctx>: GlobalMeta {
    /// Called when the global is bound to a client, return the client side
    /// object, and optionally an I/O task to be completed after the object is
    /// inserted into the client's object store
    fn bind<'b, 'c>(
        &self,
        client: &'b mut Ctx,
        object_id: u32,
    ) -> PinnedFuture<'c, std::io::Result<Box<dyn Object>>>
    where
        'b: 'c;
}

impl<Ctx: 'static> std::fmt::Debug for dyn Global<Ctx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Global")
            .field("interface", &self.interface())
            .field("version", &self.version())
            .finish()
    }
}

/// Dispatching messages related to a global.
pub trait GlobalDispatch<Ctx> {
    type Error;
    type Fut<'a, D>: Future<Output = Result<bool, Self::Error>> + 'a
    where
        Ctx: 'a,
        D: Deserializer<'a> + 'a;
    /// A value that can be used to create an instance of `Self`.
    const INIT: Self;
    /// Dispatch message from `reader` to object `obj`.
    ///
    /// Returns a future that resolves to a `Ok(true)` if the message is
    /// successfully dispatched, `Ok(false)` if the object's interface is not
    /// recognized by the global, and `Err` if the interface is recognized but
    /// an error occurs while dispatching it.
    fn dispatch<'a, D: Deserializer<'a>>(
        obj: &'a dyn Object,
        ctx: &'a mut Ctx,
        object_id: u32,
        reader: D,
    ) -> Self::Fut<'a, D>;
}

#[derive(Debug, Default)]
pub struct Display;

impl GlobalMeta for Display {
    fn interface(&self) -> &'static str {
        wl_display::v1::NAME
    }

    fn version(&self) -> u32 {
        wl_display::v1::VERSION
    }
}
impl<Ctx> Global<Ctx> for Display {
    fn bind<'b, 'c>(
        &self,
        _client: &'b mut Ctx,
        _object_id: u32,
    ) -> PinnedFuture<'c, std::io::Result<Box<dyn Object>>>
    where
        'b: 'c,
    {
        Box::pin(futures_util::future::ok(
            Box::new(crate::objects::Display) as _
        ))
    }
}
impl<Ctx> GlobalDispatch<Ctx> for Display
where
    Ctx: std::fmt::Debug + Connection,
{
    type Error = crate::error::Error;

    const INIT: Self = Display;

    global_dispatch! {
        "wl_display" => crate::objects::Display,
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

impl GlobalMeta for Registry {
    fn interface(&self) -> &'static str {
        wl_registry::NAME
    }

    fn version(&self) -> u32 {
        wl_registry::VERSION
    }
}

impl<Ctx> Global<Ctx> for Registry
where
    Ctx: DispatchTo<Self> + State<RegistryState<Ctx>> + Connection,
{
    fn bind<'b, 'c>(
        &self,
        client: &'b mut Ctx,
        object_id: u32,
    ) -> PinnedFuture<'c, std::io::Result<Box<dyn Object>>>
    where
        'b: 'c,
    {
        use crate::server::Globals;
        // Registry::new would add a listener into the Server EventSource. If you want
        // to implement the Registry global yourself, you need to remember to do
        // this yourself, somewhere.
        // Send the client a registry event if the proxy is inserted, the event handler
        // will send the list of existing globals to the client.
        // Also set up the registry state in the client context.
        if let Some(state) = client.state_mut() {
            // Add this object_id to the list of registry objects bound.
            state.add_registry_object(object_id);
        } else {
            tracing::debug!("Creating new registry state");
            client.set_state(RegistryState::new(object_id));
        }
        let handle = client.event_handle();
        client.server_context().globals().borrow().add_update_listener((handle, Ctx::SLOT));
        Box::pin(async move {
            // This should send existing globals. We can't rely on setting the event flags
            // on `client`, because we need to send this _immediately_, whereas
            // the event handling will happen at an arbitrary time in the
            // future.
            Registry::invoke(client).await?;
            Ok(Box::new(crate::objects::Registry) as _)
        })
    }
}

impl<Ctx> GlobalDispatch<Ctx> for Registry
where
    Ctx: Connection + State<RegistryState<Ctx>> + DispatchTo<Self>,
{
    type Error = crate::error::Error;

    const INIT: Self = Registry;

    global_dispatch! {
        "wl_registry" => crate::objects::Registry,
    }
}

impl<Ctx> EventHandler<Ctx> for Registry
where
    Ctx: EventMux + State<RegistryState<Ctx>> + DispatchTo<Self> + Connection,
{
    type Error = std::io::Error;

    type Fut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a;

    fn invoke(ctx: &mut Ctx) -> Self::Fut<'_> {
        // Allocation: Global addition and removal should be rare.
        use crate::server::Globals;
        let state = ctx.state().unwrap();
        tracing::debug!("{:?}", state);
        let added: Vec<_> = ctx
            .server_context()
            .globals()
            .borrow()
            .iter()
            .filter_map(|(id, global)| {
                if !state.known_globals.contains_key(&id) {
                    Some((
                        id,
                        std::ffi::CString::new(global.interface()).unwrap(),
                        global.version(),
                        Rc::downgrade(global),
                    ))
                } else {
                    None
                }
            })
            .collect();
        let state = ctx.state_mut().unwrap();
        let deleted: Vec<_> = state
            .known_globals
            .drain_filter(|_, v| v.upgrade().is_none())
            .collect();
        for (id, _, _, global) in &added {
            state.known_globals.insert(*id, global.clone());
        }
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
                for (id, interface, version, _) in added.iter() {
                    ctx.send(registry_id, wl_registry::events::Global {
                        name:      *id,
                        interface: wl_types::Str(interface.as_c_str()),
                        version:   *version,
                    })
                    .await?;
                }
                for (id, _) in deleted.iter() {
                    ctx.send(registry_id, wl_registry::events::GlobalRemove { name: *id })
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
    }
}

// TODO: support multiple versions of the same interface
/// Generate a `fn dispatch` implementation for GlobalDispatch
#[macro_export]
macro_rules! global_dispatch {
    ( $($iface:literal => $obj:ty),*$(,)? ) => {
        type Fut<'a, D> = impl std::future::Future<Output = Result<bool, Self::Error>> + 'a
        where
            Ctx: 'a,
            D: $crate::__private::Deserializer<'a> + 'a;
        fn dispatch<'a, D: $crate::__private::Deserializer<'a>>(
            obj: &'a dyn $crate::objects::Object,
            ctx: &'a mut Ctx,
            object_id: u32,
            reader: D,
        ) -> Self::Fut<'a, D> {
            async move {
                use std::any::Any;
                match obj.interface() {
                    $(
                        $iface => {
                            let obj: Option<&$obj> = (obj as &dyn Any).downcast_ref();
                            if let Some(obj) = obj {
                                $crate::__private::InterfaceMessageDispatch::
                                    dispatch(obj, ctx, object_id, reader)
                                    .await
                                    .map(|()| true)
                            } else {
                                Ok(false)
                            }
                        }
                    )*
                    _ => Ok(false),
                }
            }
        }
    };
}
