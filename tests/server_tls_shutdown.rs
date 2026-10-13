#![cfg(all(feature = "server", feature = "http1", feature = "http2", feature = "tls"))]

mod support;

use std::convert::Infallible;
use std::future::Future;
use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use async_net::{TcpListener, TcpStream};
use futures_channel::oneshot;
use futures_lite::io::{AsyncReadExt, AsyncWriteExt};
use futures_rustls::{TlsAcceptor, TlsConnector};
use futures_util::future::{self, Either};
use h12tiny::io::FuturesIo;
use h12tiny::runtime::BoxExecutor;
use h12tiny::server::conn::auto;
use http::{Request, Response};
use hyper::service::service_fn;
use support::{FullBody, SmolExecutor, collect, fixture_client_config, fixture_server_config};

type ServerTls = futures_rustls::server::TlsStream<TcpStream>;
type ClientTls = futures_rustls::client::TlsStream<TcpStream>;

async fn bounded<F: Future>(future: F) -> F::Output {
    match future::select(Box::pin(future), async_io::Timer::after(Duration::from_secs(2))).await {
        Either::Left((result, _)) => result,
        Either::Right(_) => panic!("connection operation did not complete"),
    }
}

async fn tls_pair(alpn: Option<&[u8]>) -> (ServerTls, ClientTls) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let protocols = alpn.into_iter().map(<[u8]>::to_vec).collect::<Vec<_>>();
    let mut server_config = fixture_server_config();
    server_config.alpn_protocols = protocols.clone();
    let mut client_config = fixture_client_config();
    client_config.alpn_protocols = protocols;
    let server = async move {
        let (stream, _) = listener.accept().await.unwrap();
        TlsAcceptor::from(Arc::new(server_config)).accept(stream).await.unwrap()
    };
    let client = async move {
        let stream = TcpStream::connect(address).await.unwrap();
        TlsConnector::from(Arc::new(client_config))
            .connect(rustls::pki_types::ServerName::try_from("localhost").unwrap(), stream)
            .await.unwrap()
    };
    bounded(future::join(server, client)).await
}

#[test]
fn tls_shutdown_cancels_incomplete_h2_preface_without_peer_disconnect() {
    smol::block_on(async {
        for prefix in [b"".as_slice(), b"PRI * HTTP/2.0\r\n".as_slice()] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let errors = Arc::new(AtomicUsize::new(0));
            let observed_errors = errors.clone();
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            let mut config = fixture_server_config();
            config.alpn_protocols = vec![b"h2".to_vec()];
            let server = smol::spawn(h12tiny::server::serve_tls(
                listener, TlsAcceptor::from(Arc::new(config)),
                service_fn(|_| async { Ok::<_, Infallible>(Response::new(FullBody::empty())) }),
                BoxExecutor::new(SmolExecutor),
            ).on_connection_error(move |_| { observed_errors.fetch_add(1, Ordering::SeqCst); })
                .shutdown_on(async { shutdown_rx.await.unwrap(); }));
            let stream = TcpStream::connect(address).await.unwrap();
            let mut config = fixture_client_config();
            config.alpn_protocols = vec![b"h2".to_vec()];
            let mut peer = TlsConnector::from(Arc::new(config))
                .connect(rustls::pki_types::ServerName::try_from("localhost").unwrap(), stream)
                .await.unwrap();
            peer.write_all(prefix).await.unwrap();
            peer.flush().await.unwrap();
            // SETTINGS proves HTTP setup has begun after the TLS handshake.
            let mut header = [0; 9];
            bounded(peer.read_exact(&mut header)).await.unwrap();
            assert_eq!(header[3], 4);
            shutdown_tx.send(()).unwrap();
            bounded(server).await.unwrap();
            assert_eq!(errors.load(Ordering::SeqCst), 0);
            // The peer remains owned and connected until after server cleanup.
            let mut remaining = Vec::new();
            if let Err(error) = bounded(peer.read_to_end(&mut remaining)).await {
                assert!(matches!(error.kind(),
                    std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset));
            }
        }
    });
}

#[test]
fn restricted_tls_builder_serves_matching_alpn() {
    smol::block_on(async {
        for (h2, protocol) in [(false, Some(b"http/1.1".as_slice())),
                              (true, Some(b"h2".as_slice())), (false, None)] {
            let (tls, mut peer) = tls_pair(protocol).await;
            let builder = auto::Builder::new(BoxExecutor::new(SmolExecutor));
            let builder = if h2 { builder.http2_only() } else { builder.http1_only() };
            let connection = builder.serve_tls_connection(tls, service_fn(|_| async {
                Ok::<_, Infallible>(Response::new(FullBody::from_static(b"served")))
            })).unwrap();
            let server = smol::spawn(connection);
            if h2 {
                let (mut sender, connection) = hyper::client::conn::http2::handshake(
                    BoxExecutor::new(SmolExecutor), FuturesIo::new(peer),
                ).await.unwrap();
                let driver = smol::spawn(connection);
                let response = bounded(sender.send_request(Request::get("https://localhost/")
                    .body(FullBody::empty()).unwrap())).await.unwrap();
                assert_eq!(collect(response.into_body()).await, b"served");
                drop(sender);
                bounded(driver).await.unwrap();
            } else {
                peer.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await.unwrap();
                peer.flush().await.unwrap();
                let mut response = Vec::new();
                bounded(peer.read_to_end(&mut response)).await.unwrap();
                assert!(response.starts_with(b"HTTP/1.1 200"));
                assert!(response.windows(6).any(|window| window == b"served"));
            }
            bounded(server).await.unwrap();
        }
    });
}

#[test]
fn restricted_tls_builder_returns_error_for_conflicting_alpn_and_missing_h2_alpn() {
    smol::block_on(async {
        for (h2, protocol) in [(true, Some(b"http/1.1".as_slice())),
                              (false, Some(b"h2".as_slice())), (true, None)] {
            let (tls, _peer) = tls_pair(protocol).await;
            let builder = auto::Builder::new(BoxExecutor::new(SmolExecutor));
            let builder = if h2 { builder.http2_only() } else { builder.http1_only() };
            let result = builder.serve_tls_connection(tls, service_fn(|_| async {
                Ok::<_, Infallible>(Response::new(FullBody::empty()))
            }));
            let error = match result { Err(error) => error, Ok(_) => panic!("TLS restriction ignored") };
            assert_eq!(error.downcast_ref::<std::io::Error>().unwrap().kind(), std::io::ErrorKind::Unsupported);
        }
    });
}


#[test]
fn h2_setup_shutdown_racing_full_preface_cancels_before_driver_consumes_it() {
    smol::block_on(async {
        for poll_setup in [false, true] {
            let (tls, mut peer) = tls_pair(Some(b"h2")).await;
            let builder = auto::Builder::new(BoxExecutor::new(SmolExecutor)).http2_only();
            let mut connection = Box::pin(builder.serve_tls_connection(tls, service_fn(|_| async {
                Ok::<_, Infallible>(Response::new(FullBody::empty()))
            })).unwrap());
            if poll_setup {
                future::poll_fn(|cx| {
                    assert!(connection.as_mut().poll(cx).is_pending());
                    Poll::Ready(())
                }).await;
            }
            peer.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n").await.unwrap();
            peer.flush().await.unwrap();
            connection.as_mut().graceful_shutdown();
            bounded(connection).await.unwrap();
            let mut remaining = Vec::new();
            if let Err(error) = bounded(peer.read_to_end(&mut remaining)).await {
                assert!(matches!(error.kind(),
                    std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset));
            }
        }
    });
}

#[test]
fn h2_only_cleartext_setup_shutdown_cancels_partial_preface() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server = smol::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let builder = auto::Builder::new(BoxExecutor::new(SmolExecutor)).http2_only();
            let connection = Box::pin(builder.serve_connection(FuturesIo::new(stream), service_fn(|_| async {
                Ok::<_, Infallible>(Response::new(FullBody::empty()))
            })));
            match future::select(connection, shutdown_rx).await {
                Either::Left(_) => panic!("peer was closed before shutdown"),
                Either::Right((signal, mut connection)) => {
                    signal.unwrap();
                    connection.as_mut().graceful_shutdown();
                    connection.await.unwrap();
                }
            }
        });
        let mut peer = TcpStream::connect(address).await.unwrap();
        peer.write_all(b"PRI * HTTP/2.0\r\n").await.unwrap();
        let mut header = [0; 9];
        bounded(peer.read_exact(&mut header)).await.unwrap();
        assert_eq!(header[3], 4);
        shutdown_tx.send(()).unwrap();
        bounded(server).await;
        let mut remaining = Vec::new();
        if let Err(error) = bounded(peer.read_to_end(&mut remaining)).await {
            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
        }
    });
}

#[test]
fn tls_h2_shutdown_drains_a_started_stream_after_full_preface() {
    smol::block_on(async {
        let (tls, peer) = tls_pair(Some(b"h2")).await;
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let gate = Arc::new(Mutex::new(Some((started_tx, release_rx))));
        let builder = auto::Builder::new(BoxExecutor::new(SmolExecutor)).http2_only();
        let connection = Box::pin(builder.serve_tls_connection(tls, service_fn(move |_| {
            let (started, release) = gate.lock().unwrap().take().unwrap();
            started.send(()).unwrap();
            async move {
                release.await.unwrap();
                Ok::<_, Infallible>(Response::new(FullBody::from_static(b"drained")))
            }
        })).unwrap());
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (draining_tx, draining_rx) = oneshot::channel();
        let server = smol::spawn(async move {
            match future::select(connection, shutdown_rx).await {
                Either::Left(_) => panic!("active connection ended before shutdown"),
                Either::Right((signal, mut connection)) => {
                    signal.unwrap();
                    connection.as_mut().graceful_shutdown();
                    draining_tx.send(()).unwrap();
                    connection.await.unwrap();
                }
            }
        });
        let (mut sender, connection) = hyper::client::conn::http2::handshake(
            BoxExecutor::new(SmolExecutor), FuturesIo::new(peer),
        ).await.unwrap();
        let client_driver = smol::spawn(connection);
        let mut response = smol::spawn(async move {
            sender.send_request(Request::get("https://localhost/drain")
                .body(FullBody::empty()).unwrap()).await.unwrap()
        });
        bounded(started_rx).await.unwrap();
        shutdown_tx.send(()).unwrap();
        bounded(draining_rx).await.unwrap();
        assert!(future::poll_fn(|cx| Poll::Ready(Pin::new(&mut response).poll(cx).is_pending())).await);
        release_tx.send(()).unwrap();
        let response = bounded(response).await;
        assert_eq!(bounded(collect(response.into_body())).await, b"drained");
        bounded(server).await;
        bounded(client_driver).await.unwrap();
    });
}
