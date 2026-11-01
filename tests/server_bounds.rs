#![cfg(all(feature = "server", feature = "http1"))]

mod support;

use std::convert::Infallible;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_net::{TcpListener, TcpStream};
use futures_channel::oneshot;
use futures_lite::io::{AsyncReadExt, AsyncWriteExt};
use futures_util::future::{self, Either};
use h12tiny::runtime::BoxExecutor;
use h12tiny::server::{ProductionBounds, serve};
use http::Response;
use hyper::service::service_fn;
use support::{FullBody, SmolExecutor};

fn bounds(header_read_timeout: Duration, max_connections: usize) -> ProductionBounds {
    ProductionBounds {
        max_connections: NonZeroUsize::new(max_connections).unwrap(),
        header_read_timeout: Some(header_read_timeout),
        drain_timeout: None,
    }
}

#[test]
fn production_header_timeout_closes_a_stalled_request() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server = smol::spawn(async move {
            serve(
                listener,
                service_fn(|_| async {
                    Ok::<_, Infallible>(Response::new(FullBody::from_static(b"ok")))
                }),
                BoxExecutor::new(SmolExecutor),
            )
            .with_bounds(bounds(Duration::from_millis(50), 8))
            .unwrap()
            .shutdown_on(shutdown_rx)
            .await
            .unwrap();
        });

        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n")
            .await
            .unwrap();
        client.flush().await.unwrap();

        let mut byte = [0_u8; 1];
        let result = future::select(
            Box::pin(client.read(&mut byte)),
            Box::pin(async_io::Timer::after(Duration::from_secs(2))),
        )
        .await;
        match result {
            Either::Left((Ok(0), _)) | Either::Left((Err(_), _)) => {}
            Either::Left((Ok(_), _)) => panic!("stalled request received a response"),
            Either::Right(_) => panic!("server did not enforce the header timeout"),
        }

        shutdown_tx.send(()).unwrap();
        server.await;
    });
}

#[test]
fn production_capacity_hook_reports_rejection() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let rejected = Arc::new(AtomicUsize::new(0));
        let hook = rejected.clone();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server = smol::spawn(async move {
            serve(
                listener,
                service_fn(|_| async {
                    Ok::<_, Infallible>(Response::new(FullBody::from_static(b"ok")))
                }),
                BoxExecutor::new(SmolExecutor),
            )
            .with_bounds(bounds(Duration::from_secs(5), 1))
            .unwrap()
            .on_connection_rejected(move || {
                hook.fetch_add(1, Ordering::SeqCst);
            })
            .shutdown_on(shutdown_rx)
            .await
            .unwrap();
        });

        let mut first = TcpStream::connect(address).await.unwrap();
        first
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        first.flush().await.unwrap();
        let mut response = [0_u8; 128];
        let read = first.read(&mut response).await.unwrap();
        assert!(response[..read].starts_with(b"HTTP/1.1 200"));

        let mut second = TcpStream::connect(address).await.unwrap();
        let mut byte = [0_u8; 1];
        let result = future::select(
            Box::pin(second.read(&mut byte)),
            Box::pin(async_io::Timer::after(Duration::from_secs(2))),
        )
        .await;
        match result {
            Either::Left((Ok(0), _)) | Either::Left((Err(_), _)) => {}
            Either::Left((Ok(_), _)) => panic!("over-capacity stream was served"),
            Either::Right(_) => panic!("over-capacity stream was not rejected"),
        }

        for _ in 0..100 {
            if rejected.load(Ordering::SeqCst) == 1 {
                break;
            }
            async_io::Timer::after(Duration::from_millis(1)).await;
        }
        assert_eq!(rejected.load(Ordering::SeqCst), 1);

        drop(first);
        drop(second);
        shutdown_tx.send(()).unwrap();
        server.await;
    });
}

#[test]
fn production_drain_timeout_aborts_a_stalled_handler() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (started_tx, started_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server = smol::spawn(async move {
            let started = Arc::new(std::sync::Mutex::new(Some(started_tx)));
            let service = service_fn(move |_| {
                let started = started.lock().unwrap().take();
                async move {
                    if let Some(started) = started {
                        let _ = started.send(());
                    }
                    std::future::pending::<Result<Response<FullBody>, Infallible>>().await
                }
            });
            serve(listener, service, BoxExecutor::new(SmolExecutor))
                .with_bounds(bounds(Duration::from_secs(5), 8))
                .unwrap()
                .drain_timeout(Some(Duration::from_millis(50)))
                .shutdown_on(shutdown_rx)
                .await
        });

        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        client.flush().await.unwrap();
        started_rx.await.unwrap();

        shutdown_tx.send(()).unwrap();
        let error = server
            .await
            .expect_err("a stalled handler must exceed the drain deadline");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);

        drop(client);
    });
}
