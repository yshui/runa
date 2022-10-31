use std::{ffi::CStr, os::unix::{io::OwnedFd, prelude::AsRawFd}, pin::Pin, task::Context};

use futures_lite::io::{AsyncBufRead, AsyncRead, AsyncWrite, AsyncWriteExt};
use wl_macros::generate_protocol;
use wl_scanner_support::{
    future::poll_map_fn,
    io::{
        de::Deserializer,
        utils::{ReadPool, WritePool},
        Serialize,
    },
    serde::Deserialize,
    wayland_types::{Fd, NewId, Object, Str},
};

generate_protocol!("protocols/wayland.xml");

#[test]
fn test_roundtrip() {
    use wayland::wl_display::v1::{events, Event};
    futures_executor::block_on(async {
        let mut orig_item = Event::Error(events::Error {
            object_id: Object(1),
            code:      2,
            message:   Str(CStr::from_bytes_with_nul(b"test\0").unwrap()),
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
        let (object_id, len, mut de) = Deserializer::next_message(Pin::new(&mut rx)).await.unwrap();
        assert_eq!(object_id, 0);
        let item = Event::deserialize(&mut de).unwrap();
        // Drop the deserialize first to catch potential UB, i.e. the item doesnt borrow
        // from the deserializer
        drop(de);

        eprintln!("{:#?}", item);
        eprintln!("{:#?}", orig_item);
        assert_eq!(&item, &orig_item);
        drop(item);
        Pin::new(&mut rx).consume(len);
        assert!(rx.is_eof());
    })
}

#[test]
fn test_roundtrip_with_fd() {
    use std::os::unix::io::IntoRawFd;

    use wayland::wl_shm::v1::{requests, Request};
    futures_executor::block_on(async {
        let fd = std::fs::File::open("/dev/null").unwrap();
        let mut orig_item = Request::CreatePool(requests::CreatePool {
            id:   NewId(1),
            fd:   Fd::Raw(fd.as_raw_fd()),
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
        // Make sure WritePool dup the fd
        drop(fd);

        let mut rx = ReadPool::new(buf, fds);
        let (object_id, len, mut de) = Deserializer::next_message(Pin::new(&mut rx)).await.unwrap();
        assert_eq!(object_id, 0);
        let item = Request::deserialize(&mut de).unwrap();
        drop(de);
        eprintln!("{:#?}", item);
        eprintln!("{:#?}", orig_item);

        // Compare the result skipping the file descriptor
        match item {
            Request::CreatePool(requests::CreatePool { id, ref fd, size }) => {
                assert!(fd.as_raw_fd() >= 0); // make sure fd is valid
                match orig_item {
                    Request::CreatePool(requests::CreatePool { id: id2, fd: _, size: size2 }) => {
                        assert_eq!(id, id2);
                        assert_eq!(size, size2);
                    }
                }
            },
        }

        // Making sure dropping the accessor advance the buffer.
        drop(item);
        Pin::new(&mut rx).consume(len);
        assert!(rx.is_eof());
    })
}
