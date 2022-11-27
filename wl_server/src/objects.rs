//! These are a set of objects the client can acquire. These objects typically
//! implement the RequestDispatch trait of one of the wayland interfaces. As
//! well as a `InterfaceMeta` trait to provide information about the interface,
//! and allowing them to be cast into trait objects and stored together.

use std::future::Future;

pub use ::wl_common::{wayland_object, Object};
use ::wl_protocol::wayland::{
    wl_callback, wl_display::v1 as wl_display, wl_registry::v1 as wl_registry,
};
use tracing::debug;
use wl_common::Infallible;

use crate::{
    connection::{ClientContext, Objects, State},
    globals::Bind,
    server::{self, GlobalOf, Globals},
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
pub trait Object<Ctx> {
    type Request<'a>: wl_io::traits::de::Deserialize<'a>
    where
        Ctx: 'a,
        Self: 'a;
    type Error;
    type Fut<'a>: Future<Output = (Result<(), Self::Error>, usize, usize)> + 'a
    where
        Ctx: 'a,
        Self: 'a;
    /// Return the interface name of this object.
    fn interface(&self) -> &'static str;
    /// A function that will be called when the client disconnects. It should
    /// free up allocated resources if any. This function should not try to
    /// send anything to the client, as it has already disconnected.
    ///
    /// The context object is passed as a `dyn Any` to make this function object
    /// safe.
    fn on_disconnect(&mut self, _ctx: &mut Ctx) {}
    fn cast<T: 'static>(&self) -> Option<&T>
    where
        Self: 'static + Sized,
    {
        (self as &dyn std::any::Any).downcast_ref()
    }
    /// Dispatch requests to the interface implementation. Returns a future, that resolves
    /// to (Result, usize, usize), which are the result of the request, the number of bytes
    /// and file descriptors in the message, respectively.
    fn dispatch<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        msg: Self::Request<'a>,
    ) -> Self::Fut<'a>;
}

/// The object ID of the wl_display object
pub const DISPLAY_ID: u32 = 1;

/// Default wl_display implementation
#[derive(Debug, Clone, Copy)]
pub struct Display;

#[wayland_object(crate = "crate")]
impl<Ctx> wl_display::RequestDispatch<Ctx> for Display
where
    Ctx: ClientContext + std::fmt::Debug,
    crate::globals::Registry: Bind<Ctx>,
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
                    .insert(registry.0, object)
                    .unwrap();
                Ok(())
            } else {
                Err(crate::error::Error::IdExists(registry.0))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Registry;

mod private {
    use std::rc::Weak;

    use derivative::Derivative;
    use hashbrown::{HashMap, HashSet};

    /// Set of object_ids that are wl_registry objects, so we know which objects
    /// to send the events from.
    #[derive(Derivative)]
    #[derivative(Default(bound = ""))]
    pub struct RegistryState<G> {
        /// Known wl_registry objects. We will send delta global events to them.
        pub(crate) registry_objects:     HashSet<u32>,
        /// Newly created wl_registry objects. We will send all the globals to
        /// them then move them to `registry_objects`.
        pub(crate) new_registry_objects: Vec<u32>,
        /// List of globals known to this client context
        pub(crate) known_globals:        HashMap<u32, Weak<G>>,
    }

    impl<G> RegistryState<G> {
        pub(crate) fn add_registry_object(&mut self, id: u32) {
            self.new_registry_objects.push(id);
        }
    }
    impl<G> std::fmt::Debug for RegistryState<G> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            struct DebugMap<'a, K, V>(&'a HashMap<K, V>);
            impl<'a, K, V> std::fmt::Debug for DebugMap<'a, K, V>
            where
                K: std::fmt::Debug,
            {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.debug_set().entries(self.0.keys()).finish()
                }
            }
            f.debug_struct("RegistryState")
                .field("registry_objects", &self.registry_objects)
                .field("new_registry_objects", &self.new_registry_objects)
                .field("known_globals", &DebugMap(&self.known_globals))
                .finish()
        }
    }
}

pub(crate) use private::RegistryState;

#[wayland_object(crate = "crate")]
impl<Ctx> wl_registry::RequestDispatch<Ctx> for Registry
where
    Ctx: ClientContext + State<RegistryState<GlobalOf<Ctx>>>,
{
    type Error = crate::error::Error;

    type BindFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn bind<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        _object_id: u32,
        name: u32,
        _interface: wl_types::Str<'a>,
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
            let state = ctx.state();
            let empty = Default::default();
            let known_globals = state.map(|s| &s.known_globals).unwrap_or(&empty);
            if ctx.objects().borrow().get(id.0).is_some() {
                return Err(crate::error::Error::IdExists(id.0))
            }
            let global = known_globals.get(&name).and_then(|g| g.upgrade());
            if let Some(global) = global {
                let object = global.bind(ctx, id.0).await?;
                ctx.objects().borrow_mut().insert(id.0, object).unwrap(); // can't fail
                Ok(())
            } else {
                Err(crate::error::Error::UnknownGlobal(name))
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct Callback;
impl<Ctx> Object<Ctx> for Callback {
    type Error = crate::error::Error;
    type Request<'a> = Infallible where Ctx: 'a;

    type Fut<'a> = impl Future<Output = (Result<(), Self::Error>, usize, usize)> + 'a where Ctx: 'a, Self: 'a;

    fn interface(&self) -> &'static str {
        wl_protocol::wayland::wl_callback::v1::NAME
    }

    fn dispatch<'a>(
        &'a self,
        _ctx: &'a mut Ctx,
        _object_id: u32,
        msg: Self::Request<'a>,
    ) -> Self::Fut<'a> {
        async move { match msg {} }
    }
}

impl Callback {
    pub async fn fire(object_id: u32, data: u32, ctx: &impl ClientContext) -> std::io::Result<()> {
        ctx.send(
            object_id,
            wl_protocol::wayland::wl_callback::v1::events::Done {
                callback_data: data,
            },
        )
        .await?;
        let callback = ctx.objects().borrow().get(object_id).unwrap().clone();
        let interface = callback.interface();
        let Some(_): Option<&Self> = callback.cast() else {
            panic!("object is not callback, it's {}", interface)
        };
        ctx.objects().borrow_mut().remove(object_id).unwrap();
        ctx.send(DISPLAY_ID, wl_display::events::DeleteId { id: object_id })
            .await?;
        Ok(())
    }
}
