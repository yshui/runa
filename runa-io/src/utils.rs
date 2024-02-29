#![doc(hidden)]

use std::{
    os::{
        fd::FromRawFd,
        unix::io::{OwnedFd, RawFd},
    },
    pin::Pin,
    task::Poll,
};

use super::traits::{buf::AsyncBufReadWithFd, AsyncReadWithFd, AsyncWriteWithFd, OwnedFds};
use crate::traits;

/// An in-memory buffer that implements `AsyncWrite` and `AsyncWriteWithFd`. Intended for testing
/// purposes.
#[derive(Default, Debug)]
pub struct WritePool {
    inner: Vec<u8>,
    fds:   Vec<OwnedFd>,
}

impl futures_lite::AsyncWrite for WritePool {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.inner.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncWriteWithFd for WritePool {
    fn poll_write_with_fds<Fds: traits::OwnedFds>(
        mut self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
        fds: &mut Fds,
    ) -> Poll<std::io::Result<usize>> {
        self.inner.extend_from_slice(buf);
        fds.take(&mut self.fds);
        Poll::Ready(Ok(buf.len()))
    }
}

impl WritePool {
    /// Create a new `WritePool`.
    pub fn new() -> Self {
        Self {
            inner: Vec::new(),
            fds:   Vec::new(),
        }
    }

    /// Consume the `WritePool` and return the inner buffer and file descriptors.
    pub fn into_inner(self) -> (Vec<u8>, Vec<OwnedFd>) {
        (self.inner, self.fds)
    }
}

/// An in-memory buffer that implements `AsyncRead` and `AsyncReadWithFd`. Intended for testing
/// purposes.
#[derive(Debug)]
pub struct ReadPool {
    inner: Vec<u8>,
    fds:   Vec<RawFd>,
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
        mut self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        use std::io::Read;
        std::task::Poll::Ready(self.read(buf))
    }
}

impl futures_lite::AsyncBufRead for ReadPool {
    fn poll_fill_buf(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<&[u8]>> {
        Poll::Ready(Ok(self.get_mut().inner.as_slice()))
    }

    fn consume(mut self: Pin<&mut Self>, amt: usize) {
        self.inner.drain(..amt);
    }
}

unsafe impl AsyncBufReadWithFd for ReadPool {
    fn poll_fill_buf_until<'a>(
        self: Pin<&'a mut Self>,
        _cx: &mut std::task::Context<'_>,
        len: usize,
    ) -> Poll<std::io::Result<()>> {
        if len > self.inner.len() {
            Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Unexpected EOF",
            )))
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn fds(&self) -> &[RawFd] {
        &self.fds[..]
    }

    fn buffer(&self) -> &[u8] {
        &self.inner[..]
    }

    fn consume(mut self: Pin<&mut Self>, amt: usize, amt_fd: usize) {
        self.inner.drain(..amt);
        self.fds.drain(..amt_fd);
    }
}

impl AsyncReadWithFd for ReadPool {
    fn poll_read_with_fds<Fds: OwnedFds>(
        mut self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
        fds: &mut Fds,
    ) -> Poll<std::io::Result<usize>> {
        use std::io::Read;
        let len = self.read(buf).unwrap();
        let fd_len = std::cmp::min(fds.len(), self.fds.len());
        fds.extend(
            self.fds
                .drain(..fd_len)
                .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) }),
        );
        Poll::Ready(Ok(len))
    }
}

impl ReadPool {
    /// Create a new `ReadPool` with the given data and file descriptors.
    pub fn new(data: Vec<u8>, fds: Vec<RawFd>) -> Self {
        Self { inner: data, fds }
    }

    pub fn is_eof(&self) -> bool {
        self.inner.is_empty() && self.fds.is_empty()
    }
}
