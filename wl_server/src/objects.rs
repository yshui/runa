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

pub trait ObjectMeta {
    /// Return the interface name of this object.
    fn interface(&self) -> &'static str;

    /// Cast the object into a more concrete type. This is generated by
    /// `#[derive(Object)]` when used on a `enum` of many objects.
    fn cast<T: 'static>(&self) -> Option<&T>
    where
        Self: 'static + Sized,
    {
        (self as &dyn std::any::Any).downcast_ref()
    }

    /// See [`ObjectMeta::cast`]
    fn cast_mut<T: 'static>(&mut self) -> Option<&mut T>
    where
        Self: 'static + Sized,
    {
        (self as &mut dyn std::any::Any).downcast_mut()
    }
}

/// An monomorphic object, i.e. it's a single object whose interface is known,
/// not an union of multiple objects.
pub trait StaticObjectMeta: ObjectMeta {
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
pub trait Object<Ctx>: ObjectMeta {
    type Request<'a>: wl_io::traits::de::Deserialize<'a>
    where
        Ctx: 'a;
    type Error: wl_protocol::ProtocolError;
    type Fut<'a>: Future<Output = (Result<(), Self::Error>, usize, usize)> + 'a
    where
        Ctx: 'a;
    /// A function that will be called when the client disconnects. It should
    /// free up allocated resources if any. This function should not try to
    /// send anything to the client, as it has already disconnected.
    ///
    /// The context object is passed as a `dyn Any` to make this function object
    /// safe.
    fn on_disconnect(&mut self, _ctx: &mut Ctx) {}
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
        assert!(
            ctx.objects()
                .get(object_id)
                .unwrap()
                .cast::<Self>()
                .unwrap()
                .initialized
        );
        async move {
            debug!("wl_display.sync {}", callback);
            let objects = ctx.objects();
            if objects.get(callback.0).is_some() {
                return Err(crate::error::Error::IdExists(callback.0))
            }
            let conn = ctx.connection_mut();
            conn.send(
                callback.0,
                wl_callback::v1::Event::Done(wl_callback::v1::events::Done {
                    // TODO: setup event serial
                    callback_data: 0,
                }),
            )
            .await?;
            // We never inserted this object into the store, so we have to send DeleteId
            // manually.
            conn.send(
                DISPLAY_ID,
                wl_display::events::DeleteId { id: callback.0 },
            )
            .await?;
            Ok(())
        }
    }

    fn get_registry(
        ctx: &mut Ctx,
        object_id: u32,
        registry: wl_types::NewId,
    ) -> Self::GetRegistryFut<'_> {
        assert!(
            ctx.objects()
                .get(object_id)
                .unwrap()
                .cast::<Self>()
                .unwrap()
                .initialized
        );
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
impl ObjectMeta for Callback {
    fn interface(&self) -> &'static str {
        wl_protocol::wayland::wl_callback::v1::NAME
    }
}
impl<Ctx> Object<Ctx> for Callback {
    type Error = crate::error::Error;
    type Request<'a> = Infallible where Ctx: 'a;

    type Fut<'a> = impl Future<Output = (Result<(), Self::Error>, usize, usize)> + 'a where Ctx: 'a;

    fn dispatch<'a>(_ctx: &'a mut Ctx, _object_id: u32, msg: Self::Request<'a>) -> Self::Fut<'a> {
        async move { match msg {} }
    }
}

impl Callback {
    /// Fire the callback and remove it from object store.
    pub fn poll_fire<O: ObjectMeta + 'static>(
        cx: &mut Context<'_>,
        object_id: u32,
        data: u32,
        objects: &mut impl Store<O>,
        mut conn: Pin<&mut impl WriteMessage>,
    ) -> Poll<std::io::Result<()>> {
        let obj = objects.get_mut(object_id).unwrap();
        let interface = obj.interface();
        let Some(this) = obj.cast_mut::<Self>() else {
            panic!("object is not callback, it's {interface}")
        };
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
    pub async fn fire<'a, O: ObjectMeta + 'static>(
        object_id: u32,
        data: u32,
        objects: &mut impl Store<O>,
        conn: &mut (impl WriteMessage + Unpin),
    ) -> std::io::Result<()> {
        let obj = objects.get(object_id).unwrap();
        let interface = obj.interface();
        let Some(_): Option<&Self> = obj.cast() else {
            panic!("object is not callback, it's {interface}")
        };
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
