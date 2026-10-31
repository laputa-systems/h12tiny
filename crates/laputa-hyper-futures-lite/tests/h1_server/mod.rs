pub mod fixture;

use futures_channel::mpsc;
use hyper::rt::{Read, ReadBufCursor};
use pin_project_lite::pin_project;
use std::io;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

pin_project! {
    #[derive(Debug)]
    pub struct StreamReadHalf {
        read_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        read_buffer: Vec<u8>,
    }
}

impl StreamReadHalf {
    pub fn new(read_rx: mpsc::UnboundedReceiver<Vec<u8>>) -> Self {
        Self {
            read_rx,
            read_buffer: Vec::new(),
        }
    }
}

impl Read for StreamReadHalf {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.as_mut().project();

        if !this.read_buffer.is_empty() {
            let to_read = std::cmp::min(this.read_buffer.len(), buf.remaining());
            buf.put_slice(&this.read_buffer[..to_read]);
            this.read_buffer.drain(..to_read);
            return Poll::Ready(Ok(()));
        }

        match this.read_rx.try_recv() {
            Ok(data) => {
                let to_read = std::cmp::min(data.len(), buf.remaining());
                buf.put_slice(&data[..to_read]);

                if to_read < data.len() {
                    let remaining = &data[to_read..];
                    this.read_buffer.extend_from_slice(remaining);
                }
                Poll::Ready(Ok(()))
            }
            Err(mpsc::TryRecvError::Empty) => {
                match ready!(futures_util::Stream::poll_next(Pin::new(this.read_rx), cx)) {
                    Some(data) => {
                        let to_read = std::cmp::min(data.len(), buf.remaining());
                        buf.put_slice(&data[..to_read]);

                        if to_read < data.len() {
                            let remaining = &data[to_read..];
                            this.read_buffer.extend_from_slice(remaining);
                        }
                        Poll::Ready(Ok(()))
                    }
                    None => Poll::Ready(Ok(())),
                }
            }
            Err(mpsc::TryRecvError::Closed) => Poll::Ready(Ok(())),
        }
    }
}

pub fn init_tracing() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_target(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .init();
    });
}
