#![doc(hidden)]

use std::io::Cursor;
use std::os::unix::prelude::RawFd;
use std::task::Poll;

use super::AsyncReadWithFds;
use super::AsyncWriteWithFds;

#[derive(Default)]
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

impl AsyncWriteWithFds for WritePool {
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

pub struct ReadPool {
    inner: Cursor<Vec<u8>>,
    fds: Vec<RawFd>,
    fds_offset: usize,
}

impl futures_lite::AsyncRead for ReadPool {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        use std::io::Read;
        std::task::Poll::Ready(self.inner.read(buf))
    }
}

impl futures_lite::AsyncBufRead for ReadPool {
    fn poll_fill_buf(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<&[u8]>> {
        use std::io::BufRead;
        Poll::Ready(self.get_mut().inner.fill_buf())
    }
    fn consume(mut self: std::pin::Pin<&mut Self>, amt: usize) {
        use std::io::BufRead;
        self.inner.consume(amt)
    }
}

impl crate::AsyncBufReadExt for ReadPool {
    fn poll_fill_buf_until<'a>(
            self: std::pin::Pin<&'a mut Self>,
            cx: &mut std::task::Context<'_>,
            len: usize,
        ) -> Poll<std::io::Result<&'a [u8]>> {
        if len > self.inner.get_ref().len() - self.inner.position() as usize {
            Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Unexpected EOF")))
        } else {
            use futures_lite::AsyncBufRead;
            self.poll_fill_buf(cx)
        }
    }
}

impl AsyncReadWithFds for ReadPool {
    fn poll_read_with_fds(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
        fds: &mut [RawFd],
    ) -> Poll<std::io::Result<(usize, usize)>> {
        use std::io::Read;
        let len = self.inner.read(buf)?;
        let fds_read = std::cmp::min(fds.len(), self.fds.len() - self.fds_offset);
        fds.copy_from_slice(&self.fds[self.fds_offset..self.fds_offset + fds_read]);
        self.fds_offset += fds_read;
        Poll::Ready(Ok((len, fds_read)))
    }
}

impl ReadPool {
    pub fn new(data: Vec<u8>, fds: Vec<RawFd>) -> Self {
        Self {
            inner: Cursor::new(data),
            fds,
            fds_offset: 0,
        }
    }
}
