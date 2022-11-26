use std::{future::Future, rc::Rc};

use ::wl_common::InterfaceMessageDispatch;
use wl_protocol::wayland::{wl_display, wl_registry::v1 as wl_registry};

use crate::{
    connection::{ClientContext, State},
    events::{DispatchTo, EventHandler, EventMux},
    objects::RegistryState,
    server::{GlobalOf, Server},
};

pub trait Bind<Ctx> {
    /// An object that is the union of all objects that can be created from this
    /// global.
    type Objects;
    type BindFut<'a>: Future<Output = std::io::Result<Self::Objects>> + 'a
    where
        Ctx: 'a,
        Self: 'a;
    /// Called when the global is bound to a client, return the client side
    /// object, and optionally an I/O task to be completed after the object is
    /// inserted into the client's object store
    fn bind<'a>(&'a self, client: &'a mut Ctx, object_id: u32) -> Self::BindFut<'a>;
    fn interface(&self) -> &'static str;
    fn version(&self) -> u32;
}

/// A value that can be initialized with a constant
pub trait ConstInit {
    /// A value that can be used to create an instance of `Self`.
    const INIT: Self;
}

pub trait Global<Ctx>: ConstInit + Bind<Ctx> {}
impl<Ctx, T> Global<Ctx> for T where T: ConstInit + Bind<Ctx> {}

#[derive(Debug, Default)]
pub struct Display;

impl ConstInit for Display {
    const INIT: Self = Display;
}

impl<Ctx> Bind<Ctx> for Display
where
    Ctx: ClientContext + std::fmt::Debug,
    Registry: Bind<Ctx>,
{
    type Objects = DisplayObject;

    type BindFut<'a> = impl Future<Output = std::io::Result<Self::Objects>> + 'a;

    fn interface(&self) -> &'static str {
        wl_display::v1::NAME
    }

    fn version(&self) -> u32 {
        wl_display::v1::VERSION
    }

    fn bind<'a>(&'a self, _client: &'a mut Ctx, _object_id: u32) -> Self::BindFut<'a> {
        futures_util::future::ok(DisplayObject::Display(crate::objects::Display))
    }
}

#[derive(InterfaceMessageDispatch, Debug)]
#[wayland(crate = "crate")]
pub enum DisplayObject {
    Display(crate::objects::Display),
    Callback(crate::objects::Callback),
}

/// The registry singleton. This is an interface only object. The actual list of
/// globals is stored in the `Globals` implementation of the server context.
///
/// If you use this as your registry global implementation, you must also use
/// [`crate::objects::Registry`] as your client side registry proxy
/// implementation.
#[derive(Debug, Default)]
pub struct Registry;

impl ConstInit for Registry {
    const INIT: Self = Registry;
}

impl<Ctx> Bind<Ctx> for Registry
where
    Ctx: DispatchTo<Self> + State<RegistryState<GlobalOf<Ctx>>> + ClientContext,
{
    type Objects = RegistryObject;

    type BindFut<'a> = impl Future<Output = std::io::Result<Self::Objects>> + 'a;

    fn interface(&self) -> &'static str {
        wl_registry::NAME
    }

    fn version(&self) -> u32 {
        wl_registry::VERSION
    }

    fn bind<'a>(&'a self, client: &'a mut Ctx, object_id: u32) -> Self::BindFut<'a> {
        use crate::server::Globals;
        // Registry::new would add a listener into the Server EventSource. If you want
        // to implement the Registry global yourself, you need to remember to do
        // this yourself, somewhere.
        // Send the client a registry event if the proxy is inserted, the event handler
        // will send the list of existing globals to the client.
        // Also set up the registry state in the client context.
        let state = client.state_mut();
        // Add this object_id to the list of registry objects bound.
        state.add_registry_object(object_id);
        let handle = client.event_handle();
        client
            .server_context()
            .globals()
            .borrow()
            .add_update_listener((handle, Ctx::SLOT));
        async move {
            // This should send existing globals. We can't rely on setting the event flags
            // on `client`, because we need to send this _immediately_, whereas
            // the event handling will happen at an arbitrary time in the
            // future.
            Registry::invoke(client).await?;
            Ok(RegistryObject::Registry(crate::objects::Registry))
        }
    }
}

#[derive(InterfaceMessageDispatch, Debug)]
#[wayland(crate = "crate")]
pub enum RegistryObject {
    Registry(crate::objects::Registry),
}

impl<Ctx> EventHandler<Ctx> for Registry
where
    Ctx: EventMux + State<RegistryState<GlobalOf<Ctx>>> + DispatchTo<Self> + ClientContext,
{
    type Error = std::io::Error;

    type Fut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn invoke<'a>(ctx: &'a mut Ctx) -> Self::Fut<'a> {
        // Allocation: Global addition and removal should be rare.
        use crate::server::Globals;
        let empty = Default::default();
        let state = ctx.state();
        let known_globals = state.map(|s| &s.known_globals).unwrap_or(&empty);
        tracing::debug!("{:?}", known_globals.keys());
        let added: Vec<_> = ctx
            .server_context()
            .globals()
            .borrow()
            .iter()
            .filter_map(|(id, global)| {
                if !known_globals.contains_key(&id) {
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
        let state = ctx.state_mut();
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
