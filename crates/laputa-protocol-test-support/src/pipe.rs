use futures_io::{AsyncRead, AsyncWrite};
use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

struct Buffer {
    bytes: VecDeque<u8>,
    limit: usize,
    closed: bool,
    reader: Option<Waker>,
    writer: Option<Waker>,
}

impl Buffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            limit,
            closed: false,
            reader: None,
            writer: None,
        }
    }
}

#[derive(Clone)]
pub struct BudgetPipe {
    incoming: Arc<Mutex<Buffer>>,
    outgoing: Arc<Mutex<Buffer>>,
}

impl BudgetPipe {
    pub fn pair(write_limit: usize) -> (Self, Self) {
        let first_to_second = Arc::new(Mutex::new(Buffer::new(write_limit)));
        let second_to_first = Arc::new(Mutex::new(Buffer::new(write_limit)));

        (
            Self {
                incoming: second_to_first.clone(),
                outgoing: first_to_second.clone(),
            },
            Self {
                incoming: first_to_second,
                outgoing: second_to_first,
            },
        )
    }

    pub fn buffered(&self) -> usize {
        self.outgoing.lock().unwrap().bytes.len()
    }

    pub fn set_write_limit(&self, limit: usize) {
        let wake = {
            let mut outgoing = self.outgoing.lock().unwrap();
            outgoing.limit = limit;
            outgoing.writer.take()
        };
        if let Some(waker) = wake {
            waker.wake();
        }
    }

    fn close_write(&mut self) {
        let wake = {
            let mut outgoing = self.outgoing.lock().unwrap();
            outgoing.closed = true;
            outgoing.reader.take()
        };
        if let Some(waker) = wake {
            waker.wake();
        }
    }
}

impl AsyncRead for BudgetPipe {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        let (count, writer) = {
            let mut incoming = self.incoming.lock().unwrap();
            if incoming.bytes.is_empty() {
                if incoming.closed {
                    return Poll::Ready(Ok(0));
                }
                incoming.reader = Some(cx.waker().clone());
                return Poll::Pending;
            }

            let count = buf.len().min(incoming.bytes.len());
            for byte in &mut buf[..count] {
                *byte = incoming.bytes.pop_front().unwrap();
            }
            (count, incoming.writer.take())
        };

        if let Some(waker) = writer {
            waker.wake();
        }
        Poll::Ready(Ok(count))
    }
}

impl AsyncWrite for BudgetPipe {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        let (count, reader) = {
            let mut outgoing = self.outgoing.lock().unwrap();
            if outgoing.closed {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "peer closed",
                )));
            }

            let available = outgoing.limit.saturating_sub(outgoing.bytes.len());
            if available == 0 {
                outgoing.writer = Some(cx.waker().clone());
                return Poll::Pending;
            }

            let count = buf.len().min(available);
            outgoing.bytes.extend(buf[..count].iter().copied());
            (count, outgoing.reader.take())
        };

        if let Some(waker) = reader {
            waker.wake();
        }
        Poll::Ready(Ok(count))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.close_write();
        Poll::Ready(Ok(()))
    }
}

impl Drop for BudgetPipe {
    fn drop(&mut self) {
        self.close_write();
    }
}
