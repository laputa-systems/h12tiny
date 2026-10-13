#![cfg(all(feature = "client-sync", feature = "tls"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use h12tiny::client_sync::Client;
use http::Request;

fn fixture_server_config() -> rustls::ServerConfig {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let certificate = CertificateDer::from(include_bytes!("fixtures/tls/cert.der").to_vec());
    let key = PrivateKeyDer::try_from(include_bytes!("fixtures/tls/key.der").to_vec())
        .expect("fixture key is valid PKCS#8 DER");
    let provider = Arc::new(rustls_graviola::default_provider());
    rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("Graviola supports Rustls safe protocol versions")
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .expect("fixture certificate and key match")
}

fn fixture_client_config() -> rustls::ClientConfig {
    use rustls::pki_types::CertificateDer;

    let certificate = CertificateDer::from(include_bytes!("fixtures/tls/cert.der").to_vec());
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(certificate)
        .expect("fixture certificate is a valid root");
    let provider = Arc::new(rustls_graviola::default_provider());
    rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("Graviola supports Rustls safe protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

#[test]
fn sync_client_uses_http11_alpn_and_reads_a_tls_response() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("TLS fixture should bind");
    let address = listener
        .local_addr()
        .expect("TLS fixture should expose its address");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("sync client should connect");
        let mut config = fixture_server_config();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let session = rustls::ServerConnection::new(Arc::new(config))
            .expect("fixture server configuration should be valid");
        let mut tls = rustls::StreamOwned::new(session, stream);
        let mut request = Vec::new();
        loop {
            let mut input = [0_u8; 1024];
            let read = tls.read(&mut input).expect("TLS request should read");
            request.extend_from_slice(&input[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        assert!(request.starts_with(b"GET /secure HTTP/1.1\r\n"));
        assert_eq!(tls.conn.alpn_protocol(), Some(&b"http/1.1"[..]));
        tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsecure")
            .expect("TLS response should write");
        tls.flush().expect("TLS response should flush");
    });

    let mut config = fixture_client_config();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let request = Request::builder()
        .uri(format!("https://localhost:{}/secure", address.port()))
        .body(Vec::new())
        .expect("request should be valid");
    let mut response = Client::builder()
        .connect_timeout(Duration::from_secs(1))
        .tls_config(config)
        .build()
        .request_with_timeout(request, Some(Duration::from_secs(1)))
        .expect("sync client should complete the TLS request");
    let mut body = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut body)
        .expect("sync TLS response body should read");
    assert_eq!(response.status(), 200);
    assert_eq!(body, b"secure");
    server.join().expect("TLS fixture should finish");
}

#[test]
fn sync_client_timeout_covers_slow_fragments_inside_one_tls_record() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("TLS fixture should bind");
    let address = listener
        .local_addr()
        .expect("TLS fixture should expose its address");
    let (stop, wait_for_stop) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("sync client should connect");
        let mut config = fixture_server_config();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let session = rustls::ServerConnection::new(Arc::new(config))
            .expect("fixture server configuration should be valid");
        let mut tls = rustls::StreamOwned::new(session, stream);
        let mut request = Vec::new();
        loop {
            let mut input = [0_u8; 1024];
            let read = tls.read(&mut input).expect("TLS request should read");
            request.extend_from_slice(&input[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let (mut session, mut stream) = tls.into_parts();
        session
            .writer()
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .expect("TLS response should queue");
        let mut encrypted = Vec::new();
        while session.wants_write() {
            session
                .write_tls(&mut encrypted)
                .expect("TLS response should encrypt");
        }
        stream
            .set_nodelay(true)
            .expect("TLS fixture should disable Nagle buffering");
        let chunk_size = encrypted.len().div_ceil(4);
        for chunk in encrypted.chunks(chunk_size) {
            match wait_for_stop.recv_timeout(Duration::from_millis(90)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if stream.write_all(chunk).is_err() {
                return;
            }
        }
    });

    let mut config = fixture_client_config();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let request = Request::builder()
        .uri(format!("https://localhost:{}/slow-head", address.port()))
        .body(Vec::new())
        .expect("request should be valid");
    let result = Client::builder()
        .connect_timeout(Duration::from_secs(1))
        .tls_config(config)
        .build()
        .request_with_timeout(request, Some(Duration::from_millis(250)));
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("TLS fragments in one record must share the dispatch deadline"),
    };
    let _ = stop.send(());
    server.join().expect("TLS fixture should finish");
    assert!(error.is_timeout());
}
