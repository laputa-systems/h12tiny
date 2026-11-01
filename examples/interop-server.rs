//! Small loopback server for the external interoperability and benchmark
//! harnesses.
//!
//! The endpoint body is selected only by the path:
//! `/0` is empty, `/1k` is 1 KiB, `/64k` is 64 KiB, and `/1m` is 1 MiB;
//! `/headers` returns a larger repeated header set.  The TLS listener
//! uses the committed localhost fixture and advertises both `h2` and
//! `http/1.1` through ALPN.  This is intentionally a low-level example, not
//! a general-purpose HTTP server or router.

use std::env;
use std::sync::Arc;

use async_net::TcpListener;
use futures_rustls::TlsAcceptor;
use h12tiny::io::FuturesIo;
use h12tiny::runtime::BoxExecutor;
use h12tiny::server::conn::auto;

mod support {
    pub mod benchmark;
}

use support::benchmark::{BenchmarkService, SmolExecutor};

fn tls_config() -> rustls::ServerConfig {
    use futures_rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let certificate =
        CertificateDer::from(include_bytes!("../tests/fixtures/tls/cert.der").to_vec());
    let key = PrivateKeyDer::try_from(include_bytes!("../tests/fixtures/tls/key.der").to_vec())
        .expect("the committed TLS fixture key must be PKCS#8 DER");
    let provider = Arc::new(rustls_graviola::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("Graviola supports Rustls' safe default protocol versions")
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .expect("the committed TLS fixture certificate and key must match");
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    config
}

async fn serve_plain(listener: TcpListener, executor: BoxExecutor) {
    loop {
        let (stream, peer) = listener.accept().await.expect("plain listener failed");
        let executor = executor.clone();
        smol::spawn(async move {
            let result = auto::Builder::new(executor)
                .serve_connection(FuturesIo::new(stream), BenchmarkService)
                .await;
            if let Err(error) = result {
                eprintln!("plain connection {peer} ended with error: {error}");
            }
        })
        .detach();
    }
}

async fn serve_tls(listener: TcpListener, executor: BoxExecutor, acceptor: TlsAcceptor) {
    loop {
        let (stream, peer) = listener.accept().await.expect("TLS listener failed");
        let executor = executor.clone();
        let acceptor = acceptor.clone();
        smol::spawn(async move {
            let tls = match acceptor.accept(stream).await {
                Ok(tls) => tls,
                Err(error) => {
                    eprintln!("TLS handshake from {peer} failed: {error}");
                    return;
                }
            };
            let connection =
                match auto::Builder::new(executor).serve_tls_connection(tls, BenchmarkService) {
                    Ok(connection) => connection,
                    Err(error) => {
                        eprintln!("TLS protocol selection for {peer} failed: {error}");
                        return;
                    }
                };
            if let Err(error) = connection.await {
                eprintln!("TLS connection {peer} ended with error: {error}");
            }
        })
        .detach();
    }
}

#[derive(Debug)]
struct Options {
    plain: Option<String>,
    tls: Option<String>,
}

fn usage() -> &'static str {
    "usage: cargo run --example interop-server -- [--plain ADDR] [--tls ADDR]\n\n\
defaults: --plain 127.0.0.1:3000 --tls 127.0.0.1:3443\n\
endpoints: /0, /1k, /64k, /1m, /headers"
}

fn options() -> Result<Options, String> {
    let mut plain = Some("127.0.0.1:3000".to_owned());
    let mut tls = Some("127.0.0.1:3443".to_owned());
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            "--plain" => plain = Some(args.next().ok_or("--plain needs ADDR".to_owned())?),
            "--tls" => tls = Some(args.next().ok_or("--tls needs ADDR".to_owned())?),
            "--no-plain" => plain = None,
            "--no-tls" => tls = None,
            other => return Err(format!("unknown option {other:?}\n\n{}", usage())),
        }
    }
    if plain.is_none() && tls.is_none() {
        return Err("at least one listener must be enabled".to_owned());
    }
    Ok(Options { plain, tls })
}

fn main() {
    let options = match options() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    };

    smol::block_on(async move {
        let executor = BoxExecutor::new(SmolExecutor);
        let mut listeners = Vec::new();
        if let Some(address) = options.plain {
            let listener = TcpListener::bind(&address).await?;
            eprintln!(
                "h12tiny plaintext server listening on http://{}",
                listener.local_addr()?
            );
            listeners.push(smol::spawn(serve_plain(listener, executor.clone())));
        }
        if let Some(address) = options.tls {
            let listener = TcpListener::bind(&address).await?;
            eprintln!(
                "h12tiny TLS server listening on https://{}",
                listener.local_addr()?
            );
            let acceptor = TlsAcceptor::from(Arc::new(tls_config()));
            listeners.push(smol::spawn(serve_tls(listener, executor, acceptor)));
        }
        for listener in listeners {
            listener.await;
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .unwrap_or_else(|error| {
        eprintln!("server failed: {error}");
        std::process::exit(1);
    });
}
