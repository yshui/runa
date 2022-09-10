use futures_lite::pin;
use std::{ffi::CStr, pin::Pin, task::Poll};
use wayland::wl_display::v1::{events, requests, Event, EventAccessor, Request, RequestAccessor};
use wl_scanner_macros::generate_protocol;
use wl_scanner_support::io::Deserialize;
use wl_scanner_support::wayland_types::NewId;
use wl_scanner_support::{
    io::{
        utils::{ReadPool, WritePool},
        Serialize,
    },
    wayland_types::{Object, Str},
};

generate_protocol!("protocols/wayland.xml");

#[test]
fn test_serialize() {
    let mut item = Event::Error(events::Error {
        object_id: Object(1),
        code: 2,
        message: Str(CStr::from_bytes_with_nul(b"test\0").unwrap()),
    });
    let mut tx = WritePool::new();
    futures_executor::block_on(Pin::new(&mut item).serialize(Pin::new(&mut tx))).unwrap();
    let (buf, fds) = tx.into_inner();
    eprintln!("{:x?}", buf);

    let mut rx = ReadPool::new(buf, fds);
    let item: EventAccessor<'_, _> = futures_executor::block_on(
        ::wl_scanner_support::future::poll_map_fn(Pin::new(&mut rx), |rx, cx| {
            Deserialize::poll_deserialize(rx, cx)
        }),
    )
    .unwrap();
    eprintln!("{:?}", item);
}
