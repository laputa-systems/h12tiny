#![cfg(all(
    feature = "client",
    feature = "server",
    feature = "web",
    feature = "http1",
    feature = "http2",
    feature = "tls"
))]

mod support;

use std::sync::Arc;

use async_net::TcpListener;
use futures_channel::oneshot;
use futures_rustls::TlsAcceptor;
use h12tiny::client::{Client, Connector, Protocol};
use h12tiny::runtime::BoxExecutor;
use h12tiny::server::serve_tls;
use h12tiny::util::{self, BoxBody, ResponseBodyExt};
use h12tiny::web::{Router, get};
use http::{Request, StatusCode};
use support::{SmolExecutor, fixture_client_config, fixture_server_config};

async fn serve_web_tls_once(http2: bool) -> (u16, oneshot::Sender<()>, smol::Task<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = smol::spawn(async move {
        let mut config = fixture_server_config();
        config.alpn_protocols = if http2 {
            vec![b"h2".to_vec()]
        } else {
            vec![b"http/1.1".to_vec()]
        };
        let router = Router::<()>::new().route("/web", get(|| async { "tls-router" }));
        serve_tls(
            listener,
            TlsAcceptor::from(Arc::new(config)),
            router,
            BoxExecutor::new(SmolExecutor),
        )
        .shutdown_on(async move {
            let _ = shutdown_rx.await;
        })
        .await
        .unwrap();
    });
    (port, shutdown_tx, server)
}

async fn request_web_tls(http2: bool) {
    let (port, shutdown_tx, server) = serve_web_tls_once(http2).await;
    let mut builder = Client::builder(SmolExecutor);
    builder.connector(Connector::with_tls_config(fixture_client_config()));
    if http2 {
        builder.protocol(Protocol::Http2Only);
    }
    let client = builder.build::<BoxBody>();
    let response = client
        .request(
            Request::get(format!("https://localhost:{port}/web"))
                .body(util::boxed_body(util::empty_body()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text_limited(64).await.unwrap(), "tls-router");
    drop(client);
    shutdown_tx.send(()).unwrap();
    server.await;
}

#[test]
fn router_is_protocol_neutral_over_tls_h1_and_h2() {
    smol::block_on(async {
        request_web_tls(false).await;
        request_web_tls(true).await;
    });
}
