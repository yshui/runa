use std::{
    ffi::CStr,
    os::{
        fd::{self, IntoRawFd, OwnedFd},
        unix::prelude::AsRawFd,
    },
    pin::Pin,
};

use bytes::{buf, BufMut};
use futures_lite::{io::AsyncWriteExt, AsyncBufReadExt};
use runa_io::{
    traits::{
        de::Deserialize, ser::Serialize, AsyncBufReadWithFd, RawMessage, ReadMessage, WriteMessage,
    },
    utils::{ReadPool, WritePool},
    Connection,
};
use runa_wayland_types::{Fd, NewId, Object, Str};
use wayland::wl_display::v1::{events, Event};
runa_wayland_scanner::generate_protocol!("runa-wayland-protocols/spec/wayland.xml");

#[test]
fn test_roundtrip() {
    futures_executor::block_on(async {
        let orig_item = Event::Error(events::Error {
            object_id: Object(1),
            code:      2,
            message:   Str(b"test"),
        });
        let mut tx = Connection::new(WritePool::new(), 4096);
        // Put an object id
        tx.send(1, orig_item).await.unwrap();
        tx.flush().await.unwrap();
        let pool = tx.into_inner();
        let (buf, fds) = pool.into_inner();
        let fds = fds
            .into_iter()
            .map(|fd| fd.into_raw_fd())
            .collect::<Vec<_>>();

        let mut rx = ReadPool::new(buf, fds);
        let RawMessage {
            object_id, data, ..
        } = Pin::new(&mut rx).next_message().await.unwrap();
        assert_eq!(object_id, 1);
        let item = Event::deserialize(data, &[]).unwrap();

        eprintln!("{:#?}", item);
        assert_eq!(
            &item,
            &Event::Error(events::Error {
                object_id: Object(1),
                code:      2,
                message:   Str(b"test"),
            })
        );

        let (data_len, fds_len) = (rx.buffer().len(), rx.fds().len());
        AsyncBufReadWithFd::consume(Pin::new(&mut rx), data_len, fds_len);
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

        let mut tx = Connection::new(WritePool::new(), 4096);
        // Put an object id
        tx.send(1, orig_item).await.unwrap();
        tx.flush().await.unwrap();
        let (buf, fds) = tx.into_inner().into_inner();
        eprintln!("{:x?}", buf);

        let fds = fds
            .into_iter()
            .map(|fd| fd.into_raw_fd())
            .collect::<Vec<_>>();
        let mut rx = ReadPool::new(buf, fds);
        let RawMessage {
            object_id,
            data,
            fds,
            ..
        } = Pin::new(&mut rx).next_message().await.unwrap();
        assert_eq!(object_id, 1);
        let item = Request::deserialize(data, fds).unwrap();
        eprintln!("{:#?}", item);

        // Compare the result skipping the file descriptor
        match item {
            Request::CreatePool(requests::CreatePool { id, ref fd, size }) => {
                assert!(fd.as_raw_fd() >= 0); // make sure fd is valid
                assert_eq!(id, NewId(1));
                assert_eq!(size, 100);
            },
        }

        drop(item);
        let (data_len, fds_len) = (rx.buffer().len(), rx.fds().len());
        AsyncBufReadWithFd::consume(Pin::new(&mut rx), data_len, fds_len);
        assert!(rx.is_eof());
    })
}
