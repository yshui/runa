use wl_scanner_macros::generate_protocol;
use wl_scanner_support::wayland_types::NewId;
use wl_scanner_support::io::{WithFd, Serialize, BufWriterWithFd};
use std::pin::Pin;

generate_protocol!("protocols/wayland.xml");

#[test]
fn test_serialize() {
    use std::io::Read;
    use smol::io::AsyncWriteExt;
    let mut item = wayland::wl_display::v1::requests::Sync { callback: NewId(0) };
    let (fd1, mut fd2) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut fd1 = BufWriterWithFd::new(WithFd::new(fd1).unwrap());
    smol::block_on(Pin::new(&mut item).serialize(Pin::new(&mut fd1))).unwrap();
    smol::block_on(Pin::new(&mut fd1).close()).unwrap();
    drop(fd1);
    let mut buf = Vec::new();
    fd2.read_to_end(&mut buf).unwrap();
    let numbers: &[u32] = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u32, buf.len() / 4) };
    eprintln!("{:x?}", numbers);
}
