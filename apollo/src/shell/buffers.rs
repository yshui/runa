use std::future::Future;

use wl_common::interface_message_dispatch;
use wl_protocol::wayland::wl_buffer::v1 as wl_buffer;
use wl_server::{
    objects::InterfaceMeta,
    provide_any::{Demand, Provider},
};

pub trait Buffer {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>);
}

impl Provider for dyn Buffer {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        self.provide(demand);
    }
}

/// Buffer base
///
/// Various buffer implementations can choose to provide this struct as the
/// implementation of the wl_buffer interface.
///
/// All buffer implementations in this crate uses this.
pub struct BufferBase;

#[interface_message_dispatch]
impl<Ctx> wl_buffer::RequestDispatch<Ctx> for BufferBase {
    type Error = wl_server::error::Error;

    type DestroyFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn destroy<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        // TODO: Send release event
        async move { unimplemented!() }
    }
}

pub struct ShmBuffer<Data> {
    base: BufferBase,
    data: Data,
}

impl<Data: 'static> InterfaceMeta for ShmBuffer<Data> {
    fn interface(&self) -> &'static str {
        wl_buffer::NAME
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
        demand.provide_ref(&self.base);
    }
}
