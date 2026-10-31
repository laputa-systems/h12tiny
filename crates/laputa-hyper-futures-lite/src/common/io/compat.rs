use futures_io::{AsyncRead, AsyncWrite};
use std::pin::Pin;
use std::task::{Context, Poll};

/// Adapts Hyper's runtime I/O traits to the futures I/O traits used by h2.
#[derive(Debug)]
pub(crate) struct Compat<T>(pub(crate) T);

impl<T> Compat<T> {
    pub(crate) fn new(io: T) -> Self {
        Compat(io)
    }

    fn p(self: Pin<&mut Self>) -> Pin<&mut T> {
        // SAFETY: The simplest of projections. This is just
        // a wrapper, we don't do anything that would undo the projection.
        unsafe { self.map_unchecked_mut(|me| &mut me.0) }
    }
}

impl<T> AsyncRead for Compat<T>
where
    T: crate::rt::Read,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let mut cursor = crate::rt::ReadBuf::new(buf);
        let before = cursor.len();
        match crate::rt::Read::poll_read(self.p(), cx, cursor.unfilled()) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(cursor.len() - before)),
            Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> AsyncWrite for Compat<T>
where
    T: crate::rt::Write,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        crate::rt::Write::poll_write(self.p(), cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        crate::rt::Write::poll_flush(self.p(), cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        match crate::rt::Write::poll_shutdown(self.p(), cx) {
            Poll::Ready(Err(err)) if err.kind() == std::io::ErrorKind::NotConnected => {
                Poll::Ready(Ok(()))
            }
            result => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Compat;
    use crate::rt::Write;
    use futures_io::AsyncWrite;
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    struct ShutdownIo {
        kind: io::ErrorKind,
    }

    impl Write for ShutdownIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Err(self.kind.into()))
        }
    }

    fn close(kind: io::ErrorKind) -> Poll<io::Result<()>> {
        let mut io = Compat::new(ShutdownIo { kind });
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        AsyncWrite::poll_close(Pin::new(&mut io), &mut cx)
    }

    #[test]
    fn close_treats_not_connected_as_already_closed() {
        assert!(matches!(
            close(io::ErrorKind::NotConnected),
            Poll::Ready(Ok(()))
        ));
    }

    #[test]
    fn close_propagates_other_shutdown_errors() {
        assert!(matches!(
            close(io::ErrorKind::BrokenPipe),
            Poll::Ready(Err(err)) if err.kind() == io::ErrorKind::BrokenPipe
        ));
    }

    fn noop_waker() -> Waker {
        // SAFETY: the no-op functions never dereference or access the data
        // pointer. The vtable is valid for a Waker with a null data pointer.
        unsafe fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        unsafe fn wake(_: *const ()) {}
        unsafe fn wake_by_ref(_: *const ()) {}
        unsafe fn drop(_: *const ()) {}
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);

        // SAFETY: `VTABLE` implements all operations for the null data
        // pointer and does not retain or dereference it.
        unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
    }
}
