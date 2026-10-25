#![cfg(all(feature = "client", feature = "server", feature = "http2"))]

mod support;

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use async_net::TcpListener;
use futures_channel::oneshot;
use futures_util::future;
use h12tiny::client::{Client, ErrorKind, Http2Settings};
use h12tiny::io::FuturesIo;
use h12tiny::runtime::BoxExecutor;
use h12tiny::server::conn::auto;
use http::{Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper::service::Service;
use support::{ConnectionCounters, CounterSnapshot, FullBody, SmolExecutor, collect};

#[derive(Clone)]
struct DelayService {
    counters: ConnectionCounters,
}

impl Service<Request<Incoming>> for DelayService {
    type Response = Response<FullBody>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        self.counters.request_started();
        let slow = request.uri().path() == "/slow";
        Box::pin(async move {
            let _ = collect(request.into_body()).await;
            if slow {
                async_io::Timer::after(Duration::from_millis(100)).await;
            }
            Ok(Response::new(FullBody::from_static(if slow {
                b"slow"
            } else {
                b"fast"
            })))
        })
    }
}

#[test]
fn cancelling_one_h2_response_future_does_not_poison_the_shared_session() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let counters = ConnectionCounters::default();
        let server_counters = counters.clone();
        let server = smol::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            server_counters.tcp_opened();
            server_counters.h2_opened();
            auto::Builder::new(BoxExecutor::new(SmolExecutor))
                .serve_connection(
                    FuturesIo::new(stream),
                    DelayService {
                        counters: server_counters,
                    },
                )
                .await
                .unwrap();
        });

        let mut builder = Client::builder(SmolExecutor);
        builder.http2_only(true);
        builder.http2_settings(
            Http2Settings::new()
                .initial_stream_window_size(1_024)
                .initial_connection_window_size(1_024)
                .adaptive_window(false)
                .max_frame_size(16_384)
                .max_header_list_size(8_192)
                .header_table_size(0)
                .keep_alive_interval(Duration::from_secs(1))
                .keep_alive_timeout(Duration::from_secs(1))
                .keep_alive_while_idle(true),
        );
        let client = builder.build::<FullBody>();
        let warm = client
            .request(
                Request::builder()
                    .uri(format!("http://{address}/warm"))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(collect(warm.into_body()).await, b"fast");

        let slow = smol::spawn(
            client.clone().request(
                Request::builder()
                    .uri(format!("http://{address}/slow"))
                    .body(FullBody::empty())
                    .unwrap(),
            ),
        );
        for _ in 0..100 {
            if counters.snapshot().logical_requests >= 2 {
                break;
            }
            async_io::Timer::after(Duration::from_millis(1)).await;
        }
        assert_eq!(counters.snapshot().logical_requests, 2);
        drop(slow);

        let fast = client
            .request(
                Request::builder()
                    .uri(format!("http://{address}/fast"))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(fast.status(), StatusCode::OK);
        assert_eq!(collect(fast.into_body()).await, b"fast");

        // Give the cancelled handler time to observe the reset and finish;
        // only the stream is cancelled, not its H2 connection.
        async_io::Timer::after(Duration::from_millis(120)).await;
        drop(client);
        server.await;
        assert_eq!(
            counters.snapshot(),
            CounterSnapshot {
                tcp_connections: 1,
                tls_handshakes: 0,
                h1_connections: 0,
                h2_sessions: 1,
                logical_requests: 3,
            }
        );
    });
}

#[test]
fn closed_h2_session_is_evicted_and_the_next_request_reconnects() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let counters = ConnectionCounters::default();
        let server_counters = counters.clone();
        let (close_first_tx, close_first_rx) = oneshot::channel();
        let (first_closed_tx, first_closed_rx) = oneshot::channel();
        let server = smol::spawn(async move {
            let (first, _) = listener.accept().await.unwrap();
            server_counters.tcp_opened();
            server_counters.h2_opened();
            let first_builder = auto::Builder::new(BoxExecutor::new(SmolExecutor));
            let first_connection = Box::pin(
                first_builder
                    .serve_connection(
                        FuturesIo::new(first),
                        DelayService {
                            counters: server_counters.clone(),
                        },
                    )
                    .into_owned(),
            );
            let close = Box::pin(async move {
                let _ = close_first_rx.await;
            });
            // The first session can end before the shutdown signal wins the
            // race (the peer may close first) or via graceful shutdown below.
            // Either way the session is over once this match resolves: a
            // graceful completion that reports a transport error only means
            // the peer was already gone (ENOTCONN on Linux), which still
            // requires a replacement session. The request counters below
            // verify the eviction; this gate only waits for the session end.
            match future::select(first_connection, close).await {
                future::Either::Left((result, _)) => {
                    result.unwrap();
                }
                future::Either::Right(((), mut connection)) => {
                    connection.as_mut().graceful_shutdown();
                    let _ = connection.await;
                }
            };
            let _ = first_closed_tx.send(());

            let (second, _) = listener.accept().await.unwrap();
            server_counters.tcp_opened();
            server_counters.h2_opened();
            auto::Builder::new(BoxExecutor::new(SmolExecutor))
                .serve_connection(
                    FuturesIo::new(second),
                    DelayService {
                        counters: server_counters,
                    },
                )
                .await
                .unwrap();
        });

        let mut builder = Client::builder(SmolExecutor);
        builder.http2_only(true);
        let client = builder.build::<FullBody>();
        let first = client
            .request(
                Request::builder()
                    .uri(format!("http://{address}/first"))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(collect(first.into_body()).await, b"fast");
        close_first_tx.send(()).unwrap();
        first_closed_rx.await.unwrap();

        let goaway = client
            .request(
                Request::builder()
                    .uri(format!("http://{address}/goaway"))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await;
        // Once the first session's GOAWAY has completed, Hyper can either
        // reject this request on the stale sender or make it directly on the
        // replacement session. Both outcomes are safe: it must not replay a
        // stream whose first serialization might have reached the peer, and
        // the successful case below is a fresh second-session dispatch.
        let goaway_succeeded = match goaway {
            Ok(response) => {
                assert_eq!(collect(response.into_body()).await, b"fast");
                true
            }
            Err(error) => {
                assert_eq!(error.kind(), ErrorKind::SendRequest);
                false
            }
        };
        let replacement = client
            .request(
                Request::builder()
                    .uri(format!("http://{address}/replacement"))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(collect(replacement.into_body()).await, b"fast");
        drop(client);
        server.await;
        assert_eq!(
            counters.snapshot(),
            CounterSnapshot {
                tcp_connections: 2,
                tls_handshakes: 0,
                h1_connections: 0,
                h2_sessions: 2,
                logical_requests: if goaway_succeeded { 3 } else { 2 },
            }
        );
    });
}
