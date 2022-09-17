use futures_lite::io::AsyncWrite;
use futures_lite::io::AsyncWriteExt;
use std::task::Context;
use std::{ffi::CStr, pin::Pin};
use wl_macros::generate_protocol;
use wl_scanner_support::io::de::WaylandBufAccess;
use wl_scanner_support::serde::Deserialize;
use wl_scanner_support::wayland_types::{Fd, NewId};
use wl_scanner_support::{
    future::poll_map_fn,
    io::{
        utils::{ReadPool, WritePool},
        Serialize,
    },
    wayland_types::{Object, Str},
};

generate_protocol!("protocols/wayland.xml");

#[test]
fn test_roundtrip() {
    use wayland::wl_display::v1::{events, Event};
    futures_executor::block_on(async {
        let mut orig_item = Event::Error(events::Error {
            object_id: Object(1),
            code: 2,
            message: Str(CStr::from_bytes_with_nul(b"test\0").unwrap()),
        });
        let mut tx = WritePool::new();
        // Put an object id
        tx.write(&[0, 0, 0, 0]).await.unwrap();
        Pin::new(&mut orig_item)
            .serialize(Pin::new(&mut tx))
            .await
            .unwrap();
        let (buf, fds) = tx.into_inner();

        let mut rx = ReadPool::new(buf, fds);
        let (_object_id, _opcode, mut accessor) =
            WaylandBufAccess::next_message(&mut rx).await.unwrap();
        let item = Event::deserialize(&mut accessor).unwrap();
        assert_eq!(&item, &orig_item);

        // Making sure dropping the accessor advance the buffer.
        drop(item);
        drop(accessor);
        assert!(rx.is_eof());
    })
}

#[test]
fn test_roundtrip_with_fd() {
    use wayland::wl_shm::v1::{requests, Request};
    futures_executor::block_on(async {
        let mut orig_item = Request::CreatePool(requests::CreatePool {
            id: NewId(1),
            fd: Fd::Raw(2),
            size: 100,
        });
        let mut tx = WritePool::new();
        // Put an object id
        Pin::new(&mut tx).write(&[0, 0, 0, 0]).await.unwrap();
        Pin::new(&mut orig_item)
            .serialize(Pin::new(&mut tx))
            .await
            .unwrap();
        let (buf, fds) = tx.into_inner();
        eprintln!("{:x?}", buf);

        let mut rx = ReadPool::new(buf, fds);
        let (_, _, mut accessor) = WaylandBufAccess::next_message(&mut rx).await.unwrap();
        let item = Request::deserialize(&mut accessor).unwrap();
        eprintln!("{:?}", item);
        assert_eq!(&item, &orig_item);

        // Making sure dropping the accessor advance the buffer.
        drop(item);
        drop(accessor);
        assert!(rx.is_eof());
    })
}
