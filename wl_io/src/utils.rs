#![doc(hidden)]

use std::io::Cursor;
use std::os::unix::prelude::RawFd;
use std::task::Poll;

use super::AsyncReadWithFd;
use super::AsyncWriteWithFd;

#[derive(Default, Debug)]
pub struct WritePool {
    inner: Vec<u8>,
    fds: Vec<RawFd>,
}

impl futures_lite::AsyncWrite for WritePool {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.inner.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncWriteWithFd for WritePool {
    fn poll_write_with_fds(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
        fds: &[RawFd],
    ) -> Poll<std::io::Result<usize>> {
        self.inner.extend_from_slice(buf);
        self.fds.extend_from_slice(fds);
        Poll::Ready(Ok(buf.len()))
    }
}

impl WritePool {
    pub fn new() -> Self {
        Self {
            inner: Vec::new(),
            fds: Vec::new(),
        }
    }
    pub fn into_inner(self) -> (Vec<u8>, Vec<RawFd>) {
        (self.inner, self.fds)
    }
}

#[derive(Debug)]
pub struct ReadPool {
    inner: Vec<u8>,
    fds: Vec<RawFd>,
}

impl std::io::Read for ReadPool {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let len = std::cmp::min(buf.len(), self.inner.len());
        buf[..len].copy_from_slice(&self.inner[..len]);
        self.inner.drain(..len);
        Ok(len)
    }
}

impl futures_lite::AsyncRead for ReadPool {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        use std::io::Read;
        std::task::Poll::Ready(self.read(buf))
    }
}

impl futures_lite::AsyncBufRead for ReadPool {
    fn poll_fill_buf(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<&[u8]>> {
        use std::io::BufRead;
        Poll::Ready(Ok(self.get_mut().inner.as_slice()))
    }
    fn consume(mut self: std::pin::Pin<&mut Self>, amt: usize) {
        use std::io::BufRead;
        use crate::buf::AsyncBufReadWithFd;
        self.consume_with_fds(amt, 0)
    }
}

impl crate::AsyncBufReadWithFd for ReadPool {
    fn poll_fill_buf_until<'a>(
            self: std::pin::Pin<&'a mut Self>,
            cx: &mut std::task::Context<'_>,
            len: usize,
        ) -> Poll<std::io::Result<(&'a [u8], &'a [RawFd])>> {
        if len > self.inner.len() {
            Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Unexpected EOF")))
        } else {
            let this = self.get_mut();
            Poll::Ready(Ok((this.inner.as_slice(), this.fds.as_slice())))
        }
    }
    fn consume_with_fds(self: std::pin::Pin<&mut Self>, amt_data: usize, amt_fd: usize) {
        use std::io::BufRead;
        eprintln!("consume {} {}", amt_data, amt_fd);
        let this = self.get_mut();
        this.inner.drain(..amt_data);
        this.fds.drain(..amt_fd);
    }
}

impl AsyncReadWithFd for ReadPool {
    fn poll_read_with_fds(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
        fds: &mut [RawFd],
    ) -> Poll<std::io::Result<(usize, usize)>> {
        use std::io::Read;
        let len = self.read(buf).unwrap();
        let fds_read = std::cmp::min(fds.len(), self.fds.len());
        fds.copy_from_slice(&self.fds[..fds_read]);
        self.fds.drain(..fds_read);
        Poll::Ready(Ok((len, fds_read)))
    }
}

impl ReadPool {
    pub fn new(data: Vec<u8>, fds: Vec<RawFd>) -> Self {
        Self {
            inner: data,
            fds,
        }
    }
    pub fn is_eof(&self) -> bool {
        self.inner.is_empty() && self.fds.is_empty()
    }
}
