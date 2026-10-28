use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use fastwebsockets::{after_handshake_split, Frame, OpCode, Role};
use futures_lite::io::AsyncRead;

#[derive(Clone)]
struct ReadControl(Arc<Mutex<ReadState>>);

struct ReadState {
  chunks: VecDeque<Vec<u8>>,
  waker: Option<Waker>,
}

impl ReadControl {
  fn new(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
    Self(Arc::new(Mutex::new(ReadState {
      chunks: chunks.into_iter().collect(),
      waker: None,
    })))
  }

  fn push(&self, chunk: Vec<u8>) {
    let waker = {
      let mut state = self.0.lock().unwrap();
      state.chunks.push_back(chunk);
      state.waker.take()
    };
    if let Some(waker) = waker {
      waker.wake();
    }
  }
}

struct ControlledRead(ReadControl);

impl AsyncRead for ControlledRead {
  fn poll_read(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &mut [u8],
  ) -> Poll<io::Result<usize>> {
    let mut state = self.get_mut().0.0.lock().unwrap();
    if let Some(mut chunk) = state.chunks.pop_front() {
      let n = chunk.len().min(buf.len());
      buf[..n].copy_from_slice(&chunk[..n]);
      if n < chunk.len() {
        chunk.drain(..n);
        state.chunks.push_front(chunk);
      }
      Poll::Ready(Ok(n))
    } else {
      state.waker = Some(cx.waker().clone());
      Poll::Pending
    }
  }
}

fn masked_frame(first: u8, payload: &[u8]) -> Vec<u8> {
  let mask = [0x13, 0x37, 0x42, 0xa5];
  let mut frame = vec![first];
  match payload.len() {
    len @ 0..=125 => frame.push(0x80 | len as u8),
    len @ 126..=65535 => {
      frame.push(0x80 | 126);
      frame.extend_from_slice(&(len as u16).to_be_bytes());
    }
    len => {
      frame.push(0x80 | 127);
      frame.extend_from_slice(&(len as u64).to_be_bytes());
    }
  }
  frame.extend_from_slice(&mask);
  frame.extend(
    payload
      .iter()
      .enumerate()
      .map(|(index, byte)| byte ^ mask[index % mask.len()]),
  );
  frame
}

fn masked_binary_frame(payload: &[u8]) -> Vec<u8> {
  masked_frame(0x82, payload)
}

fn noop_send(
  _: Frame<'_>,
) -> futures_lite::future::Ready<Result<(), fastwebsockets::WebSocketError>> {
  futures_lite::future::ready(Ok(()))
}

fn assert_resumes_after_cancellation(
  payload: Vec<u8>,
  initial_chunks: Vec<Vec<u8>>,
) {
  futures_lite::future::block_on(async move {
    let original = masked_binary_frame(&payload);
    let next = masked_binary_frame(b"next");
    let buffered_len = initial_chunks.iter().map(Vec::len).sum::<usize>();
    let control = ReadControl::new(initial_chunks);
    let reader = ControlledRead(control.clone());
    let (mut ws_read, _) = after_handshake_split(
      reader,
      futures_lite::io::sink(),
      Role::Server,
    );
    let mut send = noop_send;
    let mut pending_read = Box::pin(ws_read.read_frame(&mut send));
    assert!(futures_lite::future::poll_once(&mut pending_read)
      .await
      .is_none());
    drop(pending_read);

    let mut remainder = original[buffered_len..].to_vec();
    remainder.extend_from_slice(&next);
    control.push(remainder);

    let frame = ws_read.read_frame(&mut send).await.unwrap();
    assert!(frame.fin);
    assert_eq!(frame.opcode, OpCode::Binary);
    assert_eq!(&*frame.payload, payload);

    let frame = ws_read.read_frame(&mut send).await.unwrap();
    assert!(frame.fin);
    assert_eq!(frame.opcode, OpCode::Binary);
    assert_eq!(&*frame.payload, b"next");
  });
}

#[test]
fn cancellation_while_waiting_for_extended_length_preserves_frame() {
  for (length, code) in [(126, 0xfe), (65536, 0xff)] {
    assert_resumes_after_cancellation(vec![b'x'; length], vec![vec![0x82, code]]);
  }
}

#[test]
fn cancellation_while_waiting_for_mask_preserves_frame() {
  let frame = masked_binary_frame(b"hello");
  assert_resumes_after_cancellation(b"hello".to_vec(), vec![frame[..2].to_vec()]);
}

#[test]
fn cancellation_while_waiting_for_payload_preserves_frame() {
  let frame = masked_binary_frame(b"hello");
  assert_resumes_after_cancellation(
    b"hello".to_vec(),
    vec![frame[..2].to_vec(), frame[2..6].to_vec()],
  );
}

#[derive(Clone, Default)]
struct SendProbe(Arc<Mutex<SendState>>);

#[derive(Default)]
struct SendState {
  stalled: bool,
  sent: Vec<(OpCode, Vec<u8>)>,
}

fn stallable_send(
  probe: SendProbe,
) -> impl for<'frame> FnMut(Frame<'frame>)
  -> Pin<Box<dyn Future<Output = Result<(), fastwebsockets::WebSocketError>> + 'frame>> {
  move |frame: Frame<'_>| {
    let probe = probe.clone();
    Box::pin(async move {
      if probe.0.lock().unwrap().stalled {
        futures_lite::future::pending::<()>().await;
      }
      probe
        .0
        .lock()
        .unwrap()
        .sent
        .push((frame.opcode, frame.payload.to_vec()));
      Ok::<(), fastwebsockets::WebSocketError>(())
    })
  }
}

#[test]
fn cancellation_during_pong_send_resends_reply() {
  futures_lite::future::block_on(async move {
    let mut input = masked_frame(0x89, b"hello");
    input.extend_from_slice(&masked_binary_frame(b"next"));
    let probe = SendProbe::default();
    probe.0.lock().unwrap().stalled = true;
    let control = ReadControl::new([input]);
    let (mut ws_read, _) = after_handshake_split(
      ControlledRead(control),
      futures_lite::io::sink(),
      Role::Server,
    );
    let mut send = stallable_send(probe.clone());
    let mut pending_read = Box::pin(ws_read.read_frame(&mut send));
    assert!(futures_lite::future::poll_once(&mut pending_read)
      .await
      .is_none());
    drop(pending_read);

    assert!(probe.0.lock().unwrap().sent.is_empty());

    probe.0.lock().unwrap().stalled = false;
    let frame = ws_read.read_frame(&mut send).await.unwrap();
    assert_eq!(frame.opcode, OpCode::Binary);
    assert_eq!(&*frame.payload, b"next");

    let sent = &probe.0.lock().unwrap().sent;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, OpCode::Pong);
    assert_eq!(sent[0].1, b"hello");
  });
}

#[test]
fn cancellation_during_close_reply_resends_reply() {
  futures_lite::future::block_on(async move {
    let mut payload = 1000u16.to_be_bytes().to_vec();
    payload.extend_from_slice(b"bye");
    let input = masked_frame(0x88, &payload);
    let probe = SendProbe::default();
    probe.0.lock().unwrap().stalled = true;
    let control = ReadControl::new([input]);
    let (mut ws_read, _) = after_handshake_split(
      ControlledRead(control),
      futures_lite::io::sink(),
      Role::Server,
    );
    let mut send = stallable_send(probe.clone());
    let mut pending_read = Box::pin(ws_read.read_frame(&mut send));
    assert!(futures_lite::future::poll_once(&mut pending_read)
      .await
      .is_none());
    drop(pending_read);

    assert!(probe.0.lock().unwrap().sent.is_empty());

    probe.0.lock().unwrap().stalled = false;
    let frame = ws_read.read_frame(&mut send).await.unwrap();
    assert_eq!(frame.opcode, OpCode::Close);
    assert_eq!(&frame.payload[..2], &1000u16.to_be_bytes());

    let sent = &probe.0.lock().unwrap().sent;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, OpCode::Close);
    assert_eq!(sent[0].1, payload);
  });
}

#[test]
fn cancellation_during_pong_send_resends_reply_fragment_collector() {
  futures_lite::future::block_on(async move {
    let mut input = masked_frame(0x89, b"hello");
    input.extend_from_slice(&masked_binary_frame(b"next"));
    let probe = SendProbe::default();
    probe.0.lock().unwrap().stalled = true;
    let control = ReadControl::new([input]);
    let (ws_read, _) = after_handshake_split(
      ControlledRead(control),
      futures_lite::io::sink(),
      Role::Server,
    );
    let mut collector =
      fastwebsockets::FragmentCollectorRead::new(ws_read);
    let mut send = stallable_send(probe.clone());
    let mut pending_read = Box::pin(collector.read_frame(&mut send));
    assert!(futures_lite::future::poll_once(&mut pending_read)
      .await
      .is_none());
    drop(pending_read);

    assert!(probe.0.lock().unwrap().sent.is_empty());

    probe.0.lock().unwrap().stalled = false;
    let frame = collector.read_frame(&mut send).await.unwrap();
    assert_eq!(frame.opcode, OpCode::Binary);
    assert_eq!(&*frame.payload, b"next");

    let sent = &probe.0.lock().unwrap().sent;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, OpCode::Pong);
    assert_eq!(sent[0].1, b"hello");
  });
}
