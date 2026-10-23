use fastwebsockets::{self as fws, after_handshake_split, Frame, OpCode, Role};
use futures_lite::io::Cursor;

fn masked_frame(fin: bool, opcode: OpCode, payload: &[u8]) -> Vec<u8> {
  let mask = [1, 2, 3, 4];
  let mut frame = Vec::new();
  let first_byte = if fin { 0x80 } else { 0x00 } | (opcode as u8);
  frame.push(first_byte);
  assert!(payload.len() < 126);
  frame.push(0x80 | payload.len() as u8);
  frame.extend_from_slice(&mask);
  frame.extend(
    payload
      .iter()
      .enumerate()
      .map(|(index, byte)| byte ^ mask[index % mask.len()]),
  );
  frame
}

fn noop_send(
  _: Frame<'_>,
) -> futures_lite::future::Ready<Result<(), fws::WebSocketError>> {
  futures_lite::future::ready(Ok(()))
}

#[test]
fn fragmented_text_rejects_incomplete_utf8_at_fin() {
  futures_lite::future::block_on(async {
    for (first, last) in [
      (&b"abc\xf0\x9f"[..], &b""[..]),
      (&b"abc"[..], &b"\xf0\x9f"[..]),
    ] {
      let mut data = masked_frame(false, OpCode::Text, first);
      data.extend(masked_frame(true, OpCode::Continuation, last));
      let (reader, _) = after_handshake_split(
        Cursor::new(data),
        futures_lite::io::sink(),
        Role::Server,
      );
      let mut messages = fws::FragmentCollectorRead::new(reader);
      assert!(matches!(
        messages.read_frame(&mut noop_send).await,
        Err(fws::WebSocketError::InvalidUTF8)
      ));
    }
  });
}

#[test]
fn fragmented_text_with_partial_utf8() {
  futures_lite::future::block_on(async {
    let mut frame1_payload = b"Hello ".to_vec();
    frame1_payload.extend_from_slice(&[0xF0, 0x9F]);
    let frame1 = masked_frame(false, OpCode::Text, &frame1_payload);

    let mut frame2_payload = vec![0x98, 0x80];
    frame2_payload.extend_from_slice(b"!");
    let frame2 = masked_frame(true, OpCode::Continuation, &frame2_payload);

    let combined_data: Vec<u8> =
      frame1.iter().chain(frame2.iter()).copied().collect();
    let (ws_read, _ws_write) = after_handshake_split(
      Cursor::new(combined_data),
      futures_lite::io::sink(),
      Role::Server,
    );
    let mut ws = fws::FragmentCollectorRead::new(ws_read);

    let frame = ws.read_frame(&mut noop_send).await.unwrap();
    assert_eq!(frame.opcode, OpCode::Text);
    assert!(frame.fin);
    let text = std::str::from_utf8(&frame.payload).unwrap();
    assert_eq!(text, "Hello 😀!");
  });
}

#[test]
fn raw_api_passes_partial_utf8_fragments() {
  futures_lite::future::block_on(async {
    let mut frame1_payload = b"Hello ".to_vec();
    frame1_payload.extend_from_slice(&[0xF0, 0x9F]);
    let frame1 = masked_frame(false, OpCode::Text, &frame1_payload);

    let mut frame2_payload = vec![0x98, 0x80];
    frame2_payload.extend_from_slice(b"!");
    let frame2 = masked_frame(true, OpCode::Continuation, &frame2_payload);

    let combined_data: Vec<u8> =
      frame1.iter().chain(frame2.iter()).copied().collect();
    let (mut ws_read, _ws_write) = after_handshake_split(
      Cursor::new(combined_data),
      futures_lite::io::sink(),
      Role::Server,
    );

    let f1 = ws_read.read_frame(&mut noop_send).await.unwrap();
    assert_eq!(f1.opcode, OpCode::Text);
    assert!(!f1.fin);

    let f2 = ws_read.read_frame(&mut noop_send).await.unwrap();
    assert_eq!(f2.opcode, OpCode::Continuation);
    assert!(f2.fin);

    let mut combined = f1.payload.to_vec();
    combined.extend_from_slice(&f2.payload);
    let text = std::str::from_utf8(&combined).unwrap();
    assert_eq!(text, "Hello 😀!");
  });
}

#[test]
fn invalid_unfragmented_utf8_rejected() {
  futures_lite::future::block_on(async {
    let invalid_utf8 = vec![
      0xce, 0xba, 0xe1, 0xbd, 0xb9, 0xcf, 0x83, 0xce, 0xbc, 0xce, 0xb5, 0xed,
      0xa0, 0x80, 0x65, 0x64, 0x69, 0x74, 0x65, 0x64,
    ];
    let frame = masked_frame(true, OpCode::Text, &invalid_utf8);

    let (ws_read, _ws_write) = after_handshake_split(
      Cursor::new(frame),
      futures_lite::io::sink(),
      Role::Server,
    );
    let mut ws = fws::FragmentCollectorRead::new(ws_read);

    let result = ws.read_frame(&mut noop_send).await;
    assert!(result.is_err());
    assert!(matches!(result, Err(fws::WebSocketError::InvalidUTF8)));
  });
}
