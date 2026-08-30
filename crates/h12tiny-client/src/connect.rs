//! Direct DNS, TCP, and TLS connection establishment.
//!
//! This is intentionally a small replacement for Hyper-util's Tokio
//! `HttpConnector`: no proxy discovery, socket tuning, interface binding, or
//! platform TLS. The default path resolves names with `async-net` and connects
//! sockets with `async-io`; Rustls authenticates HTTPS and selects the
//! application protocol with ALPN.
//!
//! System DNS itself is blocking, but [`SystemResolver`] delegates that work to
//! `async-net`'s process-global `blocking` executor, keeping it out of the
//! task polling a connection. Applications that need resolver-specific
//! cancellation, caching, or lookup protocols can use [`Resolver`] while
//! retaining h12tiny's TCP, TLS, ALPN, and pooling policy.

use std::collections::VecDeque;
use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::io;
use std::net::{SocketAddr, TcpStream as StdTcpStream};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_io::Async;
use futures_util::future::{self, Either};
use futures_util::stream::{FuturesUnordered, StreamExt};
use http::Uri;
use hyper::rt::Timer;

use h12tiny_core::io::FuturesIo;
use h12tiny_core::runtime::AsyncIoTimer;

use super::RequestOptions;

/// A raw connection stream accepted by a custom [`Dialer`].
///
/// It is intentionally a Hyper-runtime stream rather than a futures-I/O
/// stream: custom transports may already have their own I/O adaptation. Most
/// futures-I/O callers can return `h12tiny_core::io::FuturesIo(stream)`.
pub trait ConnectionIo: hyper::rt::Read + hyper::rt::Write + Send + Unpin {}

impl<T> ConnectionIo for T where T: hyper::rt::Read + hyper::rt::Write + Send + Unpin {}

type BoxedIo = Box<dyn ConnectionIo>;

/// A TCP stream supplied by a custom [`TcpDialer`].
///
/// The stream is deliberately a futures-I/O stream rather than a Hyper
/// runtime stream. h12tiny wraps it at the HTTP boundary, after it has
/// retained ownership of TLS, ALPN, and HTTP protocol selection.
pub trait TcpConnectionIo: futures_io::AsyncRead + futures_io::AsyncWrite + Send + Unpin {}

impl<T> TcpConnectionIo for T where T: futures_io::AsyncRead + futures_io::AsyncWrite + Send + Unpin {}

type BoxedTcpIo = Box<dyn TcpConnectionIo>;

/// A successful custom TCP establishment.
pub struct TcpConnected {
    io: BoxedTcpIo,
    local_addr: Option<SocketAddr>,
    peer_addr: Option<SocketAddr>,
}

impl TcpConnected {
    /// Wrap a caller-established futures-I/O TCP stream.
    pub fn new<T>(io: T) -> Self
    where
        T: TcpConnectionIo + 'static,
    {
        Self {
            io: Box::new(io),
            local_addr: None,
            peer_addr: None,
        }
    }

    /// Attaches socket addresses known by a custom transport.
    pub fn with_addresses(
        mut self,
        local_addr: Option<SocketAddr>,
        peer_addr: Option<SocketAddr>,
    ) -> Self {
        self.local_addr = local_addr;
        self.peer_addr = peer_addr;
        self
    }
}

/// A successful custom connection establishment.
///
/// The caller declares the HTTP capability selected by its transport. A
/// custom TLS dialer must perform its own ALPN selection before returning
/// this value; h12tiny deliberately does not infer protocol from an opaque
/// custom stream.
pub struct Connected {
    pub(crate) io: BoxedIo,
    pub(crate) protocol: super::ConnectionProtocol,
    pub(crate) info: super::ConnectionInfo,
}

impl Connected {
    /// Wrap a transport stream and its negotiated HTTP capability.
    pub fn new<T>(io: T, protocol: super::ConnectionProtocol) -> Self
    where
        T: ConnectionIo + 'static,
    {
        Self {
            io: Box::new(io),
            protocol,
            info: super::ConnectionInfo::new(protocol),
        }
    }

    /// Returns the protocol capability selected during establishment.
    pub fn protocol(&self) -> super::ConnectionProtocol {
        self.protocol
    }

    /// Attaches socket addresses known by a custom transport.
    ///
    /// Default TCP connections populate these values automatically. Custom
    /// dialers can call this before returning the connection so the client
    /// records the same information in [`super::ResponseInfo`].
    pub fn with_addresses(
        mut self,
        local_addr: Option<SocketAddr>,
        peer_addr: Option<SocketAddr>,
    ) -> Self {
        self.info.local_addr = local_addr;
        self.info.peer_addr = peer_addr;
        self
    }
}

/// Erased error returned by a custom [`Dialer`].
pub type DialError = Box<dyn StdError + Send + Sync>;

/// Future returned by [`Dialer::connect`].
pub type DialFuture = Pin<Box<dyn Future<Output = Result<Connected, DialError>> + Send + 'static>>;

/// Future returned by [`TcpDialer::connect`].
pub type TcpDialFuture =
    Pin<Box<dyn Future<Output = Result<TcpConnected, DialError>> + Send + 'static>>;

/// Replaces only connection establishment while retaining the client's origin
/// normalization, protocol handshake, and per-origin pool.
///
/// The URI is always the normalized absolute origin, and `require_http2` is
/// true only when the client is configured for HTTP/2-only operation. A
/// dialer is responsible for returning a stream whose declared protocol is
/// compatible with that request. It does not receive individual requests and
/// cannot implement hidden proxy, redirect, or retry policy. It receives the
/// request's phase options so a custom transport can honor its own DNS, TCP,
/// and TLS boundaries. A custom dialer owns those opaque phases completely;
/// h12tiny does not collapse them into a misleading outer deadline.
pub trait Dialer: Send + Sync + 'static {
    fn connect(&self, origin: Uri, require_http2: bool, options: RequestOptions) -> DialFuture;
}

/// Future returned by [`Resolver::resolve`].
///
/// Resolver failures retain their source error through connection
/// establishment and are classified as [`super::ErrorKind::Connect`] by the
/// client.
pub type ResolveFuture =
    Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, DialError>> + Send + 'static>>;

/// Resolves one direct-origin host and port into candidate socket addresses.
///
/// h12tiny preserves order within each address family, starts IPv6 first when
/// both families are present, and races later candidates after the configured
/// Happy Eyeballs delay. A resolver is used only by the default TCP path;
/// [`Dialer`] and [`TcpDialer`] retain complete ownership of their own name
/// resolution and socket policy.
pub trait Resolver: Send + Sync + 'static {
    fn resolve(&self, host: String, port: u16) -> ResolveFuture;
}

/// The default system resolver used by [`Connector`].
///
/// It delegates to [`async_net::resolve`], which runs the platform resolver on
/// `blocking`'s process-global executor. This keeps an in-flight system lookup
/// from blocking the task polling a connection or delaying
/// [`ConnectorBuilder::connect_timeout`]. Dropping the returned future stops
/// waiting for the lookup, but cannot interrupt a platform lookup that has
/// already started.
///
/// `SystemResolver` intentionally has no cache or resolver-specific timeout.
/// Applications that need either, or a DNS protocol other than the platform
/// resolver, should install [`Resolver`] through [`ConnectorBuilder::resolver`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve(&self, host: String, port: u16) -> ResolveFuture {
        Box::pin(async move {
            async_net::resolve((host, port))
                .await
                .map_err(|error| Box::new(error) as DialError)
        })
    }
}

/// Replaces only capability-aware TCP establishment.
///
/// h12tiny receives the normalized absolute origin, while the caller retains
/// ownership of DNS and socket policy. For `https` origins h12tiny then runs
/// the returned stream through its configured Rustls client, selects ALPN, and
/// performs the HTTP/1 or HTTP/2 handshake. The dialer must not perform TLS or
/// HTTP negotiation itself.
pub trait TcpDialer: Send + Sync + 'static {
    fn connect(&self, origin: Uri, options: RequestOptions) -> TcpDialFuture;
}

#[derive(Debug)]
pub(crate) enum Error {
    MissingHost,
    UnsupportedScheme(String),
    #[cfg(not(feature = "tls"))]
    TlsDisabled,
    #[cfg(feature = "tls")]
    InvalidServerName(String),
    #[cfg(feature = "tls")]
    UnexpectedAlpn(Vec<u8>),
    #[cfg(feature = "tls")]
    RequiredHttp2NotNegotiated,
    Connect(std::io::Error),
    Resolve(DialError),
    Custom(DialError),
    DnsTimeout,
    ConnectTimeout,
    #[cfg(feature = "tls")]
    TlsTimeout,
    Timeout,
    #[cfg(feature = "tls")]
    Tls(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHost => f.write_str("URI has no host"),
            Self::UnsupportedScheme(scheme) => write!(f, "unsupported URI scheme {scheme:?}"),
            #[cfg(not(feature = "tls"))]
            Self::TlsDisabled => f.write_str("HTTPS requires the `tls` feature"),
            #[cfg(feature = "tls")]
            Self::InvalidServerName(host) => write!(f, "invalid TLS server name {host:?}"),
            #[cfg(feature = "tls")]
            Self::UnexpectedAlpn(protocol) => {
                write!(f, "server selected unsupported ALPN {protocol:?}")
            }
            #[cfg(feature = "tls")]
            Self::RequiredHttp2NotNegotiated => {
                f.write_str("HTTP/2 was required but ALPN did not select h2")
            }
            Self::Connect(error) => error.fmt(f),
            Self::Resolve(error) => error.fmt(f),
            Self::Custom(error) => error.fmt(f),
            Self::DnsTimeout => f.write_str("DNS resolution timed out"),
            Self::ConnectTimeout => f.write_str("TCP connection establishment timed out"),
            #[cfg(feature = "tls")]
            Self::TlsTimeout => f.write_str("TLS negotiation timed out"),
            Self::Timeout => f.write_str("connection establishment timed out"),
            #[cfg(feature = "tls")]
            Self::Tls(error) => error.fmt(f),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Connect(error) => Some(error),
            Self::Resolve(error) => Some(error.as_ref()),
            Self::Custom(error) => Some(error.as_ref()),
            #[cfg(feature = "tls")]
            Self::Tls(error) => Some(error),
            _ => None,
        }
    }
}

impl Error {
    pub(crate) fn client_error_kind(&self) -> super::ErrorKind {
        match self {
            Self::UnsupportedScheme(_) => super::ErrorKind::UnsupportedScheme,
            #[cfg(feature = "tls")]
            Self::UnexpectedAlpn(_) | Self::RequiredHttp2NotNegotiated => super::ErrorKind::Alpn,
            #[cfg(feature = "tls")]
            Self::InvalidServerName(_) | Self::Tls(_) => super::ErrorKind::Tls,
            #[cfg(not(feature = "tls"))]
            Self::TlsDisabled => super::ErrorKind::Tls,
            Self::MissingHost
            | Self::Connect(_)
            | Self::Resolve(_)
            | Self::Custom(_)
            | Self::Timeout => super::ErrorKind::Connect,
            Self::DnsTimeout => super::ErrorKind::DnsTimeout,
            Self::ConnectTimeout => super::ErrorKind::ConnectTimeout,
            #[cfg(feature = "tls")]
            Self::TlsTimeout => super::ErrorKind::TlsTimeout,
        }
    }
}

/// A cloneable direct-origin connector. One client has one TLS policy, which
/// is intentionally not included in the pool key.
#[derive(Clone)]
pub struct Connector {
    kind: ConnectorKind,
    resolver: Arc<dyn Resolver>,
    happy_eyeballs_timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    timer: Arc<dyn Timer + Send + Sync>,
    #[cfg(feature = "tls")]
    tls: Arc<rustls::ClientConfig>,
}

#[derive(Clone)]
enum ConnectorKind {
    Default,
    Custom(Arc<dyn Dialer>),
    Tcp(Arc<dyn TcpDialer>),
}

/// Small builder for connection-establishment policy.
///
/// This builder intentionally configures only dialing and the optional
/// establishment timeout. Request deadlines and logical body idle timeouts
/// belong to higher layers.
pub struct ConnectorBuilder {
    connector: Connector,
}

/// Builds a Rustls client configuration without relying on Rustls' process
/// global provider selection.
///
/// [`ClientTlsConfigBuilder::new`] uses h12tiny's Graviola provider, the
/// WebPKI roots, no client certificate, and ALPN offerings for HTTP/2 then
/// HTTP/1.1. Callers can replace the root store, ALPN list, or client
/// authentication, or restrict TLS protocol versions while retaining that
/// explicit provider. Applications with a different Rustls provider can use
/// [`ClientTlsConfigBuilder::with_provider`] instead; this is useful when
/// several Rustls consumers share one process.
///
/// For policies requiring a custom verifier or other Rustls-only settings,
/// construct a [`rustls::ClientConfig`] directly and pass it to
/// [`Connector::with_tls_config`] or [`ConnectorBuilder::tls_config`].
#[cfg(feature = "tls")]
pub struct ClientTlsConfigBuilder {
    provider: Arc<rustls::crypto::CryptoProvider>,
    roots: rustls::RootCertStore,
    alpn_protocols: Vec<Vec<u8>>,
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
    /// Starts with h12tiny's explicit Graviola provider and standard HTTPS
    /// client defaults.
    pub fn new() -> Self {
        Self::with_provider(Arc::new(rustls_graviola::default_provider()))
    }

    /// Starts with an explicitly supplied Rustls provider.
    ///
    /// [`Self::build`] returns any compatibility error, for example when the
    /// provider cannot support Rustls' safe default protocol versions. This
    /// method never installs or reads Rustls' process-global provider.
    pub fn with_provider(provider: Arc<rustls::crypto::CryptoProvider>) -> Self {
        Self {
            provider,
            roots: rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
            alpn_protocols: vec![b"h2".to_vec(), b"http/1.1".to_vec()],
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

    /// Replaces the ALPN protocol offerings sent during TLS negotiation.
    ///
    /// The default offers HTTP/2 followed by HTTP/1.1. Supplying an empty
    /// list deliberately disables ALPN negotiation.
    pub fn alpn_protocols(mut self, protocols: impl IntoIterator<Item = Vec<u8>>) -> Self {
        self.alpn_protocols = protocols.into_iter().collect();
        self
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

    /// Finishes the Rustls configuration.
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
        config.alpn_protocols = self.alpn_protocols;
        Ok(config)
    }
}

#[cfg(feature = "tls")]
impl Default for ClientTlsConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for Connector {
    fn default() -> Self {
        #[cfg(feature = "tls")]
        {
            return Self::with_tls_config(default_tls_config());
        }
        #[cfg(not(feature = "tls"))]
        {
            Self {
                kind: ConnectorKind::Default,
                resolver: Arc::new(SystemResolver),
                happy_eyeballs_timeout: Some(Duration::from_millis(250)),
                connect_timeout: None,
                timer: Arc::new(AsyncIoTimer),
            }
        }
    }
}

impl Connector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a small connector-policy builder.
    pub fn builder() -> ConnectorBuilder {
        ConnectorBuilder {
            connector: Self::new(),
        }
    }

    /// Uses a custom dialer while retaining client pooling and request
    /// normalization. This is equivalent to [`Connector::builder`] followed
    /// by [`ConnectorBuilder::dialer`].
    pub fn with_dialer<D>(dialer: D) -> Self
    where
        D: Dialer,
    {
        Self::builder().dialer(dialer).build()
    }

    /// Uses a caller-provided futures-I/O TCP establishment path while
    /// retaining h12tiny's TLS, ALPN, protocol, and pooling policy.
    pub fn with_tcp_dialer<D>(dialer: D) -> Self
    where
        D: TcpDialer,
    {
        Self::builder().tcp_dialer(dialer).build()
    }

    /// Uses an asynchronous resolver while retaining h12tiny's TCP, TLS,
    /// ALPN, protocol, and pooling policy.
    ///
    /// The resolver is consulted only by the default TCP path. Supplying a
    /// [`Dialer`] or [`TcpDialer`] replaces that path and bypasses it.
    pub fn with_resolver<R>(resolver: R) -> Self
    where
        R: Resolver,
    {
        Self::builder().resolver(resolver).build()
    }

    #[cfg(feature = "tls")]
    pub(crate) fn force_http1(&mut self) {
        let mut config = (*self.tls).clone();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        self.tls = Arc::new(config);
    }

    #[cfg(not(feature = "tls"))]
    pub(crate) fn force_http1(&mut self) {}

    /// Uses an explicit Rustls client policy. This is the intended hook for
    /// test root stores and private PKI; certificate validation remains owned
    /// by Rustls.
    #[cfg(feature = "tls")]
    pub fn with_tls_config(config: rustls::ClientConfig) -> Self {
        Self {
            kind: ConnectorKind::Default,
            resolver: Arc::new(SystemResolver),
            happy_eyeballs_timeout: Some(Duration::from_millis(250)),
            connect_timeout: None,
            timer: Arc::new(AsyncIoTimer),
            tls: Arc::new(config),
        }
    }

    /// Uses h12tiny's standard TLS policy with an explicitly supplied Rustls
    /// provider. This does not install or consult Rustls' process-global
    /// provider selection.
    #[cfg(feature = "tls")]
    pub fn with_tls_provider(
        provider: Arc<rustls::crypto::CryptoProvider>,
    ) -> Result<Self, rustls::Error> {
        ClientTlsConfigBuilder::with_provider(provider)
            .build()
            .map(Self::with_tls_config)
    }

    #[cfg(test)]
    pub(crate) async fn connect(&self, uri: Uri, require_h2: bool) -> Result<Connected, Error> {
        self.connect_with_options(uri, require_h2, RequestOptions::new())
            .await
    }

    pub(crate) async fn connect_with_options(
        &self,
        uri: Uri,
        require_h2: bool,
        options: RequestOptions,
    ) -> Result<Connected, Error> {
        if options.has_connection_timeout() {
            return self.connect_without_timeout(uri, require_h2, options).await;
        }

        let connect = Box::pin(self.connect_without_timeout(uri, require_h2, options));
        match self.connect_timeout {
            None => connect.await,
            Some(timeout) => match future::select(connect, self.timer.sleep(timeout)).await {
                Either::Left((result, _)) => result,
                Either::Right(_) => Err(Error::Timeout),
            },
        }
    }

    pub(crate) fn sleep(&self, duration: Duration) -> Pin<Box<dyn hyper::rt::Sleep>> {
        self.timer.sleep(duration)
    }

    async fn connect_without_timeout(
        &self,
        uri: Uri,
        require_h2: bool,
        options: RequestOptions,
    ) -> Result<Connected, Error> {
        if let ConnectorKind::Custom(dialer) = &self.kind {
            let connect = async {
                dialer
                    .connect(uri, require_h2, options)
                    .await
                    .map_err(Error::Custom)
            };
            return connect.await;
        }
        // `http::Uri::host()` preserves RFC authority brackets around IPv6
        // literals. DNS and Rustls `ServerName` require the bare address.
        let host = uri
            .host()
            .ok_or(Error::MissingHost)?
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or_else(|| uri.host().expect("host was checked above"))
            .to_owned();
        let scheme = uri
            .scheme_str()
            .ok_or_else(|| Error::UnsupportedScheme("".to_owned()))?;
        if !matches!(scheme, "http" | "https") {
            return Err(Error::UnsupportedScheme(scheme.to_owned()));
        }
        #[cfg(not(feature = "tls"))]
        if scheme == "https" {
            // A TCP dialer is deliberately below h12tiny's TLS boundary. In
            // a TLS-free build HTTPS is impossible, so reject it before a
            // caller-owned dialer performs an unnecessary socket side effect.
            return Err(Error::TlsDisabled);
        }
        let tcp = match &self.kind {
            ConnectorKind::Tcp(dialer) => {
                let connect = async {
                    dialer
                        .connect(uri.clone(), options)
                        .await
                        .map_err(Error::Custom)
                };
                Some(connect.await?)
            }
            ConnectorKind::Default | ConnectorKind::Custom(_) => None,
        };
        match scheme {
            "http" => {
                let tcp = match tcp {
                    Some(tcp) => tcp,
                    None => {
                        let stream = self
                            .connect_tcp(&host, uri.port_u16().unwrap_or(80), options)
                            .await?;
                        let (local_addr, peer_addr) = socket_addresses(&stream);
                        TcpConnected::new(stream).with_addresses(local_addr, peer_addr)
                    }
                };
                let TcpConnected {
                    io,
                    local_addr,
                    peer_addr,
                } = tcp;
                Ok(Connected::new(
                    FuturesIo::new(io),
                    if require_h2 {
                        super::ConnectionProtocol::Http2
                    } else {
                        super::ConnectionProtocol::Http1
                    },
                )
                .with_addresses(local_addr, peer_addr))
            }
            "https" => {
                self.connect_tls(
                    host,
                    uri.port_u16().unwrap_or(443),
                    require_h2,
                    tcp,
                    options,
                )
                .await
            }
            other => Err(Error::UnsupportedScheme(other.to_owned())),
        }
    }

    #[cfg(feature = "tls")]
    async fn connect_tls(
        &self,
        host: String,
        port: u16,
        require_h2: bool,
        custom_tcp: Option<TcpConnected>,
        options: RequestOptions,
    ) -> Result<Connected, Error> {
        let tcp = match custom_tcp {
            Some(tcp) => tcp,
            None => {
                let stream = self.connect_tcp(&host, port, options).await?;
                let (local_addr, peer_addr) = socket_addresses(&stream);
                TcpConnected::new(stream).with_addresses(local_addr, peer_addr)
            }
        };
        let TcpConnected {
            io,
            local_addr,
            peer_addr,
        } = tcp;
        let server_name = futures_rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| Error::InvalidServerName(host))?;
        let tls = self.tls.clone();
        let tls_connect = async {
            futures_rustls::TlsConnector::from(tls)
                .connect(server_name, io)
                .await
                .map_err(Error::Tls)
        };
        let tls = self
            .with_timeout(tls_connect, options.tls_timeout, Error::TlsTimeout)
            .await?;
        let protocol = match tls.get_ref().1.alpn_protocol() {
            Some(b"h2") => super::ConnectionProtocol::Http2,
            Some(b"http/1.1") | None if !require_h2 => super::ConnectionProtocol::Http1,
            None => return Err(Error::RequiredHttp2NotNegotiated),
            Some(other) => return Err(Error::UnexpectedAlpn(other.to_vec())),
        };
        Ok(Connected::new(FuturesIo::new(tls), protocol).with_addresses(local_addr, peer_addr))
    }

    #[cfg(not(feature = "tls"))]
    async fn connect_tls(
        &self,
        _: String,
        _: u16,
        _: bool,
        _: Option<TcpConnected>,
        _: RequestOptions,
    ) -> Result<Connected, Error> {
        Err(Error::TlsDisabled)
    }

    async fn connect_tcp(
        &self,
        host: &str,
        port: u16,
        options: RequestOptions,
    ) -> Result<Async<StdTcpStream>, Error> {
        let resolve = async {
            self.resolver
                .resolve(host.to_owned(), port)
                .await
                .map_err(Error::Resolve)
        };
        let addresses = self
            .with_timeout(resolve, options.dns_timeout, Error::DnsTimeout)
            .await?;
        let connect = async {
            connect_resolved_addresses(addresses, self.happy_eyeballs_timeout, self.timer.as_ref())
                .await
                .map_err(Error::Connect)
        };
        self.with_timeout(connect, options.connect_timeout, Error::ConnectTimeout)
            .await
    }

    async fn with_timeout<T, F>(
        &self,
        future: F,
        timeout: Option<Duration>,
        timeout_error: Error,
    ) -> Result<T, Error>
    where
        F: Future<Output = Result<T, Error>>,
    {
        match timeout {
            None => future.await,
            Some(timeout) => {
                match future::select(Box::pin(future), self.timer.sleep(timeout)).await {
                    Either::Left((result, _)) => result,
                    Either::Right(_) => Err(timeout_error),
                }
            }
        }
    }
}

/// Races resolved TCP candidates without committing the client to a runtime.
///
/// The first candidate starts immediately. When both address families are
/// present, later candidates are launched after `happy_eyeballs_timeout`, or
/// immediately after a failed attempt. Single-family results remain serial,
/// avoiding speculative duplicate connections with no alternate family to
/// prefer.
async fn connect_resolved_addresses(
    addresses: Vec<SocketAddr>,
    happy_eyeballs_timeout: Option<Duration>,
    timer: &(dyn Timer + Send + Sync),
) -> io::Result<Async<StdTcpStream>> {
    let addresses = interleave_address_families(addresses);
    let mut last_error = None;

    if happy_eyeballs_timeout.is_none() || !has_both_address_families(&addresses) {
        for address in addresses {
            match Async::<StdTcpStream>::connect(address).await {
                Ok(stream) => return Ok(stream),
                Err(error) => last_error = Some(error),
            }
        }
        return Err(no_connected_address(last_error));
    }

    let mut addresses = addresses.into_iter();
    let Some(first) = addresses.next() else {
        return Err(no_connected_address(None));
    };
    type Attempt = Pin<Box<dyn Future<Output = io::Result<Async<StdTcpStream>>> + Send>>;
    let mut attempts = FuturesUnordered::<Attempt>::new();
    attempts.push(Box::pin(Async::<StdTcpStream>::connect(first)));

    while let Some(next_address) = addresses.next() {
        let next_attempt = attempts.next();
        let sleep = timer.sleep(happy_eyeballs_timeout.expect("checked above"));
        match future::select(next_attempt, sleep).await {
            Either::Left((Some(Ok(stream)), _)) => return Ok(stream),
            Either::Left((Some(Err(error)), _)) => {
                last_error = Some(error);
                attempts.push(Box::pin(Async::<StdTcpStream>::connect(next_address)));
            }
            Either::Left((None, _)) => {
                attempts.push(Box::pin(Async::<StdTcpStream>::connect(next_address)));
            }
            Either::Right(((), _)) => {
                attempts.push(Box::pin(Async::<StdTcpStream>::connect(next_address)));
            }
        }
    }

    while let Some(result) = attempts.next().await {
        match result {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }

    Err(no_connected_address(last_error))
}

fn no_connected_address(last_error: Option<io::Error>) -> io::Error {
    last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not connect to any of the resolved addresses",
        )
    })
}

fn interleave_address_families(addresses: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let mut ipv6 = VecDeque::new();
    let mut ipv4 = VecDeque::new();
    for address in addresses {
        if address.is_ipv6() {
            ipv6.push_back(address);
        } else {
            ipv4.push_back(address);
        }
    }

    let mut interleaved = Vec::with_capacity(ipv6.len() + ipv4.len());
    while let Some(address) = ipv6.pop_front() {
        interleaved.push(address);
        if let Some(address) = ipv4.pop_front() {
            interleaved.push(address);
        }
    }
    interleaved.extend(ipv4);
    interleaved
}

fn has_both_address_families(addresses: &[SocketAddr]) -> bool {
    let has_ipv6 = addresses.iter().any(SocketAddr::is_ipv6);
    let has_ipv4 = addresses.iter().any(SocketAddr::is_ipv4);
    has_ipv6 && has_ipv4
}

fn socket_addresses(stream: &Async<StdTcpStream>) -> (Option<SocketAddr>, Option<SocketAddr>) {
    (
        stream.get_ref().local_addr().ok(),
        stream.get_ref().peer_addr().ok(),
    )
}

impl ConnectorBuilder {
    /// Replaces the default host resolver while retaining h12tiny's TCP,
    /// TLS, ALPN, protocol, and pooling policy.
    ///
    /// This setting is ignored by [`Self::dialer`] and [`Self::tcp_dialer`],
    /// which own their own name-resolution policy.
    pub fn resolver<R>(mut self, resolver: R) -> Self
    where
        R: Resolver,
    {
        self.connector.resolver = Arc::new(resolver);
        self
    }

    /// Sets the delay before beginning the next resolved TCP candidate.
    ///
    /// The default is 250 ms. Passing `None` disables concurrent address
    /// racing and attempts each resolved address serially. This setting
    /// applies only to the default TCP path; custom dialers own address
    /// selection themselves.
    pub fn happy_eyeballs_timeout(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.connector.happy_eyeballs_timeout = timeout.into();
        self
    }

    /// Replaces the default DNS/TCP/TLS establishment sequence.
    pub fn dialer<D>(mut self, dialer: D) -> Self
    where
        D: Dialer,
    {
        self.connector.kind = ConnectorKind::Custom(Arc::new(dialer));
        self
    }

    /// Replaces DNS/TCP establishment while retaining h12tiny's TLS,
    /// ALPN, protocol selection, and pooling.
    pub fn tcp_dialer<D>(mut self, dialer: D) -> Self
    where
        D: TcpDialer,
    {
        self.connector.kind = ConnectorKind::Tcp(Arc::new(dialer));
        self
    }

    /// Limits default resolution, TCP, and TLS establishment. Cancelling the
    /// default resolver wait stops waiting for `async-net`, but cannot
    /// interrupt a platform lookup that has already begun on `blocking`'s
    /// process-global executor. Use [`ConnectorBuilder::resolver`] when DNS itself
    /// needs a protocol-specific deadline or cancellation policy.
    /// This does not limit a request, response headers, or a response body.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connector.connect_timeout = Some(timeout);
        self
    }

    /// Replaces the timer used solely for [`ConnectorBuilder::connect_timeout`].
    /// This is mainly useful for deterministic runtimes and tests.
    pub fn timer<T>(mut self, timer: T) -> Self
    where
        T: Timer + Send + Sync + 'static,
    {
        self.connector.timer = Arc::new(timer);
        self
    }

    /// Uses an explicit Rustls client policy for the default dialer.
    #[cfg(feature = "tls")]
    pub fn tls_config(mut self, config: rustls::ClientConfig) -> Self {
        self.connector.tls = Arc::new(config);
        self
    }

    /// Uses h12tiny's standard TLS policy with an explicitly supplied Rustls
    /// provider. This does not install or consult Rustls' process-global
    /// provider selection.
    #[cfg(feature = "tls")]
    pub fn tls_provider(
        self,
        provider: Arc<rustls::crypto::CryptoProvider>,
    ) -> Result<Self, rustls::Error> {
        ClientTlsConfigBuilder::with_provider(provider)
            .build()
            .map(|config| self.tls_config(config))
    }

    /// Finalizes the connector policy.
    pub fn build(self) -> Connector {
        self.connector
    }
}

#[cfg(feature = "tls")]
fn default_tls_config() -> rustls::ClientConfig {
    ClientTlsConfigBuilder::new()
        .build()
        .expect("Graviola supports Rustls' safe default protocol versions")
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[cfg(feature = "tls")]
    use super::ClientTlsConfigBuilder;
    use super::{
        Connector, DialFuture, Dialer, Error, ResolveFuture, Resolver, TcpConnected, TcpDialFuture,
        TcpDialer,
    };
    use crate::RequestOptions;
    use http::Uri;

    #[derive(Clone, Default)]
    struct RecordingDialer(Arc<Mutex<Vec<(Uri, bool)>>>);

    impl Dialer for RecordingDialer {
        fn connect(&self, origin: Uri, require_http2: bool, _: RequestOptions) -> DialFuture {
            self.0.lock().unwrap().push((origin, require_http2));
            Box::pin(async { Err(Box::new(std::io::Error::other("fixture dial failure")) as _) })
        }
    }

    struct PendingDialer;

    impl Dialer for PendingDialer {
        fn connect(&self, _: Uri, _: bool, _: RequestOptions) -> DialFuture {
            Box::pin(std::future::pending())
        }
    }

    struct PendingTcpDialer;

    impl TcpDialer for PendingTcpDialer {
        fn connect(&self, _: Uri, _: RequestOptions) -> TcpDialFuture {
            Box::pin(std::future::pending())
        }
    }

    #[derive(Clone, Default)]
    struct DropProbeTcpDialer {
        dropped: Arc<AtomicUsize>,
    }

    struct PendingDialDropProbe(Arc<AtomicUsize>);

    impl Drop for PendingDialDropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl TcpDialer for DropProbeTcpDialer {
        fn connect(&self, _: Uri, _: RequestOptions) -> TcpDialFuture {
            let dropped = self.dropped.clone();
            Box::pin(async move {
                let _probe = PendingDialDropProbe(dropped);
                std::future::pending::<()>().await;
                unreachable!("pending TCP dial completed")
            })
        }
    }

    struct PendingResolver;

    impl Resolver for PendingResolver {
        fn resolve(&self, _: String, _: u16) -> ResolveFuture {
            Box::pin(std::future::pending())
        }
    }

    #[derive(Clone, Default)]
    struct RecordingTcpDialer(Arc<AtomicUsize>);

    impl TcpDialer for RecordingTcpDialer {
        fn connect(&self, _: Uri, _: RequestOptions) -> TcpDialFuture {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(Box::new(std::io::Error::other("unexpected dial")) as _) })
        }
    }

    #[derive(Clone, Default)]
    struct OptionsRecordingTcpDialer(Arc<Mutex<Vec<RequestOptions>>>);

    impl TcpDialer for OptionsRecordingTcpDialer {
        fn connect(&self, _: Uri, options: RequestOptions) -> TcpDialFuture {
            self.0.lock().unwrap().push(options);
            Box::pin(async { Err(Box::new(std::io::Error::other("fixture dial failure")) as _) })
        }
    }

    #[derive(Clone, Copy)]
    struct LocalTcpDialer;

    impl TcpDialer for LocalTcpDialer {
        fn connect(&self, origin: Uri, _: RequestOptions) -> TcpDialFuture {
            let host = origin.host().unwrap().to_owned();
            let port = origin.port_u16().unwrap();
            Box::pin(async move {
                let address = format!("{host}:{port}").parse::<std::net::SocketAddr>()?;
                async_io::Async::<std::net::TcpStream>::connect(address)
                    .await
                    .map(TcpConnected::new)
                    .map_err(|error| Box::new(error) as _)
            })
        }
    }

    #[derive(Clone)]
    struct RecordingResolver {
        addresses: Vec<SocketAddr>,
        requests: Arc<Mutex<Vec<(String, u16)>>>,
    }

    impl RecordingResolver {
        fn new(addresses: impl Into<Vec<SocketAddr>>) -> Self {
            Self {
                addresses: addresses.into(),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl Resolver for RecordingResolver {
        fn resolve(&self, host: String, port: u16) -> ResolveFuture {
            self.requests.lock().unwrap().push((host, port));
            let addresses = self.addresses.clone();
            Box::pin(async move { Ok(addresses) })
        }
    }

    #[test]
    fn custom_dialer_receives_the_normalized_origin_and_protocol_requirement() {
        let dialer = RecordingDialer::default();
        let connector = Connector::with_dialer(dialer.clone());
        let result =
            smol::block_on(connector.connect("http://example.test:8080/".parse().unwrap(), true));
        assert!(matches!(result, Err(Error::Custom(_))));
        assert_eq!(
            *dialer.0.lock().unwrap(),
            vec![("http://example.test:8080/".parse().unwrap(), true)]
        );
    }

    #[test]
    fn connect_timeout_cancels_a_pending_dialer() {
        let connector = Connector::builder()
            .dialer(PendingDialer)
            .connect_timeout(Duration::ZERO)
            .build();
        let result =
            smol::block_on(connector.connect("http://example.test/".parse().unwrap(), false));
        assert!(matches!(result, Err(Error::Timeout)));
    }

    #[test]
    fn connect_timeout_cancels_a_pending_tcp_dialer() {
        let connector = Connector::builder()
            .tcp_dialer(PendingTcpDialer)
            .connect_timeout(Duration::ZERO)
            .build();
        let result =
            smol::block_on(connector.connect("http://example.test/".parse().unwrap(), false));
        assert!(matches!(result, Err(Error::Timeout)));
    }

    #[test]
    fn connect_timeout_drops_a_pending_tcp_dialer_future() {
        let dialer = DropProbeTcpDialer::default();
        let connector = Connector::builder()
            .tcp_dialer(dialer.clone())
            .connect_timeout(Duration::from_millis(1))
            .build();
        let result =
            smol::block_on(connector.connect("http://example.test/".parse().unwrap(), false));

        assert!(matches!(result, Err(Error::Timeout)));
        assert_eq!(dialer.dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn tcp_dialer_is_not_called_for_an_unsupported_scheme() {
        let dialer = RecordingTcpDialer::default();
        let connector = Connector::with_tcp_dialer(dialer.clone());
        let result =
            smol::block_on(connector.connect("ftp://example.test/".parse().unwrap(), false));

        assert!(matches!(result, Err(Error::UnsupportedScheme(scheme)) if scheme == "ftp"));
        assert_eq!(dialer.0.load(Ordering::SeqCst), 0);
    }

    #[cfg(not(feature = "tls"))]
    #[test]
    fn tcp_dialer_is_not_called_for_https_without_tls_support() {
        let dialer = RecordingTcpDialer::default();
        let connector = Connector::with_tcp_dialer(dialer.clone());
        let result =
            smol::block_on(connector.connect("https://example.test/".parse().unwrap(), false));

        assert!(matches!(result, Err(Error::TlsDisabled)));
        assert_eq!(dialer.0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn custom_tcp_dialer_returns_a_futures_io_stream_to_h12tiny() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let connector = Connector::with_tcp_dialer(LocalTcpDialer);
        let connected =
            smol::block_on(connector.connect(format!("http://{address}/").parse().unwrap(), false))
                .unwrap();

        assert_eq!(connected.protocol, super::super::ConnectionProtocol::Http1);
    }

    #[test]
    fn default_tcp_connector_resolves_and_connects() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let connector = Connector::new();
        let connected = smol::block_on(
            connector.connect(format!("http://localhost:{port}/").parse().unwrap(), false),
        )
        .unwrap();

        assert_eq!(connected.protocol, super::super::ConnectionProtocol::Http1);
    }

    #[test]
    fn custom_resolver_receives_the_origin_host_and_effective_port() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let resolver = RecordingResolver::new([address]);
        let connector = Connector::with_resolver(resolver.clone());
        let connected =
            smol::block_on(connector.connect("http://example.test/".parse().unwrap(), false))
                .unwrap();

        assert_eq!(connected.protocol, super::super::ConnectionProtocol::Http1);
        assert_eq!(
            *resolver.requests.lock().unwrap(),
            vec![("example.test".to_owned(), 80)]
        );
    }

    #[test]
    fn connect_timeout_cancels_a_pending_resolver() {
        let connector = Connector::builder()
            .resolver(PendingResolver)
            .connect_timeout(Duration::ZERO)
            .build();
        let result =
            smol::block_on(connector.connect("http://example.test/".parse().unwrap(), false));

        assert!(matches!(result, Err(Error::Timeout)));
    }

    #[test]
    fn request_dns_timeout_is_distinct_from_legacy_establishment_timeout() {
        let connector = Connector::builder().resolver(PendingResolver).build();
        let result = smol::block_on(connector.connect_with_options(
            "http://example.test/".parse().unwrap(),
            false,
            RequestOptions::new().with_dns_timeout(Duration::ZERO),
        ));

        assert!(matches!(result, Err(Error::DnsTimeout)));
    }

    #[test]
    fn request_options_reach_a_custom_tcp_dialer() {
        let dialer = OptionsRecordingTcpDialer::default();
        let options = RequestOptions::new()
            .with_dns_timeout(Duration::from_millis(1))
            .with_connect_timeout(Duration::from_millis(2))
            .with_tls_timeout(Duration::from_millis(3))
            .with_headers_timeout(Duration::from_millis(4));
        let connector = Connector::builder().tcp_dialer(dialer.clone()).build();
        let result = smol::block_on(connector.connect_with_options(
            "http://example.test/".parse().unwrap(),
            false,
            options,
        ));

        assert!(matches!(result, Err(Error::Custom(_))));
        assert_eq!(*dialer.0.lock().unwrap(), vec![options]);
    }

    #[cfg(feature = "tls")]
    #[test]
    fn request_tls_timeout_starts_after_tcp_establishment() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let peer = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(20));
            drop(socket);
        });
        let connector = Connector::builder().tcp_dialer(LocalTcpDialer).build();
        let result = smol::block_on(connector.connect_with_options(
            format!("https://{address}/").parse().unwrap(),
            false,
            RequestOptions::new().with_tls_timeout(Duration::ZERO),
        ));

        assert!(
            matches!(result.as_ref().err(), Some(Error::TlsTimeout)),
            "{:?}",
            result.err()
        );
        peer.join().unwrap();
    }

    #[test]
    fn tcp_dialer_bypasses_a_custom_resolver() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let resolver = RecordingResolver::new(Vec::<SocketAddr>::new());
        let connector = Connector::builder()
            .resolver(resolver.clone())
            .tcp_dialer(LocalTcpDialer)
            .build();
        let connected =
            smol::block_on(connector.connect(format!("http://{address}/").parse().unwrap(), false))
                .unwrap();

        assert_eq!(connected.protocol, super::super::ConnectionProtocol::Http1);
        assert!(resolver.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn resolved_address_order_alternates_address_families() {
        let v4_one: SocketAddr = "127.0.0.1:80".parse().unwrap();
        let v4_two: SocketAddr = "127.0.0.2:80".parse().unwrap();
        let v6_one: SocketAddr = "[::1]:80".parse().unwrap();
        let v6_two: SocketAddr = "[::2]:80".parse().unwrap();

        assert_eq!(
            super::interleave_address_families(vec![v6_one, v6_two, v4_one, v4_two]),
            vec![v6_one, v4_one, v6_two, v4_two]
        );
        assert_eq!(
            super::interleave_address_families(vec![v4_one, v4_two, v6_one, v6_two]),
            vec![v6_one, v4_one, v6_two, v4_two]
        );
    }

    #[test]
    fn happy_eyeballs_races_only_mixed_address_families() {
        let v4_one: SocketAddr = "127.0.0.1:80".parse().unwrap();
        let v4_two: SocketAddr = "127.0.0.2:80".parse().unwrap();
        let v6_one: SocketAddr = "[::1]:80".parse().unwrap();

        assert!(super::has_both_address_families(&[v6_one, v4_one]));
        assert!(!super::has_both_address_families(&[v4_one, v4_two]));
        assert!(!super::has_both_address_families(&[v6_one]));
    }

    #[cfg(feature = "tls")]
    #[test]
    fn forcing_http1_replaces_even_a_custom_h2_alpn_policy() {
        let config = ClientTlsConfigBuilder::new().build().unwrap();
        let mut connector = Connector::with_tls_config(config);
        connector.force_http1();
        assert_eq!(connector.tls.alpn_protocols, vec![b"http/1.1".to_vec()]);
    }

    #[cfg(feature = "tls")]
    #[test]
    fn default_config_offers_h2_then_http11() {
        assert_eq!(
            super::default_tls_config().alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }

    #[cfg(feature = "tls")]
    #[test]
    fn tls_builder_constructs_an_explicit_graviola_config() {
        let config = ClientTlsConfigBuilder::new()
            .alpn_protocols([b"http/1.1".to_vec()])
            .build()
            .unwrap();

        assert_eq!(config.alpn_protocols, vec![b"http/1.1".to_vec()]);
    }

    #[cfg(feature = "tls")]
    #[test]
    fn tls_builder_accepts_an_explicit_provider() {
        let provider = Arc::new(rustls_graviola::default_provider());
        let config = ClientTlsConfigBuilder::with_provider(provider)
            .build()
            .unwrap();

        assert_eq!(
            config.alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }

    #[cfg(feature = "tls")]
    #[test]
    fn tls_builder_accepts_explicit_protocol_versions() {
        let config = ClientTlsConfigBuilder::new()
            .protocol_versions([&rustls::version::TLS13])
            .build()
            .unwrap();

        assert_eq!(
            config.alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }
}
