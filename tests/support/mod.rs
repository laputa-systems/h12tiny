#![allow(dead_code)]

pub mod tls_system;

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll};

use bytes::Bytes;
use h12tiny::runtime::BoxSendFuture;
use http_body::{Body, Frame};

#[derive(Clone)]
pub struct SmolExecutor;

impl hyper::rt::Executor<BoxSendFuture> for SmolExecutor {
    fn execute(&self, future: BoxSendFuture) {
        smol::spawn(future).detach();
    }
}

/// A finite body that produces one data frame. It lets integration tests cover
/// request and response payloads without adding a body-helper dependency.
#[derive(Debug)]
pub struct FullBody(Option<Bytes>);

impl FullBody {
    pub fn empty() -> Self {
        Self(None)
    }

    pub fn from_static(bytes: &'static [u8]) -> Self {
        Self(Some(Bytes::from_static(bytes)))
    }

    pub fn from_bytes(bytes: Bytes) -> Self {
        Self(Some(bytes))
    }
}

impl Body for FullBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(self.get_mut().0.take().map(|bytes| Ok(Frame::data(bytes))))
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_none()
    }
}

/// A body that intentionally yields between chunks, exercising Hyper's body
/// streaming state rather than only a single immediately-ready frame.
#[derive(Debug)]
pub struct YieldingBody {
    chunks: std::collections::VecDeque<Bytes>,
    yielded: bool,
}

impl YieldingBody {
    pub fn new(chunks: impl IntoIterator<Item = Bytes>) -> Self {
        Self {
            chunks: chunks.into_iter().collect(),
            yielded: false,
        }
    }
}

impl Body for YieldingBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if !self.yielded && !self.chunks.is_empty() {
            self.yielded = true;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        self.yielded = false;
        Poll::Ready(self.chunks.pop_front().map(|bytes| Ok(Frame::data(bytes))))
    }

    fn is_end_stream(&self) -> bool {
        self.chunks.is_empty()
    }
}

pub async fn collect<B>(mut body: B) -> Vec<u8>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Debug,
{
    let mut data = Vec::new();
    while let Some(frame) =
        futures_util::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
    {
        let frame = frame.unwrap();
        if let Ok(bytes) = frame.into_data() {
            data.extend_from_slice(&bytes);
        }
    }
    data
}

/// Test-side, protocol-agnostic observation. The server harness increments
/// these values at its actual connection/handshake/session boundaries; tests
/// never infer pooling merely from successful responses.
#[derive(Clone, Default)]
pub struct ConnectionCounters(Arc<CounterState>);

#[derive(Default)]
struct CounterState {
    pub tcp_connections: AtomicUsize,
    pub tls_handshakes: AtomicUsize,
    pub h1_connections: AtomicUsize,
    pub h2_sessions: AtomicUsize,
    pub logical_requests: AtomicUsize,
}

impl ConnectionCounters {
    pub fn tcp_opened(&self) {
        self.0.tcp_connections.fetch_add(1, Ordering::SeqCst);
    }

    pub fn tls_completed(&self) {
        self.0.tls_handshakes.fetch_add(1, Ordering::SeqCst);
    }

    pub fn h1_opened(&self) {
        self.0.h1_connections.fetch_add(1, Ordering::SeqCst);
    }

    pub fn h2_opened(&self) {
        self.0.h2_sessions.fetch_add(1, Ordering::SeqCst);
    }

    pub fn request_started(&self) {
        self.0.logical_requests.fetch_add(1, Ordering::SeqCst);
    }

    pub fn snapshot(&self) -> CounterSnapshot {
        CounterSnapshot {
            tcp_connections: self.0.tcp_connections.load(Ordering::SeqCst),
            tls_handshakes: self.0.tls_handshakes.load(Ordering::SeqCst),
            h1_connections: self.0.h1_connections.load(Ordering::SeqCst),
            h2_sessions: self.0.h2_sessions.load(Ordering::SeqCst),
            logical_requests: self.0.logical_requests.load(Ordering::SeqCst),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct CounterSnapshot {
    pub tcp_connections: usize,
    pub tls_handshakes: usize,
    pub h1_connections: usize,
    pub h2_sessions: usize,
    pub logical_requests: usize,
}

#[cfg(feature = "tls")]
pub fn fixture_server_config() -> rustls::ServerConfig {
    use futures_rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let certificate = CertificateDer::from(include_bytes!("../fixtures/tls/cert.der").to_vec());
    let key = PrivateKeyDer::try_from(include_bytes!("../fixtures/tls/key.der").to_vec())
        .expect("fixture key is valid PKCS#8 DER");
    let provider = Arc::new(rustls_graviola::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("Graviola supports Rustls' safe default protocol versions")
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .expect("fixture certificate and key match");
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    config
}

#[cfg(feature = "tls")]
pub fn fixture_client_config() -> rustls::ClientConfig {
    use futures_rustls::pki_types::CertificateDer;

    let certificate = CertificateDer::from(include_bytes!("../fixtures/tls/cert.der").to_vec());
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(certificate)
        .expect("fixture certificate is a valid root");
    let provider = Arc::new(rustls_graviola::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("Graviola supports Rustls' safe default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    config
}
