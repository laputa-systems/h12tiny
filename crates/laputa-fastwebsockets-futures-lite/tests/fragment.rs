use fastwebsockets::{
  self as fws, after_handshake_split, Frame, OpCode, Role, WebSocketError,
};
use futures_lite::io::Cursor;

fn raw_frame(fin: bool, opcode: OpCode, payload: &[u8]) -> Vec<u8> {
  let mut frame = Vec::new();
  let first_byte = if fin { 0x80 } else { 0x00 } | (opcode as u8);
  frame.push(first_byte);
  let len = payload.len();
  if len < 126 {
    frame.push(len as u8);
  } else if len < 65536 {
    frame.push(126);
    frame.extend_from_slice(&(len as u16).to_be_bytes());
  } else {
    frame.push(127);
    frame.extend_from_slice(&(len as u64).to_be_bytes());
  }
  frame.extend_from_slice(payload);
  frame
}

fn masked_frame(fin: bool, opcode: OpCode, payload: &[u8]) -> Vec<u8> {
  let mask = [1, 2, 3, 4];
  let mut frame = Vec::new();
  let first_byte = if fin { 0x80 } else { 0x00 } | (opcode as u8);
  frame.push(first_byte);
  let len = payload.len();
  assert!(len < 126);
  frame.push(0x80 | len as u8);
  frame.extend_from_slice(&mask);
  frame.extend(
    payload
      .iter()
      .enumerate()
      .map(|(index, byte)| byte ^ mask[index % mask.len()]),
  );
  frame
}

fn assert_frame_too_large<T>(result: Result<T, WebSocketError>) {
  assert!(matches!(result, Err(WebSocketError::FrameTooLarge)));
}

fn noop_send(
  _: Frame<'_>,
) -> futures_lite::future::Ready<Result<(), WebSocketError>> {
  futures_lite::future::ready(Ok(()))
}

#[test]
fn split_fragment_collector_rejects_message_restarts() {
  futures_lite::future::block_on(async {
    for initial in [OpCode::Text, OpCode::Binary] {
      for restart in [OpCode::Text, OpCode::Binary] {
        for fin in [false, true] {
          let mut data = masked_frame(false, initial, b"original");
          data.extend(masked_frame(fin, restart, b"replacement"));
          data.extend(masked_frame(true, OpCode::Continuation, b"!"));
          let (reader, _) = after_handshake_split(
            Cursor::new(data),
            futures_lite::io::sink(),
            Role::Server,
          );
          let mut messages = fws::FragmentCollectorRead::new(reader);
          assert!(matches!(
            messages.read_frame(&mut noop_send).await,
            Err(WebSocketError::InvalidFragment)
          ));
        }
      }
    }
  });
}

#[test]
fn split_fragment_collector_keeps_messages_across_ping() {
  futures_lite::future::block_on(async {
    for opcode in [OpCode::Text, OpCode::Binary] {
      let mut data = masked_frame(false, opcode, b"original");
      data.extend(masked_frame(true, OpCode::Ping, b"ping"));
      data.extend(masked_frame(true, OpCode::Continuation, b"!"));
      let (reader, _) = after_handshake_split(
        Cursor::new(data),
        futures_lite::io::sink(),
        Role::Server,
      );
      let mut messages = fws::FragmentCollectorRead::new(reader);
      let mut replies = Vec::new();
      let frame = messages.read_frame(&mut |frame: Frame<'_>| {
        replies.push((frame.opcode, frame.payload.to_vec()));
        futures_lite::future::ready(Ok::<_, WebSocketError>(()))
      }).await.unwrap();
      assert_eq!(frame.opcode, opcode);
      assert_eq!(&*frame.payload, b"original!");
      assert!(frame.fin);
      assert_eq!(replies.len(), 1);
      assert_eq!(replies[0].0, OpCode::Pong);
      assert_eq!(replies[0].1, b"ping");
    }
  });
}

#[test]
fn split_fragment_collector_rejects_aggregate_binary_over_limit() {
  futures_lite::future::block_on(async {
    let frames = vec![
      raw_frame(false, OpCode::Binary, b"12345"),
      raw_frame(true, OpCode::Continuation, b"67890"),
    ];
    let data: Vec<u8> = frames.iter().flatten().copied().collect();

    let (mut ws_read, _ws_write) = after_handshake_split(
      Cursor::new(data),
      futures_lite::io::sink(),
      Role::Client,
    );
    ws_read.set_max_message_size(9);
    let mut ws = fws::FragmentCollectorRead::new(ws_read);

    assert_frame_too_large(ws.read_frame(&mut noop_send).await);
  });
}

#[test]
fn split_fragment_collector_rejects_aggregate_text_over_limit() {
  futures_lite::future::block_on(async {
    let frames = vec![
      raw_frame(false, OpCode::Text, b"hello"),
      raw_frame(true, OpCode::Continuation, b"world"),
    ];
    let data: Vec<u8> = frames.iter().flatten().copied().collect();

    let (mut ws_read, _ws_write) = after_handshake_split(
      Cursor::new(data),
      futures_lite::io::sink(),
      Role::Client,
    );
    ws_read.set_max_message_size(9);
    let mut ws = fws::FragmentCollectorRead::new(ws_read);

    assert_frame_too_large(ws.read_frame(&mut noop_send).await);
  });
}

#[test]
fn server_rejects_unmasked_client_frames() {
  futures_lite::future::block_on(async {
    let (mut ws_read, _ws_write) = after_handshake_split(
      Cursor::new(raw_frame(true, OpCode::Text, b"hello")),
      futures_lite::io::sink(),
      Role::Server,
    );

    let result = ws_read.read_frame(&mut noop_send).await;
    assert!(matches!(result, Err(WebSocketError::UnmaskedFrame)));
  });
}

#[test]
fn server_unmasks_masked_client_frames() {
  futures_lite::future::block_on(async {
    let (mut ws_read, _ws_write) = after_handshake_split(
      Cursor::new(masked_frame(true, OpCode::Text, b"hello")),
      futures_lite::io::sink(),
      Role::Server,
    );

    let frame = ws_read.read_frame(&mut noop_send).await.unwrap();
    assert_eq!(frame.payload, &b"hello"[..]);
  });
}

#[test]
fn control_frames_reject_payloads_larger_than_125_bytes() {
  futures_lite::future::block_on(async {
    for opcode in [OpCode::Close, OpCode::Ping, OpCode::Pong] {
      let payload = vec![0; 126];
      let (mut ws_read, _ws_write) = after_handshake_split(
        Cursor::new(raw_frame(true, opcode, &payload)),
        futures_lite::io::sink(),
        Role::Client,
      );

      let result = ws_read.read_frame(&mut noop_send).await;
      match opcode {
        OpCode::Ping => {
          assert!(matches!(result, Err(WebSocketError::PingFrameTooLarge)));
        }
        OpCode::Close | OpCode::Pong => {
          assert!(matches!(result, Err(WebSocketError::ControlFrameTooLarge)));
        }
        _ => unreachable!(),
      }
    }
  });
}
