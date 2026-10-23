use fastwebsockets::{after_handshake_split, Frame, Role};
use futures_lite::{future::block_on, io};

#[test]
fn masked_frames_round_trip_after_entropy_acquisition() {
  let payload = b"message";
  let mut frame = Frame::binary(payload.to_vec().into());
  frame.mask().expect("OS entropy should be available");
  let mut buffer = Vec::new();
  let bytes = frame.write(&mut buffer);
  assert_eq!(bytes[1] & 0x80, 0x80);
  let key = &bytes[2..6];
  let decoded: Vec<_> = bytes[6..].iter().enumerate()
    .map(|(index, byte)| byte ^ key[index % 4]).collect();
  assert_eq!(decoded, payload);
}

#[test]
fn client_writers_mask_and_server_writers_leave_payloads_unmasked() {
  block_on(async {
    for role in [Role::Client, Role::Server] {
      let mut bytes = io::Cursor::new(Vec::new());
      let (_, mut writer) = after_handshake_split(
        io::empty(), &mut bytes, role,
      );
      writer.write_frame(Frame::binary(b"message".to_vec().into())).await.unwrap();
      drop(writer);
      assert_eq!(bytes.into_inner()[1] & 0x80, if role == Role::Client { 0x80 } else { 0 });
    }
  });
}
