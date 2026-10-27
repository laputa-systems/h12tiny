#![cfg(all(feature = "client", feature = "http1", feature = "tls"))]

mod support;

use std::sync::Arc;

use h12tiny::client::{Client, ClientTlsConfigBuilder, Connector};
use support::tls_system;
use support::{FullBody, SmolExecutor, collect};

fn isolated(test_name: &str) -> bool {
    if tls_system::in_child() {
        return false;
    }
    tls_system::run_child(test_name);
    true
}

async fn serve_chain(
    chain_der: Vec<Vec<u8>>,
    key_der: Vec<u8>,
    connections: usize,
) -> std::net::SocketAddr {
    let chain: Vec<rustls::pki_types::CertificateDer<'static>> = chain_der
        .into_iter()
        .map(rustls::pki_types::CertificateDer::from)
        .collect();
    let key = rustls::pki_types::PrivateKeyDer::try_from(key_der).expect("server key");
    let provider = Arc::new(rustls_graviola::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("Graviola supports safe defaults")
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .expect("server chain and key match");
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = futures_rustls::TlsAcceptor::from(Arc::new(config));

    let listener = async_net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    smol::spawn(async move {
        for _ in 0..connections {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            smol::spawn(async move {
                let _ = handle_one(stream, acceptor).await;
            })
            .detach();
        }
    })
    .detach();
    address
}

async fn handle_one(
    stream: async_net::TcpStream,
    acceptor: futures_rustls::TlsAcceptor,
) -> std::io::Result<()> {
    use futures_lite::io::{AsyncReadExt, AsyncWriteExt};

    let mut tls = acceptor
        .accept(stream)
        .await
        .map_err(std::io::Error::other)?;
    let mut request = Vec::new();
    loop {
        let mut input = [0_u8; 1024];
        let read = tls.read(&mut input).await?;
        request.extend_from_slice(&input[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
        .await?;
    tls.flush().await
}

async fn get_default(port: u16) -> Result<Vec<u8>, h12tiny::client::Error> {
    let client = Client::builder(SmolExecutor).build::<FullBody>();
    let response = client
        .request(
            http::Request::get(format!("https://localhost:{port}/"))
                .body(FullBody::empty())
                .unwrap(),
        )
        .await?;
    Ok(collect(response.into_body()).await)
}

#[test]
fn async_trusted_system_root_accepted() {
    if isolated("async_trusted_system_root_accepted") {
        return;
    }
    smol::block_on(async {
        let chain = tls_system::p384_chain();
        let dir = tls_system::TempDir::new();
        let bundle = tls_system::write_bundle(dir.path(), "bundle.pem", &[&chain.root_pem]);
        tls_system::set_bundle_file(&bundle);

        let address = serve_chain(
            vec![
                chain.server_der.clone(),
                chain.intermediate_der.clone(),
                chain.issuing_der.clone(),
            ],
            chain.server_key_der.clone(),
            1,
        )
        .await;
        assert_eq!(
            get_default(address.port()).await.unwrap(),
            b"ok",
            "system root must validate"
        );
    });
}

#[test]
fn async_untrusted_root_rejected() {
    if isolated("async_untrusted_root_rejected") {
        return;
    }
    smol::block_on(async {
        tls_system::clear_bundle_env();
        let chain = tls_system::p384_chain();
        let other = tls_system::p384_chain();
        let dir = tls_system::TempDir::new();
        let bundle = tls_system::write_bundle(dir.path(), "bundle.pem", &[&chain.root_pem]);
        tls_system::set_bundle_file(&bundle);

        let address = serve_chain(
            vec![
                other.server_der.clone(),
                other.intermediate_der.clone(),
                other.issuing_der.clone(),
            ],
            other.server_key_der.clone(),
            1,
        )
        .await;
        let error = get_default(address.port())
            .await
            .expect_err("untrusted root must fail");
        assert_eq!(error.kind(), h12tiny::client::ErrorKind::Tls);
    });
}

#[test]
fn async_explicit_roots_honored() {
    smol::block_on(async {
        let chain = tls_system::p384_chain();
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(
                chain.root_der.clone(),
            ))
            .unwrap();
        let tls_config = ClientTlsConfigBuilder::new()
            .root_certificates(roots)
            .build()
            .unwrap();
        let connector = Connector::builder().tls_config(tls_config).build();
        let mut builder = Client::builder(SmolExecutor);
        builder.connector(connector);
        let client = builder.build::<FullBody>();

        let address = serve_chain(
            vec![
                chain.server_der.clone(),
                chain.intermediate_der.clone(),
                chain.issuing_der.clone(),
            ],
            chain.server_key_der.clone(),
            1,
        )
        .await;
        let response = client
            .request(
                http::Request::get(format!("https://localhost:{}/", address.port()))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);
    });
}

#[test]
fn async_replacement_preserves_semantics() {
    if isolated("async_replacement_preserves_semantics") {
        return;
    }
    smol::block_on(async {
        let system_chain = tls_system::p384_chain();
        let explicit_chain = tls_system::p384_chain();
        let dir = tls_system::TempDir::new();
        let bundle =
            tls_system::write_bundle(dir.path(), "bundle.pem", &[&system_chain.root_pem]);
        tls_system::set_bundle_file(&bundle);

        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(
                explicit_chain.root_der.clone(),
            ))
            .unwrap();
        let tls_config = ClientTlsConfigBuilder::new()
            .root_certificates(roots)
            .build()
            .unwrap();
        let connector = Connector::builder().tls_config(tls_config).build();
        let mut builder = Client::builder(SmolExecutor);
        builder.connector(connector);
        let client = builder.build::<FullBody>();

        let address = serve_chain(
            vec![
                system_chain.server_der.clone(),
                system_chain.intermediate_der.clone(),
                system_chain.issuing_der.clone(),
            ],
            system_chain.server_key_der.clone(),
            1,
        )
        .await;
        let error = client
            .request(
                http::Request::get(format!("https://localhost:{}/", address.port()))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await
            .expect_err("explicit store must not include system roots");
        assert_eq!(error.kind(), h12tiny::client::ErrorKind::Tls);
    });
}

#[test]
fn async_system_refresh_observed() {
    if isolated("async_system_refresh_observed") {
        return;
    }
    smol::block_on(async {
        let first = tls_system::p384_chain();
        let second = tls_system::p384_chain();
        let dir = tls_system::TempDir::new();
        let bundle = tls_system::write_bundle(dir.path(), "bundle.pem", &[&first.root_pem]);
        tls_system::set_bundle_file(&bundle);

        let first_address = serve_chain(
            vec![
                first.server_der.clone(),
                first.intermediate_der.clone(),
                first.issuing_der.clone(),
            ],
            first.server_key_der.clone(),
            1,
        )
        .await;
        let second_address = serve_chain(
            vec![
                second.server_der.clone(),
                second.intermediate_der.clone(),
                second.issuing_der.clone(),
            ],
            second.server_key_der.clone(),
            2,
        )
        .await;
        assert_eq!(get_default(first_address.port()).await.unwrap(), b"ok");
        let error = get_default(second_address.port())
            .await
            .expect_err("unknown root must fail");
        assert_eq!(error.kind(), h12tiny::client::ErrorKind::Tls);

        tls_system::write_bundle(
            dir.path(),
            "bundle.pem",
            &[&first.root_pem, &second.root_pem],
        );
        assert_eq!(get_default(second_address.port()).await.unwrap(), b"ok");
    });
}

#[test]
fn async_custom_tls_and_provider_still_work() {
    smol::block_on(async {
        let chain = tls_system::p384_chain();
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(
                chain.root_der.clone(),
            ))
            .unwrap();
        let provider = Arc::new(rustls_graviola::default_provider());
        let tls_config = ClientTlsConfigBuilder::with_provider(provider)
            .root_certificates(roots)
            .build()
            .unwrap();
        let connector = Connector::builder().tls_config(tls_config).build();
        let mut builder = Client::builder(SmolExecutor);
        builder.connector(connector);
        let client = builder.build::<FullBody>();

        let address = serve_chain(
            vec![
                chain.server_der.clone(),
                chain.intermediate_der.clone(),
                chain.issuing_der.clone(),
            ],
            chain.server_key_der.clone(),
            1,
        )
        .await;
        let response = client
            .request(
                http::Request::get(format!("https://localhost:{}/", address.port()))
                    .body(FullBody::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);
    });
}
