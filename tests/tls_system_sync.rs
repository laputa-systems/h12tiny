#![cfg(all(feature = "client-sync", feature = "tls"))]

#[path = "support/tls_system.rs"]
mod tls_system;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use h12tiny::client_sync::{Client, ClientTlsConfigBuilder, ErrorKind};
use http::Request;

fn get(port: u16) -> Result<http::StatusCode, h12tiny::client_sync::Error> {
    let request = Request::builder()
        .uri(format!("https://localhost:{port}/"))
        .body(Vec::new())
        .unwrap();
    let response = Client::new()
        .request_with_timeout(request, Some(Duration::from_secs(5)))?;
    Ok(response.status())
}

fn serve_chain(
    chain_der: Vec<Vec<u8>>,
    key_der: Vec<u8>,
    connections: usize,
) -> std::net::SocketAddr {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let chain: Vec<CertificateDer<'static>> = chain_der
        .into_iter()
        .map(CertificateDer::from)
        .collect();
    let key = PrivateKeyDer::try_from(key_der).expect("server key is valid");
    let provider = Arc::new(rustls_graviola::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("Graviola supports safe defaults")
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .expect("server chain and key match");
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let config = Arc::new(config);

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address");
    std::thread::spawn(move || {
        for _ in 0..connections {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let _ = handle_one(stream, &config);
        }
    });
    address
}

fn handle_one(
    stream: std::net::TcpStream,
    config: &Arc<rustls::ServerConfig>,
) -> std::io::Result<()> {
    let session = rustls::ServerConnection::new(Arc::clone(config))
        .map_err(std::io::Error::other)?;
    let mut tls = rustls::StreamOwned::new(session, stream);
    let mut request = Vec::new();
    loop {
        let mut input = [0_u8; 1024];
        let read = tls.read(&mut input)?;
        request.extend_from_slice(&input[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")?;
    tls.flush()
}

fn isolated(test_name: &str) -> bool {
    if tls_system::in_child() {
        return false;
    }
    tls_system::run_child(test_name);
    true
}

#[test]
fn trusted_system_root_accepted() {
    if isolated("trusted_system_root_accepted") {
        return;
    }
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
    );
    assert_eq!(get(address.port()).unwrap(), 200);
}

#[test]
fn untrusted_root_rejected() {
    if isolated("untrusted_root_rejected") {
        return;
    }
    tls_system::clear_bundle_env();
    let chain = tls_system::p384_chain();
    let dir = tls_system::TempDir::new();
    let bundle = tls_system::write_bundle(dir.path(), "unrelated.pem", &[&chain.root_pem]);
    tls_system::set_bundle_file(&bundle);

    // A different chain's server is not covered by this bundle.
    let other = tls_system::p384_chain();
    let address = serve_chain(
        vec![
            other.server_der.clone(),
            other.intermediate_der.clone(),
            other.issuing_der.clone(),
        ],
        other.server_key_der.clone(),
        1,
    );
    let error = get(address.port()).expect_err("untrusted root must fail");
    assert_eq!(error.kind(), ErrorKind::Tls);
}

#[test]
fn wrong_hostname_rejected() {
    let chain = tls_system::p384_chain();
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(chain.root_der.clone()))
        .unwrap();
    let address = serve_chain(
        vec![
            chain.server_der.clone(),
            chain.intermediate_der.clone(),
            chain.issuing_der.clone(),
        ],
        chain.server_key_der.clone(),
        1,
    );
    let config = ClientTlsConfigBuilder::new()
        .root_certificates(roots)
        .build()
        .unwrap();
    let request = Request::builder()
        .uri(format!("https://127.0.0.1:{}/", address.port()))
        .body(Vec::new())
        .unwrap();
    let error = Client::builder()
        .tls_config(config)
        .build()
        .request_with_timeout(request, Some(Duration::from_secs(5)))
        .expect_err("wrong hostname must fail");
    assert_eq!(error.kind(), ErrorKind::Tls);
}

#[test]
fn malformed_or_absent_bundle_fails_closed() {
    if isolated("malformed_or_absent_bundle_fails_closed") {
        return;
    }
    let chain = tls_system::p384_chain();
    let dir = tls_system::TempDir::new();
    let address = serve_chain(
        vec![
            chain.server_der.clone(),
            chain.intermediate_der.clone(),
            chain.issuing_der.clone(),
        ],
        chain.server_key_der.clone(),
        3,
    );

    let missing = dir.path().join("missing.pem");
    tls_system::set_bundle_file(&missing);
    let error = get(address.port()).expect_err("absent bundle must fail");
    assert_eq!(error.kind(), ErrorKind::Tls);

    let garbage = dir.path().join("garbage.pem");
    std::fs::write(&garbage, "not a certificate bundle\n").unwrap();
    tls_system::set_bundle_file(&garbage);
    let error = get(address.port()).expect_err("malformed bundle must fail");
    assert_eq!(error.kind(), ErrorKind::Tls);

    let empty = dir.path().join("empty.pem");
    std::fs::write(&empty, "").unwrap();
    tls_system::set_bundle_file(&empty);
    let error = get(address.port()).expect_err("empty bundle must fail");
    assert_eq!(error.kind(), ErrorKind::Tls);

    tls_system::set_bundle_file(&missing);
    let build = ClientTlsConfigBuilder::new().build();
    assert!(build.is_err(), "builder must fail without trust");
}

#[test]
fn explicit_admin_root_honored() {
    let chain = tls_system::p384_chain();
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(chain.root_der.clone()))
        .unwrap();
    let config = ClientTlsConfigBuilder::new()
        .root_certificates(roots)
        .build()
        .unwrap();
    let address = serve_chain(
        vec![
            chain.server_der.clone(),
            chain.intermediate_der.clone(),
            chain.issuing_der.clone(),
        ],
        chain.server_key_der.clone(),
        1,
    );
    let request = Request::builder()
        .uri(format!("https://localhost:{}/", address.port()))
        .body(Vec::new())
        .unwrap();
    let response = Client::builder()
        .tls_config(config)
        .build()
        .request_with_timeout(request, Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[test]
fn explicit_replacement_preserves_semantics() {
    if isolated("explicit_replacement_preserves_semantics") {
        return;
    }
    let system_chain = tls_system::p384_chain();
    let explicit_chain = tls_system::p384_chain();
    let dir = tls_system::TempDir::new();
    let bundle = tls_system::write_bundle(dir.path(), "bundle.pem", &[&system_chain.root_pem]);
    tls_system::set_bundle_file(&bundle);

    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(
            explicit_chain.root_der.clone(),
        ))
        .unwrap();
    let config = ClientTlsConfigBuilder::new()
        .root_certificates(roots)
        .build()
        .unwrap();
    let client = Client::builder().tls_config(config).build();

    // The system bundle trusts the system chain, but the explicit store must
    // not merge with it.
    let address = serve_chain(
        vec![
            system_chain.server_der.clone(),
            system_chain.intermediate_der.clone(),
            system_chain.issuing_der.clone(),
        ],
        system_chain.server_key_der.clone(),
        1,
    );
    let request = Request::builder()
        .uri(format!("https://localhost:{}/", address.port()))
        .body(Vec::new())
        .unwrap();
    let error = client
        .request_with_timeout(request, Some(Duration::from_secs(5)))
        .expect_err("explicit store must not include system roots");
    assert_eq!(error.kind(), ErrorKind::Tls);

    let address = serve_chain(
        vec![
            explicit_chain.server_der.clone(),
            explicit_chain.intermediate_der.clone(),
            explicit_chain.issuing_der.clone(),
        ],
        explicit_chain.server_key_der.clone(),
        1,
    );
    let request = Request::builder()
        .uri(format!("https://localhost:{}/", address.port()))
        .body(Vec::new())
        .unwrap();
    let response = client
        .request_with_timeout(request, Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[test]
fn removed_system_root_rejects_later_handshakes() {
    if isolated("removed_system_root_rejects_later_handshakes") {
        return;
    }
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
        2,
    );
    assert_eq!(get(address.port()).unwrap(), 200);
    std::fs::remove_file(&bundle).unwrap();
    let error = get(address.port()).expect_err("removed root must fail");
    assert_eq!(error.kind(), ErrorKind::Tls);
}

#[test]
fn added_system_root_observed() {
    if isolated("added_system_root_observed") {
        return;
    }
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
    );
    let second_address = serve_chain(
        vec![
            second.server_der.clone(),
            second.intermediate_der.clone(),
            second.issuing_der.clone(),
        ],
        second.server_key_der.clone(),
        2,
    );
    assert_eq!(get(first_address.port()).unwrap(), 200);
    let error = get(second_address.port()).expect_err("unknown root must fail");
    assert_eq!(error.kind(), ErrorKind::Tls);

    tls_system::write_bundle(
        dir.path(),
        "bundle.pem",
        &[&first.root_pem, &second.root_pem],
    );
    assert_eq!(get(second_address.port()).unwrap(), 200);
}

#[test]
fn malformed_replacement_does_not_reuse_stale_trust() {
    if isolated("malformed_replacement_does_not_reuse_stale_trust") {
        return;
    }
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
        3,
    );
    assert_eq!(get(address.port()).unwrap(), 200);
    std::fs::write(&bundle, "definitely not PEM\n").unwrap();
    let error = get(address.port()).expect_err("malformed store must fail");
    assert_eq!(error.kind(), ErrorKind::Tls);
    tls_system::write_bundle(dir.path(), "bundle.pem", &[&chain.root_pem]);
    assert_eq!(get(address.port()).unwrap(), 200);
}

#[test]
fn custom_tls_and_provider_still_work() {
    let chain = tls_system::p384_chain();
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(chain.root_der.clone()))
        .unwrap();

    let provider = Arc::new(rustls_graviola::default_provider());
    let config = ClientTlsConfigBuilder::with_provider(provider)
        .root_certificates(roots)
        .build()
        .unwrap();
    let address = serve_chain(
        vec![
            chain.server_der.clone(),
            chain.intermediate_der.clone(),
            chain.issuing_der.clone(),
        ],
        chain.server_key_der.clone(),
        2,
    );
    let request = Request::builder()
        .uri(format!("https://localhost:{}/", address.port()))
        .body(Vec::new())
        .unwrap();
    let response = Client::builder()
        .tls_config(config)
        .build()
        .request_with_timeout(request, Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(response.status(), 200);

    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(chain.root_der.clone()))
        .unwrap();
    let provider = Arc::new(rustls_graviola::default_provider());
    let explicit = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let request = Request::builder()
        .uri(format!("https://localhost:{}/", address.port()))
        .body(Vec::new())
        .unwrap();
    let response = Client::builder()
        .tls_config(explicit)
        .build()
        .request_with_timeout(request, Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[test]
fn cross_signed_distrusted_spki_rejected() {
    tls_system::check_distrusted_fixtures();
    let chain = tls_system::p384_chain();
    let distrusted = tls_system::distrusted_spki("EMDVTLSCAG3B");

    let patched_intermediate = tls_system::patch_spki_contents(
        &chain.intermediate_der,
        &chain.intermediate_key_raw,
        &distrusted,
    );
    let patched_cross_signed = tls_system::patch_spki_contents(
        &chain.cross_signed_der,
        &chain.intermediate_key_raw,
        &distrusted,
    );

    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(chain.root_der.clone()))
        .unwrap();
    let provider = Arc::new(rustls_graviola::default_provider());
    let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        provider,
    )
    .build()
    .unwrap();
    let server_name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let now = rustls::pki_types::UnixTime::since_unix_epoch(std::time::Duration::from_secs(
        0x1fed_f00d,
    ));
    use rustls::client::danger::ServerCertVerifier;
    let error = verifier
        .verify_server_cert(
            &rustls::pki_types::CertificateDer::from(chain.server_der.clone()),
            &[
                rustls::pki_types::CertificateDer::from(patched_intermediate.clone()),
                rustls::pki_types::CertificateDer::from(patched_cross_signed.clone()),
            ],
            &server_name,
            &[],
            now,
        )
        .expect_err("distrusted cross-signed SPKI must fail");
    let debug = format!("{error:?}");
    assert!(
        debug.contains("CertDistrusted"),
        "expected CertDistrusted, got {debug}"
    );

    let address = serve_chain(
        vec![
            chain.server_der.clone(),
            patched_intermediate,
            patched_cross_signed,
        ],
        chain.server_key_der.clone(),
        1,
    );
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(chain.root_der.clone()))
        .unwrap();
    let config = ClientTlsConfigBuilder::new()
        .root_certificates(roots)
        .build()
        .unwrap();
    let request = Request::builder()
        .uri(format!("https://localhost:{}/", address.port()))
        .body(Vec::new())
        .unwrap();
    let error = Client::builder()
        .tls_config(config)
        .build()
        .request_with_timeout(request, Some(Duration::from_secs(5)))
        .expect_err("cross-signed distrust must fail the handshake");
    assert_eq!(error.kind(), ErrorKind::Tls);
    let chain_debug = format!("{error:?}");
    assert!(
        chain_debug.contains("CertDistrusted"),
        "expected CertDistrusted in {chain_debug}"
    );
}

#[test]
fn valid_unrelated_chains_work() {
    let chain = tls_system::p384_chain();
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(chain.root_der.clone()))
        .unwrap();
    let config = ClientTlsConfigBuilder::new()
        .root_certificates(roots)
        .build()
        .unwrap();
    let address = serve_chain(
        vec![
            chain.server_der.clone(),
            chain.intermediate_der.clone(),
            chain.issuing_der.clone(),
        ],
        chain.server_key_der.clone(),
        1,
    );
    let request = Request::builder()
        .uri(format!("https://localhost:{}/", address.port()))
        .body(Vec::new())
        .unwrap();
    let response = Client::builder()
        .tls_config(config)
        .build()
        .request_with_timeout(request, Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(response.status(), 200);
}

fn pad_pem(pem: &str, total_len: usize) -> String {
    let mut padded = pem.to_string();
    while padded.len() < total_len {
        padded.push('\n');
    }
    padded
}

fn file_mtime(path: &std::path::Path) -> std::time::SystemTime {
    std::fs::metadata(path)
        .expect("bundle metadata")
        .modified()
        .expect("bundle mtime")
}

fn restore_mtime(path: &std::path::Path, mtime: std::time::SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .expect("bundle file")
        .set_modified(mtime)
        .expect("restore mtime");
}

#[test]
fn same_size_forged_mtime_replacement_refreshes_trust() {
    if isolated("same_size_forged_mtime_replacement_refreshes_trust") {
        return;
    }
    let first = tls_system::p384_chain();
    let second = tls_system::p384_chain();
    let dir = tls_system::TempDir::new();
    let bundle = tls_system::write_bundle(dir.path(), "bundle.pem", &[&first.root_pem]);
    tls_system::set_bundle_file(&bundle);
    let stamped = file_mtime(&bundle);

    let first_address = serve_chain(
        vec![
            first.server_der.clone(),
            first.intermediate_der.clone(),
            first.issuing_der.clone(),
        ],
        first.server_key_der.clone(),
        3,
    );
    let second_address = serve_chain(
        vec![
            second.server_der.clone(),
            second.intermediate_der.clone(),
            second.issuing_der.clone(),
        ],
        second.server_key_der.clone(),
        3,
    );
    // One long-lived client: the fingerprint must notice the replacement.
    // Fresh clients would reload unconditionally and prove nothing here.
    let client = Client::new();
    let first_request = || {
        Request::builder()
            .uri(format!("https://localhost:{}/", first_address.port()))
            .body(Vec::new())
            .unwrap()
    };
    let second_request = || {
        Request::builder()
            .uri(format!("https://localhost:{}/", second_address.port()))
            .body(Vec::new())
            .unwrap()
    };
    assert_eq!(
        client
            .request_with_timeout(first_request(), Some(Duration::from_secs(5)))
            .unwrap()
            .status(),
        200
    );
    let error = client
        .request_with_timeout(second_request(), Some(Duration::from_secs(5)))
        .expect_err("unknown root must fail");
    assert_eq!(error.kind(), ErrorKind::Tls);

    let target_len = first.root_pem.len().max(second.root_pem.len());
    std::fs::write(&bundle, pad_pem(&first.root_pem, target_len)).unwrap();
    restore_mtime(&bundle, stamped);
    std::fs::write(&bundle, pad_pem(&second.root_pem, target_len)).unwrap();
    restore_mtime(&bundle, stamped);

    assert_eq!(
        client
            .request_with_timeout(second_request(), Some(Duration::from_secs(5)))
            .unwrap()
            .status(),
        200
    );
    let error = client
        .request_with_timeout(first_request(), Some(Duration::from_secs(5)))
        .expect_err("replaced root must fail");
    assert_eq!(error.kind(), ErrorKind::Tls);
}

#[test]
fn symlink_retarget_with_preserved_size_and_mtime_refreshes_trust() {
    if isolated("symlink_retarget_with_preserved_size_and_mtime_refreshes_trust") {
        return;
    }
    let first = tls_system::p384_chain();
    let second = tls_system::p384_chain();
    let dir = tls_system::TempDir::new();
    let target_len = first.root_pem.len().max(second.root_pem.len());
    let first_path = dir.path().join("first.pem");
    let second_path = dir.path().join("second.pem");
    std::fs::write(&first_path, pad_pem(&first.root_pem, target_len)).unwrap();
    std::fs::write(&second_path, pad_pem(&second.root_pem, target_len)).unwrap();
    let stamped = file_mtime(&first_path);
    restore_mtime(&second_path, stamped);

    let link = dir.path().join("bundle.pem");
    std::os::unix::fs::symlink(&first_path, &link).unwrap();
    tls_system::set_bundle_file(&link);

    let first_address = serve_chain(
        vec![
            first.server_der.clone(),
            first.intermediate_der.clone(),
            first.issuing_der.clone(),
        ],
        first.server_key_der.clone(),
        2,
    );
    let second_address = serve_chain(
        vec![
            second.server_der.clone(),
            second.intermediate_der.clone(),
            second.issuing_der.clone(),
        ],
        second.server_key_der.clone(),
        2,
    );
    let client = Client::new();
    let first_request = || {
        Request::builder()
            .uri(format!("https://localhost:{}/", first_address.port()))
            .body(Vec::new())
            .unwrap()
    };
    let second_request = || {
        Request::builder()
            .uri(format!("https://localhost:{}/", second_address.port()))
            .body(Vec::new())
            .unwrap()
    };
    assert_eq!(
        client
            .request_with_timeout(first_request(), Some(Duration::from_secs(5)))
            .unwrap()
            .status(),
        200
    );

    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&second_path, &link).unwrap();

    assert_eq!(
        client
            .request_with_timeout(second_request(), Some(Duration::from_secs(5)))
            .unwrap()
            .status(),
        200
    );
    let error = client
        .request_with_timeout(first_request(), Some(Duration::from_secs(5)))
        .expect_err("retargeted root must fail");
    assert_eq!(error.kind(), ErrorKind::Tls);
}

#[test]
fn nested_symlink_retarget_through_active_refreshes_trust() {
    if isolated("nested_symlink_retarget_through_active_refreshes_trust") {
        return;
    }
    let first = tls_system::p384_chain();
    let second = tls_system::p384_chain();
    let dir = tls_system::TempDir::new();
    // Laputa-style layout: the tracked bundle path is a symlink whose target
    // passes through the `active` symlink, which selects the generation.
    // Retargeting `active` replaces neither the tracked link nor the final
    // bundle's size or mtime, so only the resolved target identity changes.
    let gen3 = dir.path().join("generations").join("3");
    let gen4 = dir.path().join("generations").join("4");
    std::fs::create_dir_all(&gen3).unwrap();
    std::fs::create_dir_all(&gen4).unwrap();
    let target_len = first.root_pem.len().max(second.root_pem.len());
    std::fs::write(gen3.join("bundle.pem"), pad_pem(&first.root_pem, target_len)).unwrap();
    std::fs::write(gen4.join("bundle.pem"), pad_pem(&second.root_pem, target_len)).unwrap();
    let stamped = file_mtime(&gen3.join("bundle.pem"));
    restore_mtime(&gen4.join("bundle.pem"), stamped);

    let active = dir.path().join("active");
    std::os::unix::fs::symlink("generations/3", &active).unwrap();
    let link = dir.path().join("bundle.pem");
    std::os::unix::fs::symlink("active/bundle.pem", &link).unwrap();
    tls_system::set_bundle_file(&link);

    let first_address = serve_chain(
        vec![
            first.server_der.clone(),
            first.intermediate_der.clone(),
            first.issuing_der.clone(),
        ],
        first.server_key_der.clone(),
        2,
    );
    let second_address = serve_chain(
        vec![
            second.server_der.clone(),
            second.intermediate_der.clone(),
            second.issuing_der.clone(),
        ],
        second.server_key_der.clone(),
        2,
    );
    let client = Client::new();
    let first_request = || {
        Request::builder()
            .uri(format!("https://localhost:{}/", first_address.port()))
            .body(Vec::new())
            .unwrap()
    };
    let second_request = || {
        Request::builder()
            .uri(format!("https://localhost:{}/", second_address.port()))
            .body(Vec::new())
            .unwrap()
    };
    assert_eq!(
        client
            .request_with_timeout(first_request(), Some(Duration::from_secs(5)))
            .unwrap()
            .status(),
        200
    );

    std::fs::remove_file(&active).unwrap();
    std::os::unix::fs::symlink("generations/4", &active).unwrap();

    assert_eq!(
        client
            .request_with_timeout(second_request(), Some(Duration::from_secs(5)))
            .unwrap()
            .status(),
        200
    );
    let error = client
        .request_with_timeout(first_request(), Some(Duration::from_secs(5)))
        .expect_err("retargeted root must fail");
    assert_eq!(error.kind(), ErrorKind::Tls);
}

#[test]
fn designated_bundle_with_unparsable_block_fails_closed() {
    if isolated("designated_bundle_with_unparsable_block_fails_closed") {
        return;
    }
    let chain = tls_system::p384_chain();
    let dir = tls_system::TempDir::new();
    let bundle = tls_system::write_bundle(dir.path(), "bundle.pem", &[&chain.root_pem]);
    std::fs::write(
        &bundle,
        format!("{chain}\n-----BEGIN CERTIFICATE-----\n!!!not-base64!!!\n-----END CERTIFICATE-----\n", chain = chain.root_pem),
    )
    .unwrap();
    tls_system::set_bundle_file(&bundle);

    let error = ClientTlsConfigBuilder::new()
        .build()
        .expect_err("designated bundle with an unparsable block must fail");
    let message = format!("{error:?}");
    assert!(
        message.contains("undecodable") || message.contains("unusable"),
        "unexpected error: {message}"
    );
}

#[test]
fn supplementary_dir_errors_do_not_break_valid_bundle() {
    if isolated("supplementary_dir_errors_do_not_break_valid_bundle") {
        return;
    }
    let chain = tls_system::p384_chain();
    let dir = tls_system::TempDir::new();
    let bundle = tls_system::write_bundle(dir.path(), "bundle.pem", &[&chain.root_pem]);
    tls_system::set_bundle_file(&bundle);
    let certs_dir = dir.path().join("certs.d");
    std::fs::create_dir_all(&certs_dir).unwrap();
    std::fs::write(certs_dir.join("garbage.pem"), "definitely not PEM\n").unwrap();
    tls_system::set_bundle_dir(&certs_dir);

    let address = serve_chain(
        vec![
            chain.server_der.clone(),
            chain.intermediate_der.clone(),
            chain.issuing_der.clone(),
        ],
        chain.server_key_der.clone(),
        1,
    );
    assert_eq!(get(address.port()).unwrap(), 200);
}
