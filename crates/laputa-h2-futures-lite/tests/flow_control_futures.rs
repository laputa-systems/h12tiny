use futures_lite::future::zip;
use futures_lite::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use h2::client;
use http::{Method, Request, StatusCode};
use laputa_protocol_test_support::executor::block_on_with_limit;
use laputa_protocol_test_support::pipe::BudgetPipe;
use std::io;
use std::time::Duration;

const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
const PAYLOAD_LEN: usize = 16_384;

struct Frame {
    kind: u8,
    flags: u8,
    stream_id: u32,
    payload: Vec<u8>,
}

async fn read_frame<T: AsyncRead + Unpin>(io: &mut T) -> io::Result<Frame> {
    let mut header = [0; 9];
    io.read_exact(&mut header).await?;

    let payload_len = (usize::from(header[0]) << 16)
        | (usize::from(header[1]) << 8)
        | usize::from(header[2]);
    let mut payload = vec![0; payload_len];
    io.read_exact(&mut payload).await?;

    let stream_id = u32::from_be_bytes(header[5..9].try_into().unwrap()) & 0x7fff_ffff;

    Ok(Frame {
        kind: header[3],
        flags: header[4],
        stream_id,
        payload,
    })
}

async fn write_data<T: AsyncWrite + Unpin>(io: &mut T, end_stream: bool) {
    let mut frame = Vec::with_capacity(9 + PAYLOAD_LEN);
    frame.extend_from_slice(&[
        (PAYLOAD_LEN >> 16) as u8,
        (PAYLOAD_LEN >> 8) as u8,
        PAYLOAD_LEN as u8,
        0,
        u8::from(end_stream),
        0,
        0,
        0,
        1,
    ]);
    frame.resize(9 + PAYLOAD_LEN, 0);
    io.write_all(&frame).await.unwrap();
}

#[test]
fn release_capacity_sends_both_window_updates_and_allows_more_data() {
    let (client_io, mut peer_io) = BudgetPipe::pair(usize::MAX);

    let peer = async move {
        let mut preface = [0; 24];
        peer_io.read_exact(&mut preface).await.unwrap();
        assert_eq!(&preface, PREFACE);

        let settings = read_frame(&mut peer_io).await.unwrap();
        assert_eq!(settings.kind, 4);
        assert_eq!(settings.flags, 0);
        assert_eq!(settings.stream_id, 0);
        assert!(settings.payload.is_empty());

        peer_io
            .write_all(&[0, 0, 0, 4, 0, 0, 0, 0, 0])
            .await
            .unwrap();

        let settings_ack = read_frame(&mut peer_io).await.unwrap();
        assert_eq!(settings_ack.kind, 4);
        assert_eq!(settings_ack.flags, 1);
        assert_eq!(settings_ack.stream_id, 0);
        assert!(settings_ack.payload.is_empty());

        peer_io
            .write_all(&[0, 0, 0, 4, 1, 0, 0, 0, 0])
            .await
            .unwrap();

        let request = read_frame(&mut peer_io).await.unwrap();
        assert_eq!(request.kind, 1);
        assert_eq!(request.stream_id, 1);

        peer_io
            .write_all(&[0, 0, 1, 1, 4, 0, 0, 0, 1, 0x88])
            .await
            .unwrap();
        for _ in 0..3 {
            write_data(&mut peer_io, false).await;
        }

        let connection_update = read_frame(&mut peer_io).await.unwrap();
        assert_eq!(connection_update.kind, 8);
        assert_eq!(connection_update.stream_id, 0);
        assert_eq!(connection_update.payload, 32_768u32.to_be_bytes());

        let stream_update = read_frame(&mut peer_io).await.unwrap();
        assert_eq!(stream_update.kind, 8);
        assert_eq!(stream_update.stream_id, 1);
        assert_eq!(stream_update.payload, 32_768u32.to_be_bytes());

        write_data(&mut peer_io, true).await;
    };

    let client = async move {
        let (mut sender, connection) = client::handshake(client_io).await.unwrap();
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://http2.akamai.com/")
            .body(())
            .unwrap();

        let request = async move {
            let response = sender.send_request(request, true).unwrap().0.await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);

            let mut body = response.into_parts().1;
            for _ in 0..3 {
                let data = body.data().await.unwrap().unwrap();
                assert_eq!(data.len(), PAYLOAD_LEN);
            }

            body.flow_control()
                .release_capacity(PAYLOAD_LEN * 2)
                .unwrap();

            let data = body.data().await.unwrap().unwrap();
            assert_eq!(data.len(), PAYLOAD_LEN);
            assert!(body.data().await.is_none());
        };

        let (connection, ()) = zip(connection, request).await;
        connection.unwrap();
    };

    block_on_with_limit(zip(peer, client), Duration::from_secs(2))
        .expect("flow-control exchange completes without a lost wake");
}
