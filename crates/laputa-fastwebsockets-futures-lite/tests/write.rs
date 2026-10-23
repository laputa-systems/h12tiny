use fastwebsockets::Frame;
use futures_lite::{future::block_on, io::AsyncWrite};
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

struct Writer {
  bytes: Vec<u8>,
  chunk: usize,
  zero_after: Option<usize>,
  zero_writes: usize,
}

impl AsyncWrite for Writer {
  fn poll_write(
    mut self: Pin<&mut Self>,
    _: &mut Context<'_>,
    data: &[u8],
  ) -> Poll<io::Result<usize>> {
    if self.zero_after == Some(self.bytes.len()) {
      self.zero_writes += 1;
      return Poll::Ready(if self.zero_writes > 1 {
        Err(io::Error::other("writer retried after zero progress"))
      } else {
        Ok(0)
      });
    }
    let count = data.len().min(self.chunk);
    self.bytes.extend_from_slice(&data[..count]);
    Poll::Ready(Ok(count))
  }

  fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
    Poll::Ready(Ok(()))
  }

  fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
    Poll::Ready(Ok(()))
  }
}

#[test]
fn vectored_frame_writes_report_zero_progress() {
  for zero_after in [0, 1] {
    let mut writer = Writer {
      bytes: Vec::new(),
      chunk: 1,
      zero_after: Some(zero_after),
      zero_writes: 0,
    };
    let mut frame = Frame::binary(b"payload".to_vec().into());
    let error = block_on(frame.writev(&mut writer)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WriteZero);
    assert_eq!(writer.zero_writes, 1);
  }
}

#[test]
fn vectored_frame_writes_complete_partial_headers_and_payloads() {
  let payload = vec![42; 1025];
  for chunk in [1, 3, 4, 5, 1024, 2048] {
    let mut writer = Writer {
      bytes: Vec::new(),
      chunk,
      zero_after: None,
      zero_writes: 0,
    };
    let mut frame = Frame::binary(payload.clone().into());
    block_on(frame.writev(&mut writer)).unwrap();
    assert_eq!(&writer.bytes[..4], &[0x82, 126, 4, 1]);
    assert_eq!(&writer.bytes[4..], &payload);
  }
}
