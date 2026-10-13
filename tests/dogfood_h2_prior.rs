#![cfg(all(feature = "client", feature = "server", feature = "http2"))]

use std::convert::Infallible;
use std::future::{Ready, ready};
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll};
use std::time::Duration;

use async_net::TcpListener;
use bytes::Bytes;
use futures_util::future::{self, Either};
use h12tiny::client::Client;
#[cfg(not(feature = "http1"))]
use h12tiny::client::{ConnectionProtocol, ResponseInfo};
use h12tiny::io::FuturesIo;
use h12tiny::runtime::{BoxExecutor, BoxSendFuture};
use h12tiny::server::conn::auto;
use http::{Request, Response, StatusCode};
use http_body::{Body, Frame};
use hyper::body::Incoming;
use hyper::service::Service;

#[derive(Clone)]
struct SmolExecutor;

impl hyper::rt::Executor<BoxSendFuture> for SmolExecutor {
    fn execute(&self, future: BoxSendFuture) {
        smol::spawn(future).detach();
    }
}

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

#[derive(Clone)]
struct EmptyService;

impl Service<Request<Incoming>> for EmptyService {
    type Response = Response<EmptyBody>;
    type Error = Infallible;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn call(&self, _: Request<Incoming>) -> Self::Future {
        ready(Ok(Response::new(EmptyBody)))
    }
}

#[derive(Clone)]
struct CountingService(Arc<AtomicUsize>);

impl Service<Request<Incoming>> for CountingService {
    type Response = Response<EmptyBody>;
    type Error = Infallible;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn call(&self, _: Request<Incoming>) -> Self::Future {
        self.0.fetch_add(1, Ordering::SeqCst);
        ready(Ok(Response::new(EmptyBody)))
    }
}

#[test]
fn h2_client_and_auto_server_interoperate_over_cleartext_prior_knowledge() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = smol::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            auto::Builder::new(BoxExecutor::new(SmolExecutor))
                .serve_connection(FuturesIo::new(stream), EmptyService)
                .await
                .unwrap();
        });

        let mut builder = Client::builder(SmolExecutor);
        builder.http2_only(true);
        let client = builder.build::<EmptyBody>();
        let response = client
            .request(
                Request::builder()
                    .uri(format!("http://{address}/prior-knowledge"))
                    .body(EmptyBody)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        drop(response);
        // The pool owns an idle shared H2 sender until the client is dropped.
        // End that ownership before asserting the peer driver completes.
        drop(client);
        server.await;
    });
}

#[cfg(not(feature = "http1"))]
#[test]
fn h2_only_build_default_uses_cleartext_prior_knowledge() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = smol::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            auto::Builder::new(BoxExecutor::new(SmolExecutor))
                .serve_connection(FuturesIo::new(stream), EmptyService)
                .await
                .unwrap();
        });

        let client = Client::builder(SmolExecutor).build::<EmptyBody>();
        let response = client
            .request(
                Request::builder()
                    .uri(format!("http://{address}/default-prior-knowledge"))
                    .body(EmptyBody)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let info = *ResponseInfo::from_response(&response).unwrap();
        assert_eq!(info.connection().protocol(), ConnectionProtocol::Http2);
        drop(response);
        drop(client);
        server.await;
    });
}

#[test]
fn concurrent_first_h2_requests_share_one_establishment_and_session() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let served = Arc::new(AtomicUsize::new(0));
        let server_count = served.clone();
        let server = smol::spawn(async move {
            // Accept exactly one socket: a duplicate H2 establishment would
            // leave one of the client futures parked and fail the deadline.
            let (stream, _) = listener.accept().await.unwrap();
            auto::Builder::new(BoxExecutor::new(SmolExecutor))
                .serve_connection(FuturesIo::new(stream), CountingService(server_count))
                .await
                .unwrap();
        });

        let mut builder = Client::builder(SmolExecutor);
        builder.http2_only(true);
        let client = builder.build::<EmptyBody>();
        let requests = (0..16)
            .map(|index| {
                client.clone().request(
                    Request::builder()
                        .uri(format!("http://{address}/stream/{index}"))
                        .body(EmptyBody)
                        .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let deadline = async_io::Timer::after(Duration::from_secs(2));
        let responses = match future::select(future::join_all(requests), deadline).await {
            Either::Left((responses, _)) => responses,
            Either::Right(_) => panic!("concurrent H2 requests did not converge on one session"),
        };
        for response in responses {
            assert_eq!(response.unwrap().status(), StatusCode::OK);
        }
        drop(client);
        server.await;
        assert_eq!(served.load(Ordering::SeqCst), 16);
    });
}
