#![cfg(all(feature = "client", feature = "server", feature = "http1"))]

mod support;

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_net::TcpListener;
use bytes::Bytes;
use futures_util::future;
use h12tiny::client::{Client, DebugEvent, DebugEventLog};
use h12tiny::io::FuturesIo;
use h12tiny::runtime::BoxExecutor;
use h12tiny::server::conn::auto;
use http::header::CONTENT_LENGTH;
use http::{Method, Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper::service::Service;
use support::{ConnectionCounters, CounterSnapshot, SmolExecutor, YieldingBody, collect};

#[derive(Debug, PartialEq, Eq)]
struct RequestRecord {
    method: Method,
    body: Vec<u8>,
    content_length: Option<String>,
}

#[derive(Clone)]
struct RecordingService {
    counters: ConnectionCounters,
    requests: Arc<Mutex<Vec<RequestRecord>>>,
}

impl Service<Request<Incoming>> for RecordingService {
    type Response = Response<YieldingBody>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        self.counters.request_started();
        let records = self.requests.clone();
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let content_length = parts
                .headers
                .get(CONTENT_LENGTH)
                .map(|value| value.to_str().unwrap().to_owned());
            let body = collect(body).await;
            records.lock().unwrap().push(RequestRecord {
                method: parts.method,
                body: body.clone(),
                content_length,
            });
            let reply = [b"reply:".as_slice(), body.as_slice()].concat();
            Ok(Response::builder()
                .header(CONTENT_LENGTH, reply.len())
                .body(YieldingBody::new([
                    Bytes::from_static(b"reply:"),
                    Bytes::from(body),
                ]))
                .unwrap())
        })
    }
}

fn yielding(bytes: &[u8]) -> YieldingBody {
    YieldingBody::new([Bytes::copy_from_slice(bytes)])
}

fn empty() -> YieldingBody {
    YieldingBody::new(std::iter::empty::<Bytes>())
}

#[test]
fn plaintext_h1_dogfood_covers_methods_lengths_streaming_and_keep_alive() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let counters = ConnectionCounters::default();
        let records = Arc::new(Mutex::new(Vec::new()));
        let server_counters = counters.clone();
        let server_records = records.clone();
        let server = smol::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            server_counters.tcp_opened();
            server_counters.h1_opened();
            auto::Builder::new(BoxExecutor::new(SmolExecutor))
                .serve_connection(
                    FuturesIo::new(stream),
                    RecordingService {
                        counters: server_counters,
                        requests: server_records,
                    },
                )
                .await
                .unwrap();
        });

        let client = Client::builder(SmolExecutor).build::<YieldingBody>();
        let cases = [
            (Method::GET, "/empty", Vec::new(), false),
            (Method::POST, "/small", b"small".to_vec(), false),
            (Method::POST, "/large", vec![b'x'; 64 * 1024], false),
            (Method::POST, "/stream", b"stream".to_vec(), false),
            (Method::HEAD, "/head", Vec::new(), true),
        ];

        for (method, path, body, is_head) in cases {
            let request_body = if path == "/stream" {
                YieldingBody::new([Bytes::from_static(b"str"), Bytes::from_static(b"eam")])
            } else if body.is_empty() {
                empty()
            } else {
                yielding(&body)
            };
            let response = client
                .request(
                    Request::builder()
                        .method(method)
                        .uri(format!("http://{address}{path}"))
                        .header(CONTENT_LENGTH, body.len())
                        .body(request_body)
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let received = collect(response.into_body()).await;
            if is_head {
                assert!(received.is_empty());
            } else {
                assert_eq!(received, [b"reply:".as_slice(), body.as_slice()].concat());
            }
        }
        drop(client);
        server.await;

        let records = records.lock().unwrap();
        assert_eq!(records.len(), 5);
        assert_eq!(records[0].method, Method::GET);
        assert_eq!(records[1].body, b"small");
        assert_eq!(records[2].body.len(), 64 * 1024);
        assert_eq!(records[3].body, b"stream");
        assert_eq!(records[4].method, Method::HEAD);
        assert!(records.iter().all(|record| {
            record.content_length.as_deref() == Some(record.body.len().to_string().as_str())
        }));
        assert_eq!(
            counters.snapshot(),
            CounterSnapshot {
                tcp_connections: 1,
                tls_handshakes: 0,
                h1_connections: 1,
                h2_sessions: 0,
                logical_requests: 5,
            }
        );
    });
}

#[test]
fn concurrent_h1_requests_get_unique_connections_then_reuse_them() {
    const CONCURRENT: usize = 16;

    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let counters = ConnectionCounters::default();
        let records = Arc::new(Mutex::new(Vec::new()));
        let server_counters = counters.clone();
        let server_records = records.clone();
        let accept_loop = smol::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                server_counters.tcp_opened();
                server_counters.h1_opened();
                let connection_counters = server_counters.clone();
                let connection_records = server_records.clone();
                smol::spawn(async move {
                    let _ = auto::Builder::new(BoxExecutor::new(SmolExecutor))
                        .http1_only()
                        .serve_connection(
                            FuturesIo::new(stream),
                            RecordingService {
                                counters: connection_counters,
                                requests: connection_records,
                            },
                        )
                        .await;
                })
                .detach();
            }
        });

        let pool_events = DebugEventLog::default();
        let mut builder = Client::builder(SmolExecutor);
        builder.debug_event_log(pool_events.clone());
        let client = builder.build::<YieldingBody>();
        let concurrent = (0..CONCURRENT).map(|request| {
            let client = client.clone();
            async move {
                let response = client
                    .request(
                        Request::builder()
                            .uri(format!("http://{address}/concurrent/{request}"))
                            .body(empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(collect(response.into_body()).await, b"reply:");
            }
        });
        future::join_all(concurrent).await;

        // Unique H1 checkouts return to the pool from a driver task after the
        // response body finishes. A fixed sleep cannot prove those returns
        // completed: on a loaded executor the warm phase races them and each
        // miss dials an extra socket. Wait for every concurrent checkout to
        // check back in, then prove the warm phase needs no new socket.
        let settle = Instant::now() + Duration::from_secs(10);
        let mut pooled = 0;
        while pooled < CONCURRENT {
            if Instant::now() > settle {
                panic!(
                    "timed out waiting for {CONCURRENT} pooled returns, observed {pooled}: {:?}",
                    pool_events.drain()
                );
            }
            async_io::Timer::after(Duration::from_millis(1)).await;
            pooled += pool_events
                .drain()
                .into_iter()
                .filter(|event| matches!(event, DebugEvent::ConnectionPooled { .. }))
                .count();
        }
        let settled = counters.snapshot().tcp_connections;
        for request in 0..CONCURRENT {
            let response = client
                .request(
                    Request::builder()
                        .uri(format!("http://{address}/warm/{request}"))
                        .body(empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(collect(response.into_body()).await, b"reply:");
        }
        assert_eq!(
            counters.snapshot(),
            CounterSnapshot {
                tcp_connections: settled,
                tls_handshakes: 0,
                h1_connections: settled,
                h2_sessions: 0,
                logical_requests: CONCURRENT * 2,
            }
        );
        drop(client);
        drop(accept_loop);
    });
}
