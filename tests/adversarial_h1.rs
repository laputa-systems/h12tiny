#![cfg(all(feature = "server", feature = "http1"))]

mod support;

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::time::Duration;

use async_net::{TcpListener, TcpStream};
use futures_lite::io::{AsyncReadExt, AsyncWriteExt};
use futures_util::future::{self, Either};
use h12tiny::io::FuturesIo;
use h12tiny::runtime::BoxExecutor;
use h12tiny::server::conn::auto;
use http::{Request, Response};
use http_body::Body;
use hyper::body::Incoming;
use hyper::service::Service;
use support::{FullBody, SmolExecutor};

#[derive(Clone)]
struct DrainService;

impl Service<Request<Incoming>> for DrainService {
    type Response = Response<FullBody>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        Box::pin(async move {
            let mut body = request.into_body();
            while let Some(frame) =
                futures_util::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
            {
                if frame.is_err() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Hyper rejected invalid request framing",
                    ));
                }
            }
            Ok(Response::new(FullBody::empty()))
        })
    }
}

async fn server_rejects(bytes: &'static [u8]) -> bool {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = smol::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        auto::Builder::new(BoxExecutor::new(SmolExecutor))
            .http1_only()
            .serve_connection(FuturesIo::new(stream), DrainService)
            .await
    });
    let mut client = TcpStream::connect(address).await.unwrap();
    client.write_all(bytes).await.unwrap();
    client.close().await.unwrap();
    match future::select(server, async_io::Timer::after(Duration::from_secs(1))).await {
        Either::Left((result, _)) => result.is_err(),
        Either::Right(_) => false,
    }
}

async fn server_status_line(bytes: &'static [u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let _server = smol::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        auto::Builder::new(BoxExecutor::new(SmolExecutor))
            .http1_only()
            .serve_connection(FuturesIo::new(stream), DrainService)
            .await
    });
    let mut client = TcpStream::connect(address).await.unwrap();
    client.write_all(bytes).await.unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    let read = async {
        while !head.ends_with(b"\r\n\r\n") {
            if client.read(&mut byte).await.unwrap() == 0 {
                break;
            }
            head.push(byte[0]);
        }
    };
    future::select(
        Box::pin(read),
        Box::pin(async_io::Timer::after(Duration::from_secs(5))),
    )
    .await;
    String::from_utf8_lossy(&head)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn hyper_h1_serves_bare_lf_request_like_upstream_hyper() {
    smol::block_on(async {
        let status = server_status_line(b"GET / HTTP/1.1\nHost: test\n\n").await;
        assert_eq!(status, "HTTP/1.1 200 OK");
    });
}

#[test]
fn hyper_h1_rejects_ambiguous_or_malformed_raw_framing() {
    smol::block_on(async {
        // This corpus is intentionally raw. It proves that the endpoint layer
        // feeds its server parser byte-for-byte and does not weaken Hyper's
        // framing decisions with a convenience pre-parser.
        let cases = [
            (
                "duplicate_content_length",
                b"POST / HTTP/1.1\r\nHost: test\r\nContent-Length: 4\r\nContent-Length: 4\r\n\r\ntest".as_slice(),
            ),
            (
                "conflicting_content_length",
                b"POST / HTTP/1.1\r\nHost: test\r\nContent-Length: 4\r\nContent-Length: 5\r\n\r\ntest".as_slice(),
            ),
            (
                "content_length_and_transfer_encoding",
                b"POST / HTTP/1.1\r\nHost: test\r\nContent-Length: 4\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n".as_slice(),
            ),
            (
                "invalid_transfer_encoding_order",
                b"POST / HTTP/1.1\r\nHost: test\r\nTransfer-Encoding: chunked, gzip\r\n\r\n".as_slice(),
            ),
            (
                "malformed_chunk_size",
                b"POST / HTTP/1.1\r\nHost: test\r\nTransfer-Encoding: chunked\r\n\r\nnot-hex\r\n".as_slice(),
            ),
            (
                "truncated_chunk",
                b"POST / HTTP/1.1\r\nHost: test\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nab".as_slice(),
            ),
            (
                "whitespace_before_colon",
                b"GET / HTTP/1.1\r\nHost : test\r\n\r\n".as_slice(),
            ),
            (
                "invalid_header_name",
                b"GET / HTTP/1.1\r\nBad Header: test\r\n\r\n".as_slice(),
            ),
            (
                "premature_eof",
                b"POST / HTTP/1.1\r\nHost: test\r\nContent-Length: 4\r\n\r\nab".as_slice(),
            ),
            (
                "extra_bytes_after_framed_body",
                b"POST / HTTP/1.1\r\nHost: test\r\nContent-Length: 0\r\n\r\ntrailing".as_slice(),
            ),
        ];
        for (name, bytes) in cases {
            assert!(
                server_rejects(bytes).await,
                "Hyper accepted unsafe case {name}"
            );
        }
    });
}
