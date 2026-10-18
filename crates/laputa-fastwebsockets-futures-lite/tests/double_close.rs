use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use fastwebsockets::{after_handshake_split, Frame, Role, WebSocketError};
use futures_lite::io::{AsyncRead, AsyncWrite};

struct MockStream {
  write_call_count: Arc<AtomicUsize>,
  bytes_written: Arc<AtomicUsize>,
}

impl MockStream {
  fn new() -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let write_call_count = Arc::new(AtomicUsize::new(0));
    let bytes_written = Arc::new(AtomicUsize::new(0));
    (
      Self {
        write_call_count: write_call_count.clone(),
        bytes_written: bytes_written.clone(),
      },
      write_call_count,
      bytes_written,
    )
  }
}

impl AsyncRead for MockStream {
  fn poll_read(
    self: Pin<&mut Self>,
    _cx: &mut Context<'_>,
    _buf: &mut [u8],
  ) -> Poll<std::io::Result<usize>> {
    Poll::Pending
  }
}

impl AsyncWrite for MockStream {
  fn poll_write(
    self: Pin<&mut Self>,
    _cx: &mut Context<'_>,
    buf: &[u8],
  ) -> Poll<std::io::Result<usize>> {
    self.write_call_count.fetch_add(1, Ordering::SeqCst);
    self.bytes_written.fetch_add(buf.len(), Ordering::SeqCst);
    Poll::Ready(Ok(buf.len()))
  }

  fn poll_flush(
    self: Pin<&mut Self>,
    _cx: &mut Context<'_>,
  ) -> Poll<std::io::Result<()>> {
    Poll::Ready(Ok(()))
  }

  fn poll_close(
    self: Pin<&mut Self>,
    _cx: &mut Context<'_>,
  ) -> Poll<std::io::Result<()>> {
    Poll::Ready(Ok(()))
  }
}

#[test]
fn double_close_sends_only_one_frame() {
  let (mock, write_count, bytes_written) = MockStream::new();
  let (_read, mut write) =
    after_handshake_split(futures_lite::io::empty(), mock, Role::Server);
  write.set_writev(false);

  futures_lite::future::block_on(async {
    write
      .write_frame(Frame::close(1000, b"goodbye"))
      .await
      .unwrap();

    let writes_after_first = write_count.load(Ordering::SeqCst);
    let bytes_after_first = bytes_written.load(Ordering::SeqCst);

    assert!(writes_after_first > 0, "First close should write data");
    assert!(bytes_after_first > 0, "First close should write bytes");

    write
      .write_frame(Frame::close(1000, b"goodbye again"))
      .await
      .unwrap();

    let writes_after_second = write_count.load(Ordering::SeqCst);
    let bytes_after_second = bytes_written.load(Ordering::SeqCst);

    assert_eq!(
      writes_after_first,
      writes_after_second,
      "Second close should NOT write any data (it wrote {} more bytes)",
      bytes_after_second - bytes_after_first
    );
  });
}

#[test]
fn is_closed_after_close_frame() {
  let (mock, _, _) = MockStream::new();
  let (_read, mut write) =
    after_handshake_split(futures_lite::io::empty(), mock, Role::Server);
  write.set_writev(false);

  futures_lite::future::block_on(async {
    assert!(!write.is_closed(), "Should not be closed initially");
    write.write_frame(Frame::close(1000, b"")).await.unwrap();
    assert!(
      write.is_closed(),
      "Should be closed after sending close frame"
    );
  });
}

#[test]
fn non_close_frame_after_close_fails() {
  let (mock, _, _) = MockStream::new();
  let (_read, mut write) =
    after_handshake_split(futures_lite::io::empty(), mock, Role::Server);
  write.set_writev(false);

  futures_lite::future::block_on(async {
    write.write_frame(Frame::close(1000, b"")).await.unwrap();

    let result = write
      .write_frame(Frame::text(b"hello".to_vec().into()))
      .await;

    assert!(
      result.is_err(),
      "Sending non-close frame after close should fail"
    );
    assert!(
      matches!(result, Err(WebSocketError::ConnectionClosed)),
      "Should get ConnectionClosed error"
    );
  });
}
