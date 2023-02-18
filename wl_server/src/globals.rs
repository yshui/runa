use std::future::Future;

use futures_util::{pin_mut, FutureExt};
use wl_protocol::wayland::{wl_display, wl_registry::v1 as wl_registry};

use crate::{
    connection::{
        traits::{LockableStore, Store},
        Client, WriteMessage,
    },
    events::EventSource,
    objects::ObjectMeta,
    server::{GlobalsUpdate, Server},
};

pub trait GlobalMeta {
    type Object;
    /// Create a proxy of this global, called when the client tries to bound
    /// the global.
    fn new_object(&self) -> Self::Object;
    fn interface(&self) -> &'static str;
    fn version(&self) -> u32;
}

pub trait Bind<Ctx>: GlobalMeta {
    type BindFut<'a>: Future<Output = std::io::Result<()>> + 'a
    where
        Ctx: 'a,
        Self: 'a;
    /// Setup a proxy of this global, the proxy object has already been inserted
    /// into the client's object store, with the given object id.
    ///
    /// If Err() is returned, an protocol error will be sent to the client, and
    /// the client will be disconnected.
    fn bind<'a>(&'a self, client: &'a mut Ctx, object_id: u32) -> Self::BindFut<'a>;
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

impl GlobalMeta for Display {
    type Object = crate::objects::Display;

    fn interface(&self) -> &'static str {
        wl_display::v1::NAME
    }

    fn version(&self) -> u32 {
        wl_display::v1::VERSION
    }

    fn new_object(&self) -> Self::Object {
        crate::objects::Display
    }
}

impl<Ctx> Bind<Ctx> for Display {
    type BindFut<'a > = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a, Ctx: 'a;

    fn bind<'a>(&'a self, _client: &'a mut Ctx, _object_id: u32) -> Self::BindFut<'a> {
        futures_util::future::ok(())
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

impl GlobalMeta for Registry {
    type Object = crate::objects::Registry;

    fn interface(&self) -> &'static str {
        wl_registry::NAME
    }

    fn version(&self) -> u32 {
        wl_registry::VERSION
    }

    fn new_object(&self) -> Self::Object {
        crate::objects::Registry(None)
    }
}

impl<Ctx: Client> Bind<Ctx> for Registry {
    type BindFut<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a, Ctx: 'a;

    fn bind<'a>(&'a self, client: &'a mut Ctx, object_id: u32) -> Self::BindFut<'a> {
        async move {
            let rx = client.server_context().globals().borrow().subscribe();
            let (fut, handle) = futures_util::future::abortable(handle_registry_events(
                object_id,
                client.server_context().clone(),
                client.connection().clone(),
                rx,
            ));
            client.spawn(fut.map(|_| ()));
            let objects = client.objects();
            let mut objects = objects.lock().await;
            let object = objects
                .get_mut(object_id)
                .unwrap()
                .cast_mut::<Self::Object>()
                .unwrap();
            object.0 = Some(handle);
            Ok(())
        }
    }
}

async fn handle_registry_events<SC: Server, C: WriteMessage>(
    registry_id: u32,
    server_context: SC,
    connection: C,
    rx: impl futures_util::Stream<Item = GlobalsUpdate<<SC as Server>::Global>>,
) {
    use futures_lite::StreamExt as _;

    // Allocation: Global addition and removal should be rare.
    use crate::server::Globals;
    // Send existing globals
    let globals: Vec<_> = server_context
        .globals()
        .borrow()
        .iter()
        .map(|(id, global)| (id, global.clone()))
        .collect();

    for (id, global) in globals {
        let interface = std::ffi::CString::new(global.interface()).unwrap();
        let version = global.version();
        connection
            .send(registry_id, wl_registry::events::Global {
                name: id,
                interface: wl_types::Str(interface.as_c_str()),
                version,
            })
            .await
            .unwrap()
    }
    pin_mut!(rx);
    loop {
        match rx.next().await.unwrap() {
            GlobalsUpdate::Removed(name) => {
                connection
                    .send(registry_id, wl_registry::events::GlobalRemove { name })
                    .await
                    .unwrap();
            },
            GlobalsUpdate::Added(name, global) => {
                let interface = std::ffi::CString::new(global.interface()).unwrap();
                let version = global.version();
                connection
                    .send(registry_id, wl_registry::events::Global {
                        name,
                        interface: wl_types::Str(interface.as_c_str()),
                        version,
                    })
                    .await
                    .unwrap();
            },
        }
    }
}
