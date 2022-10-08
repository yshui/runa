use std::{future::Future, os::unix::io::OwnedFd};

use wl_common::{interface_message_dispatch, Infallible};
use wl_protocol::wayland::{
    wl_display::v1 as wl_display, wl_shm::v1 as wl_shm, wl_shm_pool::v1 as wl_shm_pool,
};
use wl_server::{
    connection::{Connection, Objects},
    error,
    objects::{InterfaceMeta, DISPLAY_ID},
    provide_any,
    renderer_capability::RendererCapability,
    server::{Server, ServerBuilder},
};

pub struct Shm;
impl Shm {
    pub const EVENT_SLOT: i32 = -1;

    pub fn new() -> Shm {
        Shm
    }

    pub fn init_server<Ctx: ServerBuilder>(server: &mut Ctx) -> Result<(), Infallible>
    where
        Ctx::Output: RendererCapability,
        std::io::Error: From<<<Ctx::Output as Server>::Connection as Connection>::Error>,
    {
        server.global(crate::globals::Shm);
        Ok(())
    }

    pub async fn handle_events<Ctx>(
        _ctx: &Ctx,
        _slot: usize,
        _event: &'static str,
    ) -> Result<(), Infallible> {
        Ok(())
    }
}
impl InterfaceMeta for Shm {
    fn interface(&self) -> &'static str {
        wl_shm::NAME
    }

    fn provide<'a>(&'a self, demand: &mut provide_any::Demand<'a>) {
        demand.provide_ref(self);
    }
}

// TODO: Add a trait for ShmPool and make Shm generic over the pool type.

#[interface_message_dispatch]
impl<Ctx> wl_shm::RequestDispatch<Ctx> for Shm
where
    Ctx: Objects + Connection,
    error::Error: From<Ctx::Error>,
{
    type Error = error::Error;

    type CreatePoolFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn create_pool<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
        mut fd: wl_types::Fd,
        size: i32,
    ) -> Self::CreatePoolFut<'a> {
        async move {
            if size <= 0 {
                ctx.send(
                    DISPLAY_ID,
                    wl_display::Event::Error(wl_display::events::Error {
                        code:      wl_shm::enums::Error::InvalidStride as u32,
                        object_id: object_id.into(),
                        message:   wl_types::str!("invalid size"),
                    }),
                )
                .await?;
                return Ok(())
            }
            let fd = unsafe {
                fd.assume_owned();
                fd.take().unwrap_unchecked()
            };
            let pool = ShmPool { fd };
            if ctx.insert(id.0, pool).is_err() {
                ctx.send(
                    DISPLAY_ID,
                    wl_display::Event::Error(wl_display::events::Error {
                        code:      wl_display::enums::Error::InvalidObject as u32,
                        object_id: object_id.into(),
                        message:   wl_types::str!("id already in use"),
                    }),
                )
                .await?;
            }
            Ok(())
        }
    }
}

pub struct ShmPool {
    fd: OwnedFd,
}

impl InterfaceMeta for ShmPool {
    fn interface(&self) -> &'static str {
        wl_shm_pool::NAME
    }

    fn provide<'a>(&'a self, demand: &mut provide_any::Demand<'a>) {
        demand.provide_ref(self);
    }
}

#[interface_message_dispatch]
impl<Ctx> wl_shm_pool::RequestDispatch<Ctx> for ShmPool {
    type Error = error::Error;

    type CreateBufferFut<'a> = impl Future<Output = Result<(), error::Error>> + 'a where Ctx: 'a;
    type DestroyFut<'a> = impl Future<Output = Result<(), error::Error>> + 'a where Ctx: 'a;
    type ResizeFut<'a> = impl Future<Output = Result<(), error::Error>> + 'a where Ctx: 'a;

    fn resize<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32, size: i32) -> Self::ResizeFut<'a> {
        async move { unimplemented!() }
    }

    fn create_buffer<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
        offset: i32,
        width: i32,
        height: i32,
        stride: i32,
        format: wl_protocol::wayland::wl_shm::v1::enums::Format,
    ) -> Self::CreateBufferFut<'a> {
        async move { unimplemented!() }
    }

    fn destroy<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        async move { unimplemented!() }
    }
}

impl ShmPool {
    pub const EVENT_SLOT: i32 = -1;

    pub fn init_server<Ctx: ServerBuilder>(_server: &mut Ctx) -> Result<(), Infallible> {
        Ok(())
    }

    pub async fn handle_events<Ctx>(
        _ctx: &Ctx,
        _slot: usize,
        _event: &'static str,
    ) -> Result<(), Infallible> {
        Ok(())
    }
}
