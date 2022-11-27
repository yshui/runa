use std::{
    ffi::CStr,
    os::{fd::IntoRawFd, unix::prelude::AsRawFd},
    pin::Pin,
};

use futures_lite::io::AsyncWriteExt;
use wayland::wl_display::v1::{events, Event};
use wl_io::utils::{ReadPool, WritePool};
use wl_macros::generate_protocol;
use wl_scanner_support::{
    io::{
        buf::{AsyncBufReadWithFd, AsyncBufReadWithFdExt},
        de::Deserialize,
        ser::Serialize,
    },
    wayland_types::{Fd, NewId, Object, Str},
};
generate_protocol!("protocols/wayland.xml");

#[test]
fn test_roundtrip() {
    futures_executor::block_on(async {
        let orig_item = Event::Error(events::Error {
            object_id: Object(1),
            code:      2,
            message:   Str(CStr::from_bytes_with_nul(b"test\0").unwrap()),
        });
        let mut tx = WritePool::new();
        // Put an object id
        tx.write(&[0, 0, 0, 0]).await.unwrap();
        orig_item.serialize(Pin::new(&mut tx));
        let (buf, fds) = tx.into_inner();
        let fds = fds
            .into_iter()
            .map(|fd| fd.into_raw_fd())
            .collect::<Vec<_>>();

        let mut rx = ReadPool::new(buf, fds);
        let (object_id, _len, bytes, fds) = Pin::new(&mut rx).next_message().await.unwrap();
        assert_eq!(object_id, 0);
        let mut de = wl_io::de::Deserializer::new(bytes, fds);
        let item = Event::deserialize(de.borrow_mut()).unwrap();
        let (bytes_read, fds_read) = de.consumed();
        // Drop the deserialize first to catch potential UB, i.e. the item doesnt borrow
        // from the deserializer
        drop(de);

        eprintln!("{:#?}", item);
        assert_eq!(
            &item,
            &Event::Error(events::Error {
                object_id: Object(1),
                code:      2,
                message:   Str(CStr::from_bytes_with_nul(b"test\0").unwrap()),
            })
        );
        drop(item);
        Pin::new(&mut rx).consume(bytes_read, fds_read);
        assert!(rx.is_eof());
    })
}

#[test]
fn test_roundtrip_with_fd() {
    use std::os::unix::io::IntoRawFd;

    use wayland::wl_shm::v1::{requests, Request};
    futures_executor::block_on(async {
        let fd = std::fs::File::open("/dev/null").unwrap();
        let orig_item = Request::CreatePool(requests::CreatePool {
            id:   NewId(1),
            fd:   Fd::Owned(fd.into()),
            size: 100,
        });

        let mut tx = WritePool::new();
        // Put an object id
        Pin::new(&mut tx).write(&[0, 0, 0, 0]).await.unwrap();
        orig_item.serialize(Pin::new(&mut tx));
        let (buf, fds) = tx.into_inner();
        eprintln!("{:x?}", buf);

        let fds = fds
            .into_iter()
            .map(|fd| fd.into_raw_fd())
            .collect::<Vec<_>>();
        let mut rx = ReadPool::new(buf, fds);
        let (object_id, _, bytes, fds) = Pin::new(&mut rx).next_message().await.unwrap();
        assert_eq!(object_id, 0);
        let mut de = wl_io::de::Deserializer::new(bytes, fds);
        let item = Request::deserialize(de.borrow_mut()).unwrap();
        let (bytes_read, fds_read) = de.consumed();
        drop(de);
        eprintln!("{:#?}", item);
        Pin::new(&mut rx).consume(bytes_read, fds_read);

        // Compare the result skipping the file descriptor
        match item {
            Request::CreatePool(requests::CreatePool { id, ref fd, size }) => {
                assert!(fd.as_raw_fd() >= 0); // make sure fd is valid
                assert_eq!(id, NewId(1));
                assert_eq!(size, 100);
            },
        }

        // Making sure dropping the accessor advance the buffer.
        drop(item);
        assert!(rx.is_eof());
    })
}
