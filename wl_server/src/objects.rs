//! These are a set of objects the client can acquire. These objects typically
//! implement the RequestDispatch trait of one of the wayland interfaces. As
//! well as a `InterfaceMeta` trait to provide information about the interface,
//! and allowing them to be cast into trait objects and stored together.

use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};

use ::wl_protocol::wayland::{
    wl_callback, wl_display::v1 as wl_display, wl_registry::v1 as wl_registry,
};
use tracing::debug;
use wl_io::traits::WriteMessage;
pub use wl_macros::{wayland_object, Object};

use crate::{
    connection::traits::{Client, ClientParts, Store},
    globals::{Bind, GlobalMeta},
    server::{self, Globals},
};

/// A trait for storing type erased objects. This trait is separated from the
/// bigger [`Object`] trait to keep it object safe, and away from the pollution
/// of the `Ctx` parameter.
pub trait AnyObject: 'static + Sized {
    /// Return the interface name of this object.
    fn interface(&self) -> &'static str;

    /// Cast the object into a more concrete type. This is generated by
    /// `#[derive(Object)]` for casting a enum of many object types to the
    /// concrete type.
    #[inline]
    fn cast<T: 'static>(&self) -> Option<&T> {
        (self as &dyn std::any::Any).downcast_ref()
    }

    /// See [`AnyObject::cast`]
    #[inline]
    fn cast_mut<T: 'static>(&mut self) -> Option<&mut T> {
        (self as &mut dyn std::any::Any).downcast_mut()
    }

    /// Generate the initial value for the singleton state, see
    /// [`MonoObject::SingletonState`]. If `None` is returned, there will be no
    /// state associated with this object type.
    ///
    /// # Note
    ///
    /// This concrete type of the returned value must be consistent with
    /// [`MonoObject::SingletonState`], otherwise the `Store` implementation
    /// might panic. You don't need to worry about this if you use
    /// `#[derive(Object)]` and `#[wayland_object]` macros to generate the
    /// implementation.
    fn singleton_state(&self) -> Option<Box<dyn std::any::Any>>;

    /// Type id of the concrete object type. If this is an enum of multiple
    /// object types, this should return the type id of the inhabited variant.
    #[inline]
    fn type_id(&self) -> std::any::TypeId {
        std::any::Any::type_id(self)
    }
}

/// An monomorphic object, i.e. it's a single object whose interface is known,
/// not an union of multiple objects. This is deliberately separate from
/// ObjectMeta so that could be object safe.
pub trait MonoObject: AnyObject {
    /// A singleton state associated with the object type. This state is
    /// associated with the type, so there is only one instance of
    /// the state for all objects of the same type. The lifetime of this
    /// state is managed by the object store, and it will be dropped when
    /// the last object of the type is dropped.
    type SingletonState: 'static;

    const INTERFACE: &'static str;
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
pub trait Object<Ctx: crate::connection::traits::Client>: AnyObject {
    type Request<'a>: wl_io::traits::de::Deserialize<'a>
    where
        Ctx: 'a;
    type Error: wl_protocol::ProtocolError;
    type Fut<'a>: Future<Output = (Result<(), Self::Error>, usize, usize)> + 'a
    where
        Ctx: 'a;
    /// A function that will be called when the client disconnects. It should
    /// free up allocated resources if any. This function only gets reference to
    /// the server context, because:
    ///
    ///   - It cannot send anything to the client, as it has already
    ///     disconnected. So no `Ctx::Connection`.
    ///   - The object store will be borrowed mutably when this is called, to
    ///     iterate over all objects and calling their on_disconnect. So no
    ///     `Ctx::ObjectStore`.
    ///   - Adding more event handlers at this point doesn't make sense. So no
    ///     `Ctx::EventDispatcher`.
    ///
    /// This function also gets access to the singleton state associated with
    /// this object type, if there is any.
    fn on_disconnect(
        &mut self,
        _server_ctx: &mut Ctx::ServerContext,
        _state: Option<&mut dyn std::any::Any>,
    ) {
    }
    /// Dispatch requests to the interface implementation. Returns a future,
    /// that resolves to (Result, usize, usize), which are the result of the
    /// request, the number of bytes and file descriptors in the message,
    /// respectively.
    fn dispatch<'a>(ctx: &'a mut Ctx, object_id: u32, msg: Self::Request<'a>) -> Self::Fut<'a>;
}

/// The object ID of the wl_display object
pub const DISPLAY_ID: u32 = 1;

/// Default wl_display implementation
#[derive(Debug, Clone, Copy, Default)]
pub struct Display {
    pub(crate) initialized: bool,
}

#[wayland_object(crate = "crate")]
impl<Ctx> wl_display::RequestDispatch<Ctx> for Display
where
    Ctx: Client + std::fmt::Debug,
    Ctx::Object: From<Self>,
{
    type Error = crate::error::Error;

    type GetRegistryFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;
    type SyncFut<'a> = impl std::future::Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn sync(ctx: &mut Ctx, object_id: u32, callback: wl_types::NewId) -> Self::SyncFut<'_> {
        assert!(ctx.objects().get::<Self>(object_id).unwrap().initialized);
        async move {
            debug!("wl_display.sync {}", callback);
            let objects = ctx.objects();
            if objects.contains(callback.0) {
                return Err(crate::error::Error::IdExists(callback.0))
            }
            let conn = ctx.connection_mut();
            conn.send(callback.0, wl_callback::v1::events::Done {
                // TODO: setup event serial
                callback_data: 0,
            })
            .await?;
            // We never inserted this object into the store, so we have to send DeleteId
            // manually.
            conn.send(DISPLAY_ID, wl_display::events::DeleteId { id: callback.0 })
                .await?;
            Ok(())
        }
    }

    fn get_registry(
        ctx: &mut Ctx,
        object_id: u32,
        registry: wl_types::NewId,
    ) -> Self::GetRegistryFut<'_> {
        assert!(ctx.objects().get::<Self>(object_id).unwrap().initialized);
        async move {
            use server::Server;
            debug!("wl_display.get_registry {}", registry);
            let ClientParts {
                server_context,
                objects,
                ..
            } = ctx.as_mut_parts();
            let global = {
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
            let inserted = objects.try_insert_with(registry.0, || global.new_object());
            drop(objects); // unlock the object store
            if inserted {
                global.bind(ctx, registry.0).await?;
                Ok(())
            } else {
                Err(crate::error::Error::IdExists(registry.0))
            }
        }
    }
}

#[derive(Debug)]
pub struct Registry(pub(crate) Option<futures_util::future::AbortHandle>);

impl Drop for Registry {
    fn drop(&mut self) {
        if let Some(abort) = self.0.take() {
            abort.abort();
        }
    }
}

#[wayland_object(crate = "crate")]
impl<Ctx> wl_registry::RequestDispatch<Ctx> for Registry
where
    Ctx: Client + std::fmt::Debug,
{
    type Error = crate::error::Error;

    type BindFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn bind<'a>(
        ctx: &'a mut Ctx,
        _object_id: u32,
        name: u32,
        _interface: wl_types::Str<'a>,
        _version: u32,
        id: wl_types::NewId,
    ) -> Self::BindFut<'a> {
        tracing::debug!("bind name:{name}, id:{id}");
        async move {
            use crate::server::Server;
            let ClientParts {
                server_context,
                objects,
                ..
            } = ctx.as_mut_parts();
            let global = server_context.globals().borrow().get(name).cloned();
            if let Some(global) = global {
                let inserted = objects.try_insert_with(id.0, || global.new_object());

                if !inserted {
                    Err(crate::error::Error::IdExists(id.0))
                } else {
                    global.bind(ctx, id.0).await?;
                    Ok(())
                }
            } else {
                Err(crate::error::Error::UnknownGlobal(name))
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct Callback {
    fired: bool,
}
impl MonoObject for Callback {
    type SingletonState = ::std::convert::Infallible;

    const INTERFACE: &'static str = wl_protocol::wayland::wl_callback::v1::NAME;
}

impl AnyObject for Callback {
    fn interface(&self) -> &'static str {
        wl_protocol::wayland::wl_callback::v1::NAME
    }

    fn singleton_state(&self) -> Option<Box<dyn std::any::Any>> {
        None
    }
}
impl<Ctx: crate::connection::traits::Client> Object<Ctx> for Callback {
    type Error = crate::error::Error;
    type Request<'a> = Infallible where Ctx: 'a;

    type Fut<'a> = impl Future<Output = (Result<(), Self::Error>, usize, usize)> + 'a where Ctx: 'a;

    fn dispatch<'a>(_ctx: &'a mut Ctx, _object_id: u32, msg: Self::Request<'a>) -> Self::Fut<'a> {
        async move { match msg {} }
    }
}

impl Callback {
    /// Fire the callback and remove it from object store.
    pub fn poll_fire<O: AnyObject + 'static>(
        cx: &mut Context<'_>,
        object_id: u32,
        data: u32,
        objects: &mut impl Store<O>,
        mut conn: Pin<&mut impl WriteMessage>,
    ) -> Poll<std::io::Result<()>> {
        let this = objects.get_mut::<Self>(object_id).unwrap();
        if !this.fired {
            ready!(conn.as_mut().poll_ready(cx))?;
            conn.as_mut().start_send(
                object_id,
                wl_protocol::wayland::wl_callback::v1::events::Done {
                    callback_data: data,
                },
            );
            this.fired = true;
        }

        ready!(conn.as_mut().poll_ready(cx))?;
        objects.remove(object_id).unwrap();
        Poll::Ready(Ok(()))
    }

    /// Fire the callback and remove it from object store.
    pub async fn fire<'a, O: AnyObject + 'static>(
        object_id: u32,
        data: u32,
        objects: &mut impl Store<O>,
        conn: &mut (impl WriteMessage + Unpin),
    ) -> std::io::Result<()> {
        objects.get::<Self>(object_id).unwrap();
        conn.send(
            object_id,
            wl_protocol::wayland::wl_callback::v1::events::Done {
                callback_data: data,
            },
        )
        .await?;
        objects.remove(object_id).unwrap();
        Ok(())
        // store unlocked here
    }
}
