//! Interfaces for wayland objects
//!
//! This module defines common interfaces for wayland objects. [`MonoObject`]
//! describes a object with a concrete type, whereas [`AnyObject`] describes a
//! type erased object, akin to `dyn Any`.
//!
//! Note these interfaces really only represents part of what a wayland object
//! does. These objects would also implement one of the `RequestDispatch` traits
//! generated from the wayland protocol specification, which will be used by the
//! [`Object`] trait to handle wayland requests.
//!
//! Normally, you wouldn't implement traits defined here by hand, but instead
//! use the [`#[derive(Object)]`](runa_macros::Object) or the
//! [`#[wayland_object]`](wayland_object) attribute to generate the impls for
//! you.
//!
//! Reference implementations of the core wayland objects: `wl_display`,
//! `wl_registry`, and `wl_callback` are also provided here.

use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};

use ::runa_wayland_protocols::wayland::{
    wl_callback, wl_display::v1 as wl_display, wl_registry::v1 as wl_registry,
};
use runa_io::traits::WriteMessage;
/// Generate `Object` impls for types that implement `RequestDispatch` for a
/// certain interface. Should be attached to `RequestDispatch` impls.
///
/// It deserialize a message from a deserializer, and calls appropriate function
/// in the `RequestDispatch` based on the message content. Your impl of
/// `RequestDispatch` should contains an error type that can be converted from
/// deserailization error.
///
/// # Arguments
///
/// * `message` - The message type. By default, this attribute try to cut the
///   "Dispatch" suffix from the trait name. i.e.
///   `wl_buffer::v1::RequestDispatch` will become `wl_buffer::v1::Request`.
/// * `interface` - The interface name. By default, this attribute finds the
///   parent of the `RequestDispatch` trait, i.e.
///   `wl_buffer::v1::RequestDispatch` will become `wl_buffer::v1`; then attach
///   `::NAME` to it as the interface.
/// * `on_disconnect` - The function to call when the client disconnects. Used
///   for the [`Object::on_disconnect`](crate::objects::Object::on_disconnect)
///   impl.
/// * `crate` - The path to the `runa_core` crate. "runa_core" by default.
/// * `state` - The type of the singleton state associated with the object. See
///   [`MonoObject::SingletonState`](crate::objects::MonoObject::SingletonState)
///   for more information. If not set, this will be a unit type (e.g. `()`). An
///   optional where clause can be added, which will be attached to the impl for
///   `MonoObject`.
/// * `state_init` - An expression to create the initial value of the state.
///   This expression must be evaluate-able in const context. By default, if
///   `state` is not set, this will be `()`; otherwise this will be
///   `Default::default`, which must be defined as an associated constant. Note
///   you don't need to wrap this in `Some`.
pub use runa_macros::wayland_object;
#[doc(no_inline)]
pub use runa_macros::Object;
use runa_wayland_types::{NewId, Str};
use tracing::debug;

use crate::{
    client::traits::{Client, ClientParts, Store},
    globals::{AnyGlobal, Bind},
    server::traits::{GlobalStore, Server},
};

/// A polymorphic object, i.e. it's an union of multiple objects types.
///
/// A trait for storing "type erased" objects. This trait is separated from the
/// bigger [`Object`] trait to keep it away from the pollution of the `Ctx`
/// parameter.
///
/// # Example
///
/// Normally you won't need to worry about implementing this trait, as this can
/// be generated automatically by [`#[derive(Object)]`](runa_macros::Object),
/// but here is an example of how to implement this trait.
///
/// ```rust
/// use std::any::Any;
///
/// use wl_server::objects::{AnyObject, MonoObject};
///
/// // Define two monomorphic objects
/// struct A;
/// impl MonoObject for A {
///     type SingletonState = usize;
///
///     const INTERFACE: &'static str = "A";
///
///     fn new_singleton_state() -> Option<Self::SingletonState> {
///         Some(0usize)
///     }
/// }
///
/// struct B;
/// impl MonoObject for B {
///     type SingletonState = ();
///
///     const INTERFACE: &'static str = "B";
///
///     fn new_singleton_state() -> Option<Self::SingletonState> {
///         Some(())
///     }
/// }
///
/// // Define a polymorphic object as the union of the two monomorphic objects
/// enum MyObject {
///     A(A),
///     B(B),
/// }
/// impl AnyObject for MyObject {
///     fn interface(&self) -> &'static str {
///         match self {
///             MyObject::A(_) => A::INTERFACE,
///             MyObject::B(_) => B::INTERFACE,
///         }
///     }
///
///     fn type_id(&self) -> std::any::TypeId {
///         match self {
///             MyObject::A(_) => std::any::TypeId::of::<A>(),
///             MyObject::B(_) => std::any::TypeId::of::<B>(),
///         }
///     }
///
///     fn cast<T: 'static>(&self) -> Option<&T> {
///         match self {
///             MyObject::A(a) => (a as &dyn Any).downcast_ref::<T>(),
///             MyObject::B(b) => (b as &dyn Any).downcast_ref::<T>(),
///         }
///     }
///
///     fn cast_mut<T: 'static>(&mut self) -> Option<&mut T> {
///         match self {
///             MyObject::A(a) => (a as &mut dyn Any).downcast_mut::<T>(),
///             MyObject::B(b) => (b as &mut dyn Any).downcast_mut::<T>(),
///         }
///     }
///
///     fn new_singleton_state(&self) -> Option<Box<dyn std::any::Any>> {
///         match self {
///             MyObject::A(_) =>
///                 <A as MonoObject>::new_singleton_state().map(|s| Box::new(s) as _),
///             MyObject::B(_) =>
///                 <B as MonoObject>::new_singleton_state().map(|s| Box::new(s) as _),
///         }
///     }
/// }
/// ```
///
/// Which is equivalent to:
///
/// ```ignore
/// #[derive(Object)]
/// enum MyObject {
///     A(A),
///     B(B),
/// }
/// ```
pub trait AnyObject: 'static + Sized {
    /// Return the interface name of the concrete object.
    fn interface(&self) -> &'static str;

    /// Cast the object into a more concrete type. This is generated by
    /// `#[derive(Object)]` for casting a enum of many object types to the
    /// concrete type. This should also work if `T == Self`.
    fn cast<T: 'static>(&self) -> Option<&T>;

    /// See [`AnyObject::cast`]
    fn cast_mut<T: 'static>(&mut self) -> Option<&mut T>;

    /// Generate the initial value for the singleton state, see
    /// [`MonoObject::SingletonState`]. If `None` is returned, there will be no
    /// state associated with this object type.
    ///
    /// # Note
    ///
    /// This concrete type of the returned value must be consistent with
    /// [`MonoObject::SingletonState`] for the `MonoObject` object contained in
    /// this `AnyObject`, otherwise the `Store` implementation might panic.
    ///
    /// i.e. if [`Self::type_id`] returns `std::any::TypeId::of::<A>()`, then
    /// this method must return `Box::new(<A as
    /// MonoObject>::new_singleton_state())`.
    ///
    /// You don't need to worry about this if you use `#[derive(Object)]`
    /// and `#[wayland_object]` macros to generate the implementation.
    fn new_singleton_state(&self) -> Box<dyn std::any::Any>;

    /// Type id of the concrete object type. If this is an enum of multiple
    /// object types, this should return the type id of the inhabited variant.
    fn type_id(&self) -> std::any::TypeId;
}

/// An monomorphic object, i.e. it's a single object whose interface is known,
/// as opposed to [`AnyObject`].
///
/// This is deliberately separate from [`Object`] to keep it away from the
/// pollution of the `Ctx` parameter.
///
/// This trait is automatically derived by the `#[wayland_object]` macro.
///
/// # Note
///
/// If the object is a proxy of a global, it has to recognize if the global's
/// lifetime has ended, and turn all message sent to it to no-ops. This can
/// often be achieved by holding a Weak reference to the global object.
pub trait MonoObject: 'static {
    /// A singleton state associated with the object type. This state is
    /// associated with the type, so there is only one instance of
    /// the state for all objects of the same type. The lifetime of this
    /// state is managed by the object store, and it will be dropped when
    /// the last object of the type is dropped.
    type SingletonState: 'static;

    /// Create a new instance of the singleton state.
    fn new_singleton_state() -> Self::SingletonState;

    /// The wayland interface implemented by this object.
    const INTERFACE: &'static str;
}

/// A wayland object.
///
/// An object can either be a [`MonoObject`] or an [`AnyObject`].
///
/// A `MonoObject` would be an object that is known to be of a single type,
/// and it will have a manually implemented `RequestDispatch` trait. Its
/// `Object` trait implementation can be generated from the `RequestDispatch`
/// implementation with the help of the `#[wayland_object]` macro.
///
/// An `AnyObject` can be seen as a union of multiple `MonoObject` types. Its
/// `Object` trait implementation can be generated using the
/// [`#[derive(Object)]`](runa_macros::Object) macro.
pub trait Object<Ctx: crate::client::traits::Client>: 'static {
    /// The type of wayland messages that this object can receive.
    /// This is what the [`dispatch`](Self::dispatch) method accepts.
    type Request<'a>: runa_io::traits::de::Deserialize<'a>
    where
        Ctx: 'a;

    /// Error returned by the [`dispatch`](Self::dispatch) method.
    type Error: crate::error::ProtocolError;

    /// The future type returned by the [`dispatch`](Self::dispatch) method.
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
        _state: &mut dyn std::any::Any,
    ) {
    }
    /// Dispatch a wayland request to this object. Returns a future,
    /// that resolves to (Result, usize, usize), which are the result of the
    /// request, the number of bytes and file descriptors in the request,
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

    fn sync(ctx: &mut Ctx, object_id: u32, callback: NewId) -> Self::SyncFut<'_> {
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

    fn get_registry(ctx: &mut Ctx, object_id: u32, registry: NewId) -> Self::GetRegistryFut<'_> {
        assert!(ctx.objects().get::<Self>(object_id).unwrap().initialized);
        async move {
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
            let inserted = objects
                .try_insert_with(registry.0, || global.new_object())
                .is_some();
            if inserted {
                global.bind(ctx, registry.0).await?;
                Ok(())
            } else {
                Err(crate::error::Error::IdExists(registry.0))
            }
        }
    }
}

/// Default wl_registry implementation
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
        _interface: Str<'a>,
        _version: u32,
        id: NewId,
    ) -> Self::BindFut<'a> {
        // TODO: remember the version requested by the client, and adjust objects
        // behavior accordingly.
        tracing::debug!("bind name:{name}, id:{id}");
        async move {
            let ClientParts {
                server_context,
                objects,
                ..
            } = ctx.as_mut_parts();
            let global = server_context.globals().borrow().get(name).cloned();
            if let Some(global) = global {
                let inserted = objects
                    .try_insert_with(id.0, || global.new_object())
                    .is_some();

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

/// Default wl_callback implementation
#[derive(Debug, Default, Clone, Copy)]
pub struct Callback {
    fired: bool,
}
impl MonoObject for Callback {
    type SingletonState = ();

    const INTERFACE: &'static str = runa_wayland_protocols::wayland::wl_callback::v1::NAME;

    fn new_singleton_state() {}
}

impl<Ctx: crate::client::traits::Client> Object<Ctx> for Callback {
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
                runa_wayland_protocols::wayland::wl_callback::v1::events::Done {
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
            runa_wayland_protocols::wayland::wl_callback::v1::events::Done {
                callback_data: data,
            },
        )
        .await?;
        objects.remove(object_id).unwrap();
        Ok(())
        // store unlocked here
    }
}
