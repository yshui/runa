use std::{future::Future, rc::Rc};

use wl_protocol::wayland::{wl_display, wl_registry::v1 as wl_registry};

use crate::{
    connection::{Client, State},
    events::{DispatchTo, EventHandler, EventMux},
    objects::RegistryState,
    server::Server,
};

pub trait Bind<Ctx> {
    type BindFut<'a>: Future<Output = std::io::Result<Ctx::Object>> + 'a
    where
        Ctx: Client + 'a,
        Self: 'a;
    /// Called when the global is bound to a client, return the client side
    /// object, and optionally an I/O task to be completed after the object is
    /// inserted into the client's object store
    fn bind<'a>(&'a self, client: &'a mut Ctx, object_id: u32) -> Self::BindFut<'a>
    where
        Ctx: Client;
    fn interface(&self) -> &'static str;
    fn version(&self) -> u32;
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

impl<Ctx> Bind<Ctx> for Display
where
    Ctx: Client + std::fmt::Debug,
    Ctx::Object: From<crate::objects::Display>,
    Registry: Bind<Ctx>,
{
    type BindFut<'a> = impl Future<Output = std::io::Result<Ctx::Object>> + 'a;

    fn interface(&self) -> &'static str {
        wl_display::v1::NAME
    }

    fn version(&self) -> u32 {
        wl_display::v1::VERSION
    }

    fn bind<'a>(&'a self, _client: &'a mut Ctx, _object_id: u32) -> Self::BindFut<'a>
    where
        Ctx: Client,
    {
        futures_util::future::ok(crate::objects::Display.into())
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

type GlobalOf<Ctx> = <<Ctx as Client>::ServerContext as Server>::Global;

impl<Ctx> Bind<Ctx> for Registry
where
    Ctx: DispatchTo<Self> + State<RegistryState<GlobalOf<Ctx>>> + Client,
    Ctx::Object: From<crate::objects::Registry>,
{
    type BindFut<'a> = impl Future<Output = std::io::Result<Ctx::Object>> + 'a;

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
            Ok(crate::objects::Registry.into())
        }
    }
}

impl<Ctx> EventHandler<Ctx> for Registry
where
    Ctx: EventMux + State<RegistryState<GlobalOf<Ctx>>> + DispatchTo<Self> + Client,
{
    type Error = std::io::Error;

    type Fut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn invoke(ctx: &mut Ctx) -> Self::Fut<'_> {
        // Allocation: Global addition and removal should be rare.
        use crate::server::Globals;
        let (ro_ctx, state) = ctx.state();
        let known_globals = &state.known_globals;
        tracing::debug!("{:?}", known_globals.keys());
        let added: Vec<_> = ro_ctx
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
            use crate::connection::WriteMessage;
            for registry_id in registry_objects.into_iter() {
                for (id, interface, version, _) in added.iter() {
                    ctx.connection().send(registry_id, wl_registry::events::Global {
                        name:      *id,
                        interface: wl_types::Str(interface.as_c_str()),
                        version:   *version,
                    })
                    .await?;
                }
                for (id, _) in deleted.iter() {
                    ctx.connection().send(registry_id, wl_registry::events::GlobalRemove { name: *id })
                        .await?;
                }
            }
            if let Some((new_registry_objects, known_globals)) = send_new {
                for registry_id in new_registry_objects.into_iter() {
                    for (id, global) in known_globals.iter() {
                        let global = global.upgrade().unwrap();
                        let interface = std::ffi::CString::new(global.interface()).unwrap();
                        ctx.connection().send(registry_id, wl_registry::events::Global {
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
