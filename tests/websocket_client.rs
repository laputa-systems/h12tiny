#![cfg(all(
    feature = "server",
    feature = "web",
    feature = "websocket",
    feature = "websocket-client",
    feature = "http1"
))]

use async_net::{TcpListener, TcpStream};
use futures_lite::io::{AsyncReadExt, AsyncWriteExt};
use h12tiny::io::FuturesIo;
use h12tiny::runtime::{BoxExecutor, BoxSendFuture};
use h12tiny::server::conn::auto;
use h12tiny::web::{
    connect_websocket, get, Router, WebSocketConnectError, WebSocketFrame, WebSocketOpCode,
    WebSocketPayload, WebSocketUpgrade,
};
use http::Uri;

#[derive(Clone, Copy, Debug)]
struct TestExecutor;

impl hyper::rt::Executor<BoxSendFuture> for TestExecutor {
    fn execute(&self, future: BoxSendFuture) {
        std::thread::spawn(move || futures_lite::future::block_on(future));
    }
}

fn echo_server() -> std::net::SocketAddr {
    let listener = futures_lite::future::block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        futures_lite::future::block_on(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let router = Router::<()>::new().route(
                "/echo",
                get(|upgrade: WebSocketUpgrade| async move {
                    let response = upgrade.response();
                    std::thread::spawn(move || {
                        futures_lite::future::block_on(async move {
                            let (mut reader, mut writer) =
                                upgrade.into_connection().await.unwrap().split();
                            let frame = reader
                                .read_frame(&mut |_| async { Ok::<(), std::io::Error>(()) })
                                .await
                                .unwrap();
                            writer
                                .write_frame(WebSocketFrame::new(
                                    true,
                                    frame.opcode,
                                    None,
                                    frame.payload,
                                ))
                                .await
                                .unwrap();
                        });
                    });
                    response
                }),
            );
            auto::Builder::new(BoxExecutor::new(TestExecutor))
                .http1_only()
                .serve_connection(FuturesIo::new(stream), router)
                .await
                .unwrap();
        });
    });
    address
}

fn raw_peer(response: &'static [u8]) -> std::net::SocketAddr {
    let listener = futures_lite::future::block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        futures_lite::future::block_on(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            let mut byte = [0_u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).await.unwrap() == 0 {
                    return;
                }
                head.push(byte[0]);
            }
            stream.write_all(response).await.unwrap();
            stream.flush().await.unwrap();
            let _ = stream.read(&mut byte).await;
        });
    });
    address
}

async fn connect(address: std::net::SocketAddr, path: &str) -> Result<h12tiny::web::WebSocketConnection, WebSocketConnectError> {
    let stream = TcpStream::connect(address).await.unwrap();
    let uri: Uri = format!("ws://{address}{path}").parse().unwrap();
    connect_websocket(stream, &uri).await
}

#[test]
fn a_client_handshake_exchanges_masked_frames_with_the_server_adapter() {
    let address = echo_server();
    futures_lite::future::block_on(async {
        let (mut reader, mut writer) = connect(address, "/echo").await.unwrap().split();
        writer
            .write_frame(WebSocketFrame::text(WebSocketPayload::Borrowed(b"hello")))
            .await
            .unwrap();
        let frame = reader
            .read_frame(&mut |_| async { Ok::<(), std::io::Error>(()) })
            .await
            .unwrap();
        assert_eq!(frame.opcode, WebSocketOpCode::Text);
        assert_eq!(&*frame.payload, b"hello");
    });
}

#[test]
fn a_mismatched_accept_value_is_rejected() {
    let address = raw_peer(
        b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n",
    );
    let error = futures_lite::future::block_on(connect(address, "/")).err().unwrap();
    assert!(matches!(error, WebSocketConnectError::Accept), "{error}");
}

#[test]
fn a_refused_upgrade_reports_its_status() {
    let address = raw_peer(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
    let error = futures_lite::future::block_on(connect(address, "/missing")).err().unwrap();
    assert!(
        matches!(error, WebSocketConnectError::Status(status) if status == http::StatusCode::NOT_FOUND),
        "{error}"
    );
}
