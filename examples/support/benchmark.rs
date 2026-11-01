//! Shared deterministic service for the HTTP benchmark examples.
//!
//! Keep this module deliberately small and transport-agnostic: the
//! `interop-server` exercises h12tiny's auto server layer, while
//! `h2-reference-server` calls Hyper's H2 builder directly. Sharing this
//! service makes their response body and application work identical.

use std::convert::Infallible;
use std::future::{Ready, ready};
use std::pin::Pin;
use std::task::{Context, Poll};

use std::sync::OnceLock;

use bytes::Bytes;
use h12tiny::runtime::BoxSendFuture;
use http::header::{HeaderName, HeaderValue};
use http::{HeaderMap, Request, Response, StatusCode};
use http_body::{Body, Frame};
use hyper::body::Incoming;
use hyper::service::Service;

static BODY_1K: [u8; 1024] = [b'x'; 1024];
static BODY_64K: [u8; 64 * 1024] = [b'x'; 64 * 1024];
static BODY_1M: [u8; 1024 * 1024] = [b'x'; 1024 * 1024];

/// The executor shared by benchmark endpoints.
#[derive(Clone, Copy, Debug)]
pub struct SmolExecutor;

impl hyper::rt::Executor<BoxSendFuture> for SmolExecutor {
    fn execute(&self, future: BoxSendFuture) {
        smol::spawn(future).detach();
    }
}

/// A response body that emits its complete payload in one frame.
#[derive(Debug)]
pub struct OneFrameBody(Option<Bytes>);

impl OneFrameBody {
    fn from_bytes(body: Bytes) -> Self {
        Self((!body.is_empty()).then_some(body))
    }
}

impl Body for OneFrameBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(self.0.take().map(|body| Ok(Frame::data(body))))
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_none()
    }
}

fn repeated_headers() -> &'static HeaderMap {
    static HEADERS: OnceLock<HeaderMap> = OnceLock::new();
    HEADERS.get_or_init(|| {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        for index in 0..32 {
            let name = HeaderName::from_bytes(format!("x-bench-{index:02}").as_bytes())
                .expect("benchmark header name is valid");
            headers.insert(name, HeaderValue::from_static("benchmark-header-value"));
        }
        headers
    })
}

/// Deterministic payload service shared by benchmark endpoint variants.
#[derive(Clone, Copy, Debug)]
pub struct BenchmarkService;

impl Service<Request<Incoming>> for BenchmarkService {
    type Response = Response<OneFrameBody>;
    type Error = Infallible;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn call(&self, request: Request<Incoming>) -> Self::Future {
        if request.uri().path() == "/headers" {
            let mut response = Response::new(OneFrameBody::from_bytes(Bytes::new()));
            *response.headers_mut() = repeated_headers().clone();
            return ready(Ok(response));
        }
        let (status, body) = match request.uri().path() {
            "/0" => (StatusCode::OK, Bytes::new()),
            "/1k" => (StatusCode::OK, Bytes::from_static(&BODY_1K)),
            "/64k" => (StatusCode::OK, Bytes::from_static(&BODY_64K)),
            "/1m" => (StatusCode::OK, Bytes::from_static(&BODY_1M)),
            _ => (
                StatusCode::NOT_FOUND,
                Bytes::from_static(b"use /0, /1k, /64k, /1m, or /headers\n"),
            ),
        };
        ready(Ok(Response::builder()
            .status(status)
            .header("content-type", "application/octet-stream")
            .body(OneFrameBody::from_bytes(body))
            .expect("benchmark response builder is valid")))
    }
}
