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
        crate::rt::Write::poll_shutdown(self.p(), cx)
    }
}
