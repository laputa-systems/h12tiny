#![cfg(all(
    feature = "client",
    feature = "server",
    feature = "http1",
    feature = "tls",
    not(feature = "http2")
))]

mod support;

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_net::{TcpListener, TcpStream};
use futures_rustls::{TlsAcceptor, pki_types::CertificateDer};
use h12tiny::client::{
    Client, ClientTlsConfigBuilder, Connector, ErrorKind, TcpConnected, TcpDialFuture, TcpDialer,
};
use h12tiny::runtime::BoxExecutor;
use h12tiny::server::conn::auto;
use http::{Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper::service::Service;
use support::{FullBody, SmolExecutor, collect, fixture_server_config};

#[derive(Clone, Copy)]
struct FixtureTcpDialer;

impl TcpDialer for FixtureTcpDialer {
    fn connect(&self, origin: http::Uri, _: h12tiny::client::RequestOptions) -> TcpDialFuture {
        let host = origin.host().expect("fixture origin has host").to_owned();
        let port = origin.port_u16().expect("fixture origin has port");
        Box::pin(async move {
            let stream = TcpStream::connect(format!("{host}:{port}"))
                .await
                .map_err(|error| Box::new(error) as h12tiny::client::DialError)?;
            let local_addr = stream.local_addr().ok();
            let peer_addr = stream.peer_addr().ok();
            Ok(TcpConnected::new(stream).with_addresses(local_addr, peer_addr))
        })
    }
}

#[derive(Clone, Copy)]
struct OkService;

impl Service<Request<Incoming>> for OkService {
    type Response = Response<FullBody>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, _: Request<Incoming>) -> Self::Future {
        Box::pin(async { Ok(Response::new(FullBody::from_static(b"ok"))) })
    }
}

#[test]
fn h1_only_build_default_negotiates_with_a_dual_protocol_tls_peer() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = smol::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let tls = TlsAcceptor::from(Arc::new(fixture_server_config()))
                .accept(stream)
                .await
                .unwrap();
            assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
            auto::Builder::new(BoxExecutor::new(SmolExecutor))
                .serve_tls_connection(tls, OkService)
                .unwrap()
                .await
                .unwrap();
        });

        let certificate = CertificateDer::from(include_bytes!("fixtures/tls/cert.der").to_vec());
        let tls_config = ClientTlsConfigBuilder::new()
            .add_root_certificate(certificate)
            .unwrap()
            .build()
            .unwrap();
        let connector = Connector::builder()
            .tcp_dialer(FixtureTcpDialer)
            .tls_config(tls_config)
            .build();
        let mut builder = Client::builder(SmolExecutor);
        builder.connector(connector);
        let client = builder.build::<FullBody>();

        let response = client
            .request(
                Request::get(format!("https://localhost:{port}/h1-only"))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(collect(response.into_body()).await, b"ok");
        drop(client);
        server.await;
    });
}

#[test]
fn h1_only_build_preserves_explicit_custom_alpn_policy() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = smol::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let tls = TlsAcceptor::from(Arc::new(fixture_server_config()))
                .accept(stream)
                .await
                .unwrap();
            assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
        });

        let certificate = CertificateDer::from(include_bytes!("fixtures/tls/cert.der").to_vec());
        let tls_config = ClientTlsConfigBuilder::new()
            .add_root_certificate(certificate)
            .unwrap()
            .alpn_protocols([b"h2".to_vec(), b"http/1.1".to_vec()])
            .build()
            .unwrap();
        let connector = Connector::builder()
            .tcp_dialer(FixtureTcpDialer)
            .tls_config(tls_config)
            .build();
        let mut builder = Client::builder(SmolExecutor);
        builder.connector(connector);
        let client = builder.build::<FullBody>();

        let error = client
            .request(
                Request::get(format!("https://localhost:{port}/custom-alpn"))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ProtocolUnavailable);
        drop(client);
        server.await;
    });
}
