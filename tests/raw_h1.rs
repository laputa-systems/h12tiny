#![cfg(all(feature = "client", feature = "http1"))]

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use async_net::TcpListener;
use bytes::Bytes;
use futures_channel::oneshot;
use futures_lite::io::{AsyncReadExt, AsyncWriteExt};
use futures_util::future::{self, Either};
use h12tiny::client::{
    Client, ConnectionProtocol, DebugEvent, DebugEventLog, ErrorKind, RequestOptions,
};
use h12tiny::runtime::BoxSendFuture;
use http::{Request, StatusCode};
use http_body::{Body, Frame};

/// The client owns no task runtime: tests supply this narrow executor bridge.
#[derive(Clone)]
struct SmolExecutor;

impl hyper::rt::Executor<BoxSendFuture> for SmolExecutor {
    fn execute(&self, future: BoxSendFuture) {
        smol::spawn(future).detach();
    }
}

/// A zero-length request body without adding an HTTP-body helper dependency.
struct EmptyBody;

impl Body for EmptyBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(None)
    }

    fn is_end_stream(&self) -> bool {
        true
    }
}

#[test]
fn direct_h1_uses_origin_form_and_synthesizes_host_on_the_wire() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = smol::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let count = socket.read(&mut request).await.unwrap();
            request.truncate(count);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            request
        });

        let events = DebugEventLog::default();
        let mut builder = Client::builder(SmolExecutor);
        builder.debug_event_log(events.clone());
        let client = builder.build::<EmptyBody>();
        let uri = format!("http://{address}/foo?x=1");
        let response = client
            .request(Request::builder().uri(uri).body(EmptyBody).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        drop(response);

        let request = peer.await;
        let wire = std::str::from_utf8(&request).unwrap();
        assert!(
            wire.starts_with(&format!("GET /foo?x=1 HTTP/1.1\r\nHost: {address}\r\n")),
            "unexpected wire request: {wire:?}"
        );
        assert!(!wire.starts_with("GET http://"));
        let events = events.drain();
        assert!(events.iter().any(|event| matches!(
            event,
            DebugEvent::PoolCheckout { origin } if origin == &format!("http://{address}")
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            DebugEvent::ConnectionEstablished {
                origin,
                protocol: ConnectionProtocol::Http1,
            } if origin == &format!("http://{address}")
        )));
    });
}

#[test]
fn request_headers_timeout_cancels_only_the_pending_exchange() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = smol::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            async_io::Timer::after(Duration::from_millis(20)).await;
        });

        let client = Client::builder(SmolExecutor).build::<EmptyBody>();
        let result = client
            .request_with_options(
                Request::builder()
                    .uri(format!("http://{address}/pending-headers"))
                    .body(EmptyBody)
                    .unwrap(),
                RequestOptions::new().with_headers_timeout(Duration::ZERO),
            )
            .await;

        assert!(matches!(result, Err(error) if error.kind() == ErrorKind::HeadersTimeout));
        peer.await;
    });
}

async fn read_head(stream: &mut async_net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut byte = [0; 1];
    loop {
        let count = stream.read(&mut byte).await.unwrap();
        assert_ne!(count, 0, "peer closed before completing request headers");
        request.extend_from_slice(&byte[..count]);
        if request.ends_with(b"\r\n\r\n") {
            return request;
        }
    }
}

#[test]
fn sequential_h1_requests_reuse_one_direct_connection() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = smol::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let first = read_head(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            let second = read_head(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            (first, second)
        });

        let events = DebugEventLog::default();
        let mut builder = Client::builder(SmolExecutor);
        builder.debug_event_log(events.clone());
        let client = builder.build::<EmptyBody>();
        let first_response = client
            .request(
                Request::builder()
                    .uri(format!("http://{address}/one"))
                    .body(EmptyBody)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first_response.status(), StatusCode::OK);
        drop(first_response);

        // The zero TCP deadline would reject a new socket. A successful
        // second exchange proves this pooled connection is neither keyed by
        // options nor subjected to a fresh connection deadline.
        let second_response = client
            .request_with_options(
                Request::builder()
                    .uri(format!("http://{address}/two"))
                    .body(EmptyBody)
                    .unwrap(),
                RequestOptions::new().with_connect_timeout(Duration::ZERO),
            )
            .await
            .unwrap();
        assert_eq!(second_response.status(), StatusCode::OK);
        drop(second_response);
        assert!(events.drain().iter().any(|event| matches!(
            event,
            DebugEvent::ConnectionPooled { origin } if origin == &format!("http://{address}")
        )));
        drop(client);

        let deadline = async_io::Timer::after(Duration::from_secs(2));
        let (first, second) = match future::select(peer, deadline).await {
            Either::Left((requests, _)) => requests,
            Either::Right(_) => panic!("second request did not reuse the open HTTP/1 connection"),
        };
        assert!(
            std::str::from_utf8(&first)
                .unwrap()
                .starts_with("GET /one HTTP/1.1")
        );
        assert!(
            std::str::from_utf8(&second)
                .unwrap()
                .starts_with("GET /two HTTP/1.1")
        );
    });
}

#[test]
fn closed_h1_session_is_not_reused_and_next_request_reconnects() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = smol::spawn(async move {
            for response in [
                b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".as_slice(),
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".as_slice(),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let _request = read_head(&mut socket).await;
                socket.write_all(response).await.unwrap();
            }
        });

        let client = Client::builder(SmolExecutor).build::<EmptyBody>();
        for path in ["/closed", "/reconnected"] {
            let response = client
                .request(
                    Request::builder()
                        .uri(format!("http://{address}{path}"))
                        .body(EmptyBody)
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            drop(response);
        }
        drop(client);

        let deadline = async_io::Timer::after(Duration::from_secs(2));
        match future::select(peer, deadline).await {
            Either::Left(((), _)) => {}
            Either::Right(_) => panic!("client did not reconnect after server closed HTTP/1"),
        }
    });
}

#[test]
fn explicit_host_and_connect_authority_form_are_preserved_on_the_wire() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = smol::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let explicit_host = read_head(&mut first).await;
            first
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            drop(first);

            let (mut second, _) = listener.accept().await.unwrap();
            let connect = read_head(&mut second).await;
            second
                .write_all(b"HTTP/1.1 200 Connection Established\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            (explicit_host, connect)
        });

        let client = Client::builder(SmolExecutor).build::<EmptyBody>();
        let response = client
            .request(
                Request::builder()
                    .uri(format!("http://{address}/explicit-host"))
                    .header("Host", "chosen.example")
                    .body(EmptyBody)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        drop(response);

        // The first server response closes its socket so this request cannot
        // accidentally reuse it; authority-form itself is the target under
        // test, not reuse behavior.
        let response = client
            .request(
                Request::builder()
                    .method("CONNECT")
                    .uri(address.to_string())
                    .body(EmptyBody)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        drop(response);
        drop(client);

        let (explicit_host, connect) = peer.await;
        let explicit_host = std::str::from_utf8(&explicit_host).unwrap();
        assert!(
            explicit_host.starts_with("GET /explicit-host HTTP/1.1\r\nHost: chosen.example\r\n")
        );
        let connect = std::str::from_utf8(&connect).unwrap();
        assert!(connect.starts_with(&format!(
            "CONNECT {address} HTTP/1.1\r\nHost: {address}\r\n"
        )));
    });
}

#[test]
fn client_classifies_relative_and_unsupported_schemes() {
    smol::block_on(async {
        let client = Client::builder(SmolExecutor).build::<EmptyBody>();
        let relative = client
            .request(
                Request::builder()
                    .uri("/only-a-path")
                    .body(EmptyBody)
                    .unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(relative.kind(), ErrorKind::AbsoluteUriRequired);

        let unsupported = client
            .request(
                Request::builder()
                    .uri("ftp://example.test/file")
                    .body(EmptyBody)
                    .unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(unsupported.kind(), ErrorKind::UnsupportedScheme);
    });
}

#[test]
fn ipv6_literal_synthesizes_a_bracketed_host_header() {
    smol::block_on(async {
        let listener = TcpListener::bind("[::1]:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = smol::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_head(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            request
        });
        let client = Client::builder(SmolExecutor).build::<EmptyBody>();
        let response = client
            .request(
                Request::builder()
                    .uri(format!(
                        "http://[{ip}]:{port}/ipv6",
                        ip = "::1",
                        port = address.port()
                    ))
                    .body(EmptyBody)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        drop(response);
        drop(client);
        let request = String::from_utf8(peer.await).unwrap();
        assert!(request.starts_with(&format!(
            "GET /ipv6 HTTP/1.1\r\nHost: [::1]:{}\r\n",
            address.port()
        )));
    });
}

#[test]
fn cancelling_h1_request_closes_that_session_before_a_later_request() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (started_tx, started_rx) = oneshot::channel();
        let peer = smol::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let _request = read_head(&mut first).await;
            let _ = started_tx.send(());
            let mut byte = [0; 1];
            assert_eq!(
                first.read(&mut byte).await.unwrap(),
                0,
                "cancelled H1 session remained open"
            );

            let (mut second, _) = listener.accept().await.unwrap();
            let request = read_head(&mut second).await;
            second
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            request
        });

        let client = Client::builder(SmolExecutor).build::<EmptyBody>();
        let cancelled = smol::spawn(
            client.clone().request(
                Request::builder()
                    .uri(format!("http://{address}/cancelled"))
                    .body(EmptyBody)
                    .unwrap(),
            ),
        );
        started_rx.await.unwrap();
        drop(cancelled);

        let response = client
            .request(
                Request::builder()
                    .uri(format!("http://{address}/later"))
                    .body(EmptyBody)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        drop(response);
        drop(client);
        let deadline = async_io::Timer::after(Duration::from_secs(2));
        let request = match future::select(peer, deadline).await {
            Either::Left((request, _)) => request,
            Either::Right(_) => panic!("later request did not reconnect after H1 cancellation"),
        };
        assert!(
            String::from_utf8(request)
                .unwrap()
                .starts_with("GET /later HTTP/1.1")
        );
    });
}
