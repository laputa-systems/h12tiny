//! Blocking direct-origin HTTP/1.1 client.
//!
//! `h12tiny-client-sync` is deliberately independent of the futures-I/O
//! client. It uses only `std::net` and, with the optional `tls` feature,
//! synchronous Rustls. There is no HTTP/2 negotiation, executor, connection
//! pool, proxy policy, redirect policy, or background driver.
//!
//! Requests use absolute `http` or `https` URIs to select their peer. Before
//! bytes are written, the shared direct-origin normalization boundary derives
//! the authority, synthesizes `Host` when requested, and writes an HTTP/1.1
//! origin-form target. Responses own their blocking body reader; dropping a
//! body closes its connection, so callers must not infer connection reuse from
//! body completion.

use std::error::Error as StdError;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use h12tiny_client_normalize::{extract_origin, normalize_http1_request, Origin};
use http::header::{CONNECTION, CONTENT_LENGTH, TRANSFER_ENCODING};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Version};

/// The `http` crate whose `Request`/`Response` types form this client's API,
/// reexported so callers build requests with the exact version in use here.
pub use http;

const MAX_RESPONSE_HEAD_BYTES: usize = 64 * 1024;
const MAX_CHUNK_LINE_BYTES: usize = 8 * 1024;
const MAX_TRAILER_BYTES: usize = 64 * 1024;

/// A direct-origin blocking HTTP/1.1 client.
#[derive(Clone)]
pub struct Client {
    connect_timeout: Option<Duration>,
    set_host: bool,
    #[cfg(feature = "tls")]
    tls: std::sync::Arc<rustls::ClientConfig>,
}

impl Client {
    /// Starts a builder for a small blocking client.
    pub fn builder() -> Builder {
        Builder::new()
    }

    /// Builds the default client configuration.
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Sends one request without a response-header timeout.
    ///
    /// The returned [`ResponseBody`] owns the connection. Configure a body
    /// idle timeout with [`ResponseBody::set_read_timeout`] before each read
    /// when a streaming caller needs one.
    pub fn request(&self, request: Request<Vec<u8>>) -> Result<Response<ResponseBody>, Error> {
        self.request_with_timeout(request, None)
    }

    /// Sends one request and bounds the request write plus response-header
    /// read with `timeout`.
    ///
    /// This is intentionally not a whole-response deadline. A response body
    /// can stream indefinitely when callers keep making progress; use
    /// [`ResponseBody::set_read_timeout`] to bound individual reads.
    pub fn request_with_timeout(
        &self,
        request: Request<Vec<u8>>,
        timeout: Option<Duration>,
    ) -> Result<Response<ResponseBody>, Error> {
        let (parts, body) = request.into_parts();
        let content_length = body.len() as u64;
        self.send(parts, content_length, body.as_slice(), timeout)
    }

    /// Sends one request whose body is read from `body` while it is written,
    /// so large uploads are never held in memory.
    ///
    /// Exactly `content_length` bytes are sent as the body, and a `Content-Length`
    /// header is synthesized from it unless the request already carries
    /// `Content-Length` or `Transfer-Encoding`. A caller-supplied framing
    /// header must agree with `content_length`; the client does not check it.
    /// A reader that ends early fails the request with
    /// [`ErrorKind::SendRequest`]. The body is consumed once, so the request is
    /// never replayed. `timeout` bounds the request write plus response-header
    /// read, as in [`Self::request_with_timeout`].
    pub fn request_streaming<R: Read>(
        &self,
        request: Request<R>,
        content_length: u64,
        timeout: Option<Duration>,
    ) -> Result<Response<ResponseBody>, Error> {
        let (parts, body) = request.into_parts();
        self.send(parts, content_length, body, timeout)
    }

    fn send(
        &self,
        parts: http::request::Parts,
        content_length: u64,
        body: impl Read,
        timeout: Option<Duration>,
    ) -> Result<Response<ResponseBody>, Error> {
        if matches!(timeout, Some(timeout) if timeout.is_zero()) {
            return Err(Error::new(ErrorKind::Timeout));
        }

        let mut request = Request::from_parts(parts, ());
        let is_connect = request.method() == Method::CONNECT;
        match request.version() {
            Version::HTTP_10 | Version::HTTP_11 => {}
            _ => return Err(Error::new(ErrorKind::UnsupportedVersion)),
        }
        let origin = extract_origin(request.uri_mut(), is_connect)
            .map_err(|error| Error::with_source(ErrorKind::AbsoluteUriRequired, error))?;
        normalize_http1_request(&mut request, self.set_host);

        let head_request = request.method() == Method::HEAD;
        let mut connection = self.connect(&origin)?;
        connection
            .set_timeouts(timeout, timeout)
            .map_err(|error| Error::from_io(ErrorKind::SendRequest, error))?;
        write_request(&mut connection, request.into_parts().0, content_length, body)?;
        read_response(connection, head_request)
    }

    fn connect(&self, origin: &Origin) -> Result<Connection, Error> {
        let uri = origin.uri();
        let host = uri
            .host()
            .ok_or_else(|| Error::new(ErrorKind::Connect))?
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or_else(|| uri.host().expect("host was checked above"))
            .to_owned();
        let scheme = uri.scheme_str().unwrap_or_default();
        match scheme {
            "http" => connect_tcp(&host, uri.port_u16().unwrap_or(80), self.connect_timeout)
                .map(Connection::Tcp)
                .map_err(|error| Error::from_io(ErrorKind::Connect, error)),
            "https" => self.connect_tls(host, uri.port_u16().unwrap_or(443)),
            other => Err(Error::with_source(
                ErrorKind::UnsupportedScheme,
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unsupported URI scheme {other:?}"),
                ),
            )),
        }
    }

    #[cfg(feature = "tls")]
    fn connect_tls(&self, host: String, port: u16) -> Result<Connection, Error> {
        let mut stream = connect_tcp(&host, port, self.connect_timeout)
            .map_err(|error| Error::from_io(ErrorKind::Connect, error))?;
        stream
            .set_read_timeout(self.connect_timeout)
            .and_then(|()| stream.set_write_timeout(self.connect_timeout))
            .map_err(|error| Error::from_io(ErrorKind::Tls, error))?;
        let server_name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| Error::with_source(ErrorKind::Tls, InvalidServerName(host.clone())))?;
        let mut session = rustls::ClientConnection::new(self.tls.clone(), server_name)
            .map_err(|error| Error::with_source(ErrorKind::Tls, error))?;
        session
            .complete_io(&mut stream)
            .map_err(|error| Error::from_io(ErrorKind::Tls, error))?;
        match session.alpn_protocol() {
            Some(b"http/1.1") | None => {}
            Some(protocol) => {
                return Err(Error::with_source(
                    ErrorKind::Alpn,
                    UnsupportedAlpn(protocol.to_vec()),
                ))
            }
        }
        stream
            .set_read_timeout(None)
            .and_then(|()| stream.set_write_timeout(None))
            .map_err(|error| Error::from_io(ErrorKind::Tls, error))?;
        Ok(Connection::Tls(rustls::StreamOwned::new(session, stream)))
    }

    #[cfg(not(feature = "tls"))]
    fn connect_tls(&self, _: String, _: u16) -> Result<Connection, Error> {
        Err(Error::new(ErrorKind::TlsUnavailable))
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for [`Client`] connection and request-normalization policy.
pub struct Builder {
    client: Client,
}

impl Builder {
    fn new() -> Self {
        Self {
            client: Client {
                connect_timeout: None,
                set_host: true,
                #[cfg(feature = "tls")]
                tls: std::sync::Arc::new(default_tls_config()),
            },
        }
    }

    /// Bounds each TCP candidate connection and the TLS handshake after DNS
    /// resolution. `ToSocketAddrs` is synchronous and cannot be preempted.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.client.connect_timeout = Some(timeout);
        self
    }

    /// Controls whether a missing `Host` header is synthesized from the
    /// absolute request URI. The default is `true`.
    pub fn set_host(mut self, enabled: bool) -> Self {
        self.client.set_host = enabled;
        self
    }

    /// Replaces the Rustls configuration used for HTTPS connections.
    #[cfg(feature = "tls")]
    pub fn tls_config(mut self, config: rustls::ClientConfig) -> Self {
        self.client.tls = std::sync::Arc::new(config);
        self
    }

    /// Finalizes the blocking client.
    pub fn build(self) -> Client {
        self.client
    }
}

/// Rustls configuration builder for the blocking HTTP/1.1 client.
///
/// It selects Graviola explicitly, validates against WebPKI roots, and offers
/// only `http/1.1` through ALPN. Use [`Builder::tls_config`] for advanced
/// Rustls configuration such as a private verifier.
#[cfg(feature = "tls")]
pub struct ClientTlsConfigBuilder {
    provider: std::sync::Arc<rustls::crypto::CryptoProvider>,
    roots: rustls::RootCertStore,
    protocol_versions: Option<Vec<&'static rustls::SupportedProtocolVersion>>,
    client_auth: ClientAuthentication,
}

#[cfg(feature = "tls")]
enum ClientAuthentication {
    None,
    Certificates {
        certificate_chain: Vec<rustls::pki_types::CertificateDer<'static>>,
        private_key: rustls::pki_types::PrivateKeyDer<'static>,
    },
}

#[cfg(feature = "tls")]
impl ClientTlsConfigBuilder {
    /// Starts with h12tiny's explicit Graviola provider and public roots.
    pub fn new() -> Self {
        Self::with_provider(std::sync::Arc::new(rustls_graviola::default_provider()))
    }

    /// Starts with an explicitly supplied Rustls provider.
    pub fn with_provider(provider: std::sync::Arc<rustls::crypto::CryptoProvider>) -> Self {
        Self {
            provider,
            roots: rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
            protocol_versions: None,
            client_auth: ClientAuthentication::None,
        }
    }

    /// Replaces the trust store used for server certificate validation.
    pub fn root_certificates(mut self, roots: rustls::RootCertStore) -> Self {
        self.roots = roots;
        self
    }

    /// Adds one trust anchor to the existing trust store.
    pub fn add_root_certificate(
        mut self,
        certificate: rustls::pki_types::CertificateDer<'static>,
    ) -> Result<Self, rustls::Error> {
        self.roots.add(certificate)?;
        Ok(self)
    }

    /// Restricts TLS negotiation to exactly these protocol versions.
    ///
    /// By default, Rustls' safe protocol versions supported by the selected
    /// provider are used. Supplying an empty list makes [`Self::build`] return
    /// a Rustls configuration error rather than silently broadening policy.
    pub fn protocol_versions(
        mut self,
        versions: impl IntoIterator<Item = &'static rustls::SupportedProtocolVersion>,
    ) -> Self {
        self.protocol_versions = Some(versions.into_iter().collect());
        self
    }

    /// Configures no TLS client authentication, which is the default.
    pub fn no_client_auth(mut self) -> Self {
        self.client_auth = ClientAuthentication::None;
        self
    }

    /// Configures a certificate chain and private key for mutual TLS.
    pub fn client_auth(
        mut self,
        certificate_chain: Vec<rustls::pki_types::CertificateDer<'static>>,
        private_key: rustls::pki_types::PrivateKeyDer<'static>,
    ) -> Self {
        self.client_auth = ClientAuthentication::Certificates {
            certificate_chain,
            private_key,
        };
        self
    }

    /// Finishes a configuration that can negotiate HTTP/1.1 only.
    pub fn build(self) -> Result<rustls::ClientConfig, rustls::Error> {
        let builder = rustls::ClientConfig::builder_with_provider(self.provider);
        let builder = match self.protocol_versions {
            Some(versions) => builder.with_protocol_versions(&versions)?,
            None => builder.with_safe_default_protocol_versions()?,
        };
        let mut config = match self.client_auth {
            ClientAuthentication::None => builder
                .with_root_certificates(self.roots)
                .with_no_client_auth(),
            ClientAuthentication::Certificates {
                certificate_chain,
                private_key,
            } => builder
                .with_root_certificates(self.roots)
                .with_client_auth_cert(certificate_chain, private_key)?,
        };
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(config)
    }
}

#[cfg(feature = "tls")]
impl Default for ClientTlsConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Error returned while establishing, writing, or parsing one request.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    source: Option<Box<dyn StdError + Send + Sync>>,
}

/// Stable classification for a blocking client failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ErrorKind {
    AbsoluteUriRequired,
    UnsupportedScheme,
    TlsUnavailable,
    Connect,
    Tls,
    Alpn,
    UnsupportedVersion,
    UnsupportedUpgrade,
    SendRequest,
    Response,
    Timeout,
}

impl Error {
    fn new(kind: ErrorKind) -> Self {
        Self { kind, source: None }
    }

    fn with_source(kind: ErrorKind, source: impl Into<Box<dyn StdError + Send + Sync>>) -> Self {
        Self {
            kind,
            source: Some(source.into()),
        }
    }

    fn from_io(kind: ErrorKind, error: io::Error) -> Self {
        let kind = if is_timeout(&error) {
            ErrorKind::Timeout
        } else {
            kind
        };
        Self::with_source(kind, error)
    }

    /// Returns the boundary at which this request failed.
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Reports whether the configured blocking I/O timer expired.
    pub const fn is_timeout(&self) -> bool {
        matches!(self.kind, ErrorKind::Timeout)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.kind {
            ErrorKind::AbsoluteUriRequired => "client requests require an absolute URI",
            ErrorKind::UnsupportedScheme => "request URI scheme is unsupported",
            ErrorKind::TlsUnavailable => "HTTPS requires the `tls` feature",
            ErrorKind::Connect => "connection establishment failed",
            ErrorKind::Tls => "TLS negotiation or certificate validation failed",
            ErrorKind::Alpn => "TLS ALPN did not select HTTP/1.1",
            ErrorKind::UnsupportedVersion => "request HTTP version is unsupported",
            ErrorKind::UnsupportedUpgrade => "HTTP/1 protocol upgrades are unsupported",
            ErrorKind::SendRequest => "HTTP request dispatch failed",
            ErrorKind::Response => "HTTP response framing failed",
            ErrorKind::Timeout => "blocking HTTP operation timed out",
        })
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

/// Blocking response body that owns its TCP or TLS connection.
pub struct ResponseBody {
    connection: Connection,
    buffered: Vec<u8>,
    mode: BodyMode,
}

impl ResponseBody {
    /// Sets the timeout for each subsequent blocking body read.
    ///
    /// `None` restores the platform's default blocking behavior. A zero
    /// duration is rejected by `TcpStream` and is therefore not a timeout
    /// shorthand.
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        if self.is_complete() || !self.buffered.is_empty() {
            return Ok(());
        }
        self.connection.set_read_timeout(timeout)
    }

    /// Reports whether HTTP framing has already reached the end of this body.
    ///
    /// This is true after an empty or fixed-length body is consumed, or after
    /// the final chunk and trailers are parsed. Close-delimited bodies need an
    /// EOF read before their completion can be known.
    pub fn is_complete(&self) -> bool {
        matches!(
            self.mode,
            BodyMode::Empty | BodyMode::ContentLength(0) | BodyMode::Chunked { complete: true, .. }
        )
    }

    fn read_buffered(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if !self.buffered.is_empty() {
            let count = output.len().min(self.buffered.len());
            output[..count].copy_from_slice(&self.buffered[..count]);
            self.buffered.drain(..count);
            return Ok(count);
        }
        self.connection.read(output)
    }

    fn read_exact_buffered(&mut self, output: &mut [u8]) -> io::Result<()> {
        let mut offset = 0;
        while offset < output.len() {
            let read = self.read_buffered(&mut output[offset..])?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "HTTP response ended before its declared body framing completed",
                ));
            }
            offset += read;
        }
        Ok(())
    }

    fn read_line(&mut self, max_len: usize) -> io::Result<Vec<u8>> {
        loop {
            if let Some(end) = find_crlf(&self.buffered) {
                let line = self.buffered.drain(..end).collect();
                self.buffered.drain(..2);
                return Ok(line);
            }
            if self.buffered.len() >= max_len {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "HTTP chunk line exceeds the configured limit",
                ));
            }
            let mut input = [0_u8; 4096];
            let read = self.connection.read(&mut input)?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "HTTP response ended in a chunk line",
                ));
            }
            self.buffered.extend_from_slice(&input[..read]);
        }
    }

    fn consume_trailers(&mut self) -> io::Result<()> {
        let mut total: usize = 0;
        loop {
            let line = self.read_line(MAX_CHUNK_LINE_BYTES)?;
            total = total.saturating_add(line.len() + 2);
            if total > MAX_TRAILER_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "HTTP response trailers exceed the configured limit",
                ));
            }
            if line.is_empty() {
                return Ok(());
            }
            if !line.contains(&b':') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "malformed HTTP response trailer",
                ));
            }
        }
    }

    fn read_chunked(&mut self, output: &mut [u8]) -> io::Result<usize> {
        loop {
            let (remaining, separator_due, complete) = match self.mode {
                BodyMode::Chunked {
                    remaining,
                    separator_due,
                    complete,
                } => (remaining, separator_due, complete),
                _ => unreachable!("chunked reader requires chunked body state"),
            };
            if complete {
                return Ok(0);
            }
            if remaining == 0 {
                if separator_due {
                    let mut separator = [0_u8; 2];
                    self.read_exact_buffered(&mut separator)?;
                    if separator != *b"\r\n" {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "HTTP chunk data was not followed by CRLF",
                        ));
                    }
                }
                let line = self.read_line(MAX_CHUNK_LINE_BYTES)?;
                let size = parse_chunk_size(&line)?;
                if size == 0 {
                    self.consume_trailers()?;
                    self.mode = BodyMode::Chunked {
                        remaining: 0,
                        separator_due: false,
                        complete: true,
                    };
                    return Ok(0);
                }
                self.mode = BodyMode::Chunked {
                    remaining: size,
                    separator_due: false,
                    complete: false,
                };
                continue;
            }

            let wanted = output
                .len()
                .min(usize::try_from(remaining).unwrap_or(usize::MAX));
            let read = self.read_buffered(&mut output[..wanted])?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "HTTP response ended in a chunk",
                ));
            }
            self.mode = BodyMode::Chunked {
                remaining: remaining - u64::try_from(read).expect("usize fits in u64"),
                separator_due: remaining == u64::try_from(read).expect("usize fits in u64"),
                complete: false,
            };
            return Ok(read);
        }
    }
}

impl Read for ResponseBody {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        match self.mode {
            BodyMode::Empty => Ok(0),
            BodyMode::Eof => self.read_buffered(output),
            BodyMode::ContentLength(remaining) => {
                if remaining == 0 {
                    return Ok(0);
                }
                let wanted = output
                    .len()
                    .min(usize::try_from(remaining).unwrap_or(usize::MAX));
                let read = self.read_buffered(&mut output[..wanted])?;
                if read == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "HTTP response ended before its Content-Length",
                    ));
                }
                self.mode = BodyMode::ContentLength(
                    remaining - u64::try_from(read).expect("usize fits in u64"),
                );
                Ok(read)
            }
            BodyMode::Chunked { .. } => self.read_chunked(output),
        }
    }
}

impl fmt::Debug for ResponseBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResponseBody")
            .finish_non_exhaustive()
    }
}

enum Connection {
    Tcp(TcpStream),
    #[cfg(feature = "tls")]
    Tls(rustls::StreamOwned<rustls::ClientConnection, TcpStream>),
}

impl Connection {
    fn set_timeouts(
        &self,
        read_timeout: Option<Duration>,
        write_timeout: Option<Duration>,
    ) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream
                .set_read_timeout(read_timeout)
                .and_then(|()| stream.set_write_timeout(write_timeout)),
            #[cfg(feature = "tls")]
            Self::Tls(stream) => stream
                .sock
                .set_read_timeout(read_timeout)
                .and_then(|()| stream.sock.set_write_timeout(write_timeout)),
        }
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.set_read_timeout(timeout),
            #[cfg(feature = "tls")]
            Self::Tls(stream) => stream.sock.set_read_timeout(timeout),
        }
    }
}

impl Read for Connection {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(output),
            #[cfg(feature = "tls")]
            Self::Tls(stream) => stream.read(output),
        }
    }
}

impl Write for Connection {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(input),
            #[cfg(feature = "tls")]
            Self::Tls(stream) => stream.write(input),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            #[cfg(feature = "tls")]
            Self::Tls(stream) => stream.flush(),
        }
    }
}

#[derive(Clone, Copy)]
enum BodyMode {
    Empty,
    ContentLength(u64),
    Chunked {
        remaining: u64,
        separator_due: bool,
        complete: bool,
    },
    Eof,
}

fn connect_tcp(host: &str, port: u16, timeout: Option<Duration>) -> io::Result<TcpStream> {
    let mut last_error = None;
    let mut addresses = (host, port).to_socket_addrs()?;
    while let Some(address) = addresses.next() {
        let result = match timeout {
            Some(timeout) => TcpStream::connect_timeout(&address, timeout),
            None => TcpStream::connect(address),
        };
        match result {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not resolve the direct-origin host to a socket address",
        )
    }))
}

fn write_request(
    connection: &mut Connection,
    mut parts: http::request::Parts,
    content_length: u64,
    body: impl Read,
) -> Result<(), Error> {
    if !parts.headers.contains_key(CONTENT_LENGTH) && !parts.headers.contains_key(TRANSFER_ENCODING)
    {
        let value = HeaderValue::from_str(&content_length.to_string())
            .expect("the decimal length of a body is a valid HTTP header value");
        parts.headers.insert(CONTENT_LENGTH, value);
    }
    // The client has no idle pool. State this explicitly so a server can
    // terminate close-delimited responses and body drop never strands a peer.
    if !parts.headers.contains_key(CONNECTION) {
        parts
            .headers
            .insert(CONNECTION, HeaderValue::from_static("close"));
    }

    write!(connection, "{} {} HTTP/1.1\r\n", parts.method, parts.uri)
        .map_err(|error| Error::from_io(ErrorKind::SendRequest, error))?;
    for (name, value) in &parts.headers {
        connection
            .write_all(name.as_str().as_bytes())
            .and_then(|()| connection.write_all(b": "))
            .and_then(|()| connection.write_all(value.as_bytes()))
            .and_then(|()| connection.write_all(b"\r\n"))
            .map_err(|error| Error::from_io(ErrorKind::SendRequest, error))?;
    }
    connection
        .write_all(b"\r\n")
        .map_err(|error| Error::from_io(ErrorKind::SendRequest, error))?;
    let sent = io::copy(&mut body.take(content_length), connection)
        .map_err(|error| Error::from_io(ErrorKind::SendRequest, error))?;
    if sent != content_length {
        return Err(Error::with_source(
            ErrorKind::SendRequest,
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "request body ended before its declared length",
            ),
        ));
    }
    connection
        .flush()
        .map_err(|error| Error::from_io(ErrorKind::SendRequest, error))
}

fn read_response(
    mut connection: Connection,
    head_request: bool,
) -> Result<Response<ResponseBody>, Error> {
    let mut buffered = Vec::new();
    loop {
        let end = read_response_head(&mut connection, &mut buffered)?;
        let head = buffered.drain(..end).collect::<Vec<_>>();
        buffered.drain(..4);
        let (status, version, headers) = parse_response_head(&head)?;
        // RFC 9112 interim responses do not carry a message body. Continue
        // until the final response while retaining any bytes already read.
        if status.is_informational() && status != StatusCode::SWITCHING_PROTOCOLS {
            continue;
        }
        if status == StatusCode::SWITCHING_PROTOCOLS {
            return Err(Error::new(ErrorKind::UnsupportedUpgrade));
        }
        let mode = response_body_mode(&headers, status, head_request)?;
        let mut response = Response::new(ResponseBody {
            connection,
            buffered,
            mode,
        });
        *response.status_mut() = status;
        *response.version_mut() = version;
        *response.headers_mut() = headers;
        return Ok(response);
    }
}

fn read_response_head(connection: &mut Connection, buffered: &mut Vec<u8>) -> Result<usize, Error> {
    loop {
        if let Some(end) = find_head_end(buffered) {
            return Ok(end);
        }
        if buffered.len() >= MAX_RESPONSE_HEAD_BYTES {
            return Err(response_error(
                "HTTP response headers exceed the configured limit",
            ));
        }
        let mut input = [0_u8; 4096];
        let read = connection
            .read(&mut input)
            .map_err(|error| Error::from_io(ErrorKind::Response, error))?;
        if read == 0 {
            return Err(response_error(
                "connection closed before HTTP response headers",
            ));
        }
        buffered.extend_from_slice(&input[..read]);
    }
}

fn parse_response_head(head: &[u8]) -> Result<(StatusCode, Version, HeaderMap), Error> {
    let mut lines = head.split(|byte| *byte == b'\n');
    let status_line = lines
        .next()
        .map(strip_line_crlf)
        .transpose()?
        .ok_or_else(|| response_error("malformed HTTP response status line"))?;
    let status_line = std::str::from_utf8(status_line)
        .map_err(|_| response_error("HTTP response status line was not ASCII"))?;
    let mut fields = status_line.splitn(3, ' ');
    let version = match fields.next() {
        Some("HTTP/1.0") => Version::HTTP_10,
        Some("HTTP/1.1") => Version::HTTP_11,
        _ => return Err(response_error("response did not use HTTP/1.x")),
    };
    let status = fields
        .next()
        .ok_or_else(|| response_error("HTTP response omitted its status code"))?
        .parse::<u16>()
        .ok()
        .and_then(|status| StatusCode::from_u16(status).ok())
        .ok_or_else(|| response_error("HTTP response supplied an invalid status code"))?;

    let mut headers = HeaderMap::new();
    for line in lines {
        let line = strip_line_crlf(line)?;
        if line.is_empty() {
            return Err(response_error(
                "unexpected empty line in HTTP response headers",
            ));
        }
        let separator = line
            .iter()
            .position(|byte| *byte == b':')
            .ok_or_else(|| response_error("malformed HTTP response header"))?;
        let (name, value) = (&line[..separator], &line[separator + 1..]);
        let name = HeaderName::from_bytes(name)
            .map_err(|_| response_error("HTTP response header name was invalid"))?;
        let value = HeaderValue::from_bytes(trim_ows(value))
            .map_err(|_| response_error("HTTP response header value was invalid"))?;
        headers.append(name, value);
    }
    Ok((status, version, headers))
}

fn strip_line_crlf(line: &[u8]) -> Result<&[u8], Error> {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.contains(&b'\r') {
        return Err(response_error(
            "HTTP response header was not CRLF terminated",
        ));
    }
    Ok(line)
}

fn response_body_mode(
    headers: &HeaderMap,
    status: StatusCode,
    head_request: bool,
) -> Result<BodyMode, Error> {
    if head_request
        || status.is_informational()
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED
    {
        return Ok(BodyMode::Empty);
    }
    let transfer_codings = headers
        .get_all(TRANSFER_ENCODING)
        .iter()
        .flat_map(|value| value.as_bytes().split(|byte| *byte == b','))
        .map(trim_ows)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if !transfer_codings.is_empty() {
        if headers.contains_key(CONTENT_LENGTH)
            || transfer_codings.len() != 1
            || !transfer_codings[0].eq_ignore_ascii_case(b"chunked")
        {
            return Err(response_error(
                "HTTP response has unsupported or ambiguous transfer framing",
            ));
        }
        return Ok(BodyMode::Chunked {
            remaining: 0,
            separator_due: false,
            complete: false,
        });
    }

    let mut content_length = None;
    for value in headers.get_all(CONTENT_LENGTH).iter() {
        let value = std::str::from_utf8(trim_ows(value.as_bytes()))
            .map_err(|_| response_error("HTTP Content-Length was not ASCII"))?
            .parse::<u64>()
            .map_err(|_| response_error("HTTP Content-Length was invalid"))?;
        match content_length {
            Some(previous) if previous != value => {
                return Err(response_error(
                    "HTTP response has conflicting Content-Length values",
                ))
            }
            _ => content_length = Some(value),
        }
    }
    Ok(match content_length {
        Some(length) => BodyMode::ContentLength(length),
        None => BodyMode::Eof,
    })
}

fn find_head_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn find_crlf(bytes: &[u8]) -> Option<usize> {
    bytes.windows(2).position(|window| window == b"\r\n")
}

fn trim_ows(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.first(), Some(b' ' | b'\t')) {
        bytes = &bytes[1..];
    }
    while matches!(bytes.last(), Some(b' ' | b'\t')) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn parse_chunk_size(line: &[u8]) -> io::Result<u64> {
    let size = line.split(|byte| *byte == b';').next().unwrap_or_default();
    let size = std::str::from_utf8(trim_ows(size))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "HTTP chunk size was not ASCII"))?;
    u64::from_str_radix(size, 16)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "HTTP chunk size was invalid"))
}

fn response_error(message: &'static str) -> Error {
    Error::with_source(
        ErrorKind::Response,
        io::Error::new(io::ErrorKind::InvalidData, message),
    )
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

#[cfg(feature = "tls")]
fn default_tls_config() -> rustls::ClientConfig {
    ClientTlsConfigBuilder::new()
        .build()
        .expect("Graviola supports Rustls safe default protocol versions")
}

#[cfg(feature = "tls")]
#[derive(Debug)]
struct InvalidServerName(String);

#[cfg(feature = "tls")]
impl fmt::Display for InvalidServerName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid TLS server name {:?}", self.0)
    }
}

#[cfg(feature = "tls")]
impl StdError for InvalidServerName {}

#[cfg(feature = "tls")]
#[derive(Debug)]
struct UnsupportedAlpn(Vec<u8>);

#[cfg(feature = "tls")]
impl fmt::Display for UnsupportedAlpn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "server selected unsupported ALPN {:?}", self.0)
    }
}

#[cfg(feature = "tls")]
impl StdError for UnsupportedAlpn {}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration;

    use http::Request;

    use super::{Client, ErrorKind};

    #[test]
    fn writes_normalized_request_and_reads_chunked_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (request_sent, request_received) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            loop {
                let mut input = [0_u8; 1024];
                let read = stream.read(&mut input).unwrap();
                request.extend_from_slice(&input[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            request_sent.send(request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\nfirst \r\n6\r\nsecond\r\n0\r\n\r\n",
                )
                .unwrap();
        });

        let request = Request::builder()
            .uri(format!("http://{address}/catalog?available=true"))
            .body(Vec::new())
            .unwrap();
        let mut response = Client::new().request(request).unwrap();
        let mut body = Vec::new();
        response.body_mut().read_to_end(&mut body).unwrap();
        assert_eq!(body, b"first second");

        let request = request_received
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let request = std::str::from_utf8(&request).unwrap();
        assert!(request.starts_with("GET /catalog?available=true HTTP/1.1\r\n"));
        let request_lower = request.to_ascii_lowercase();
        assert!(request_lower.contains(&format!("host: {address}\r\n")));
        assert!(request_lower.contains("content-length: 0\r\n"));
        assert!(request_lower.contains("connection: close\r\n"));
        server.join().unwrap();
    }

    #[test]
    fn streams_exactly_the_declared_request_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            stream.read_to_end(&mut request).unwrap_or_default();
            request
        });

        // The reader holds more than the declared length; only 5 bytes may be sent.
        let request = Request::builder()
            .method("PUT")
            .uri(format!("http://{address}/object"))
            .body(std::io::Cursor::new(b"hello world".to_vec()))
            .unwrap();
        let error = Client::new()
            .request_streaming(request, 5, Some(Duration::from_millis(200)))
            .err()
            .expect("the server never responds");
        assert!(error.is_timeout());

        let request = String::from_utf8(server.join().unwrap()).unwrap();
        assert!(request.starts_with("PUT /object HTTP/1.1\r\n"));
        assert!(request.to_ascii_lowercase().contains("content-length: 5\r\n"));
        assert!(request.ends_with("\r\n\r\nhello"));
    }

    #[test]
    fn short_streamed_request_body_fails() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut sink = Vec::new();
            stream.read_to_end(&mut sink).unwrap_or_default();
        });

        let request = Request::builder()
            .method("PUT")
            .uri(format!("http://{address}/object"))
            .body(std::io::Cursor::new(b"abc".to_vec()))
            .unwrap();
        let error = Client::new()
            .request_streaming(request, 10, None)
            .err()
            .expect("the reader is shorter than the declared length");
        assert_eq!(error.kind(), ErrorKind::SendRequest);
        server.join().unwrap();
    }

    #[test]
    fn classifies_response_header_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            std::thread::sleep(Duration::from_millis(100));
        });

        let request = Request::builder()
            .uri(format!("http://{address}/slow"))
            .body(Vec::new())
            .unwrap();
        let error = Client::new()
            .request_with_timeout(request, Some(Duration::from_millis(20)))
            .expect_err("a stalled response head must time out");
        assert_eq!(error.kind(), ErrorKind::Timeout);
        server.join().unwrap();
    }

    #[test]
    fn response_body_read_timeout_can_be_refreshed_after_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nx")
                .unwrap();
        });

        let request = Request::builder()
            .uri(format!("http://{address}/timeout"))
            .body(Vec::new())
            .unwrap();
        let mut response = Client::new()
            .request_with_timeout(request, Some(Duration::from_secs(1)))
            .unwrap();
        let mut byte = [0_u8; 1];
        response.body_mut().read_exact(&mut byte).unwrap();
        assert!(response.body().is_complete());
        server.join().unwrap();
        response
            .body_mut()
            .set_read_timeout(Some(Duration::from_millis(900)))
            .expect("a completed body does not reconfigure its closed socket");
    }

    #[test]
    fn streaming_post_body_can_refresh_its_idle_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (first_sent, first_received) = mpsc::channel();
        let (release, wait_for_release) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nfirst ",
                )
                .unwrap();
            stream.flush().unwrap();
            first_sent.send(()).unwrap();
            wait_for_release.recv().unwrap();
            stream.write_all(b"second").unwrap();
        });

        let request = Request::builder()
            .method("POST")
            .uri(format!("http://{address}/stream"))
            .header("content-type", "application/json")
            .body(b"{}".to_vec())
            .unwrap();
        let mut response = Client::new()
            .request_with_timeout(request, Some(Duration::from_secs(5)))
            .unwrap();
        first_received.recv_timeout(Duration::from_secs(1)).unwrap();
        response
            .body_mut()
            .set_read_timeout(Some(Duration::new(4, 999_000_000)))
            .expect("the streaming body accepts its first idle timeout");
        let mut first = [0_u8; 6];
        response.body_mut().read_exact(&mut first).unwrap();
        response
            .body_mut()
            .set_read_timeout(Some(Duration::new(4, 998_000_000)))
            .expect("the streaming body can refresh its idle timeout");
        release.send(()).unwrap();
        server.join().unwrap();
    }
}
