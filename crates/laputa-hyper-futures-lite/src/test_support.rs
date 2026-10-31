#![allow(unreachable_pub)]
#![allow(dead_code)]
use std::future::Future;

pub use laputa_protocol_test_support::mock_io;
pub use laputa_protocol_test_support::timer::timeout;
pub use std::task::Poll as PollResult;

pub fn block_on<F: Future>(future: F) -> F::Output {
    smol::block_on(future)
}

pub mod task {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    struct WakeFlag(AtomicBool);

    impl Wake for WakeFlag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    pub struct Spawn<T: Future> {
        future: Pin<Box<T>>,
        flag: Arc<WakeFlag>,
    }

    pub fn spawn<T: Future>(future: T) -> Spawn<T> {
        Spawn {
            future: Box::pin(future),
            flag: Arc::new(WakeFlag(AtomicBool::new(false))),
        }
    }

    impl<T: Future> Spawn<T> {
        pub fn poll(&mut self) -> Poll<T::Output> {
            let waker = Waker::from(self.flag.clone());
            let mut cx = Context::from_waker(&waker);
            self.flag.0.store(false, Ordering::SeqCst);
            self.future.as_mut().poll(&mut cx)
        }

        pub fn enter<F, R>(&mut self, f: F) -> R
        where
            F: FnOnce(&mut Context<'_>, Pin<&mut T>) -> R,
        {
            let waker = Waker::from(self.flag.clone());
            let mut cx = Context::from_waker(&waker);
            self.flag.0.store(false, Ordering::SeqCst);
            f(&mut cx, self.future.as_mut())
        }

        pub fn is_woken(&self) -> bool {
            self.flag.0.load(Ordering::SeqCst)
        }
    }
}

macro_rules! assert_ready {
    ($e:expr) => {
        match $e {
            $crate::test_support::PollResult::Ready(value) => value,
            $crate::test_support::PollResult::Pending => {
                panic!("future was not ready")
            }
        }
    };
}

macro_rules! assert_ready_ok {
    ($e:expr) => {
        match $crate::test_support::assert_ready!($e) {
            Ok(value) => value,
            Err(_) => panic!("future was not ready"),
        }
    };
}

macro_rules! assert_ready_err {
    ($e:expr) => {
        match $crate::test_support::assert_ready!($e) {
            Err(error) => error,
            Ok(_) => panic!("future was not ready"),
        }
    };
}

pub(crate) use assert_ready;
pub(crate) use assert_ready_err;
pub(crate) use assert_ready_ok;

pub struct FuturesIo<T>(pub T);

impl<T> FuturesIo<T> {
    pub fn new(inner: T) -> Self {
        Self(inner)
    }

    pub fn inner(self) -> T {
        self.0
    }
}

impl<T> crate::rt::Read for FuturesIo<T>
where
    T: futures_io::AsyncRead,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        mut buf: crate::rt::ReadBufCursor<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let mut inner = unsafe {
            // SAFETY: the wrapper has no drop implementation and structurally
            // pins its only field.
            self.map_unchecked_mut(|this| &mut this.0)
        };
        if buf.remaining() == 0 {
            return std::task::Poll::Ready(Ok(()));
        }
        let capacity = buf.remaining();
        let dst = buf.initialize_unfilled();
        let count = match futures_io::AsyncRead::poll_read(inner.as_mut(), cx, dst) {
            std::task::Poll::Ready(Ok(count)) => count,
            std::task::Poll::Ready(Err(error)) => return std::task::Poll::Ready(Err(error)),
            std::task::Poll::Pending => return std::task::Poll::Pending,
        };
        if count > capacity {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "futures-io AsyncRead returned more bytes than requested",
            )));
        }
        unsafe {
            // SAFETY: the region was initialized and `count` is bounded.
            buf.advance(count);
        }
        std::task::Poll::Ready(Ok(()))
    }
}

impl<T> crate::rt::Write for FuturesIo<T>
where
    T: futures_io::AsyncWrite,
{
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let mut inner = unsafe {
            // SAFETY: see the read projection above.
            self.map_unchecked_mut(|this| &mut this.0)
        };
        futures_io::AsyncWrite::poll_write(inner.as_mut(), cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let mut inner = unsafe {
            // SAFETY: see the read projection above.
            self.map_unchecked_mut(|this| &mut this.0)
        };
        futures_io::AsyncWrite::poll_flush(inner.as_mut(), cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let mut inner = unsafe {
            // SAFETY: see the read projection above.
            self.map_unchecked_mut(|this| &mut this.0)
        };
        futures_io::AsyncWrite::poll_close(inner.as_mut(), cx)
    }
}
