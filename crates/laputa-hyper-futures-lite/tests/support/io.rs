#![allow(dead_code)]
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use hyper::rt::{Executor, ReadBufCursor, Sleep, Timer, Write};

#[derive(Debug)]
pub struct FuturesIo<T> {
    inner: T,
}

impl<T> FuturesIo<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    pub fn inner(self) -> T {
        self.inner
    }

    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T> hyper::rt::Read for FuturesIo<T>
where
    T: futures_io::AsyncRead,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: ReadBufCursor<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let mut inner = unsafe {
            // SAFETY: `FuturesIo` has no drop implementation and structurally
            // pins its only field; the field is never moved while projected.
            self.map_unchecked_mut(|this| &mut this.inner)
        };

        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }

        let capacity = buf.remaining();
        let dst = buf.initialize_unfilled();
        let n = match futures_io::AsyncRead::poll_read(inner.as_mut(), cx, dst) {
            Poll::Ready(Ok(n)) => n,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Pending => return Poll::Pending,
        };

        if n > capacity {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "futures-io AsyncRead returned more bytes than requested",
            )));
        }

        unsafe {
            // SAFETY: the cursor region was initialized above and the bounds
            // check proves `n` stays within it.
            buf.advance(n);
        }
        Poll::Ready(Ok(()))
    }
}

impl<T> Write for FuturesIo<T>
where
    T: futures_io::AsyncWrite,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let mut inner = unsafe {
            // SAFETY: see the read projection above.
            self.map_unchecked_mut(|this| &mut this.inner)
        };
        futures_io::AsyncWrite::poll_write(inner.as_mut(), cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        let mut inner = unsafe {
            // SAFETY: see the read projection above.
            self.map_unchecked_mut(|this| &mut this.inner)
        };
        futures_io::AsyncWrite::poll_flush(inner.as_mut(), cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let mut inner = unsafe {
            // SAFETY: see the read projection above.
            self.map_unchecked_mut(|this| &mut this.inner)
        };
        futures_io::AsyncWrite::poll_close(inner.as_mut(), cx)
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<Result<usize, std::io::Error>> {
        let mut inner = unsafe {
            // SAFETY: see the read projection above.
            self.map_unchecked_mut(|this| &mut this.inner)
        };
        futures_io::AsyncWrite::poll_write_vectored(inner.as_mut(), cx, bufs)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SmolExecutor;

impl<F> Executor<F> for SmolExecutor
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, future: F) {
        smol::spawn(future).detach();
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SmolTimer;

impl Timer for SmolTimer {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Sleep>> {
        Box::pin(SmolSleep(smol::Timer::after(duration)))
    }

    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn Sleep>> {
        Box::pin(SmolSleep(smol::Timer::at(deadline)))
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

struct SmolSleep(smol::Timer);

impl Future for SmolSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.get_mut().0).poll(cx) {
            Poll::Ready(_) => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Sleep for SmolSleep {}

pub struct HyperIo<T> {
    inner: T,
}

impl<T> HyperIo<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    pub fn inner(self) -> T {
        self.inner
    }
}

impl<T> futures_io::AsyncRead for HyperIo<T>
where
    T: hyper::rt::Read,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let mut inner = unsafe {
            // SAFETY: `HyperIo` has no drop implementation and structurally
            // pins its only field; the field is never moved while projected.
            self.map_unchecked_mut(|this| &mut this.inner)
        };
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let mut read_buf = hyper::rt::ReadBuf::new(buf);
        match hyper::rt::Read::poll_read(inner.as_mut(), cx, read_buf.unfilled()) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(read_buf.filled().len())),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> futures_io::AsyncWrite for HyperIo<T>
where
    T: hyper::rt::Write,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let mut inner = unsafe {
            // SAFETY: see the read projection above.
            self.map_unchecked_mut(|this| &mut this.inner)
        };
        hyper::rt::Write::poll_write(inner.as_mut(), cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let mut inner = unsafe {
            // SAFETY: see the read projection above.
            self.map_unchecked_mut(|this| &mut this.inner)
        };
        hyper::rt::Write::poll_write_vectored(inner.as_mut(), cx, bufs)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let mut inner = unsafe {
            // SAFETY: see the read projection above.
            self.map_unchecked_mut(|this| &mut this.inner)
        };
        hyper::rt::Write::poll_flush(inner.as_mut(), cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let mut inner = unsafe {
            // SAFETY: see the read projection above.
            self.map_unchecked_mut(|this| &mut this.inner)
        };
        hyper::rt::Write::poll_shutdown(inner.as_mut(), cx)
    }
}
