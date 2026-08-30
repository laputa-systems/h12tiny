#![cfg(any(feature = "http1", feature = "http2"))]

//! Direct-origin HTTP client with legacy Hyper-util pooling semantics.
//!
//! The public client accepts absolute `http` and `https` URIs. It owns origin
//! routing, TLS/ALPN, pooling, and request normalization; Hyper continues to
//! own all HTTP/1 and HTTP/2 protocol machinery.

mod connect;
mod normalize;
mod pool;

use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures_util::future::{self, Either};
use http::{Method, Request, Response, Version};
use hyper::body::{Body, Incoming};

use self::normalize::PoolKey;
use self::pool::{CheckoutError, Poolable, Protocol as PoolProtocol, Reservation};
use h12tiny_core::runtime::{AsyncIoTimer, BoxExecutor, BoxSendFuture};

#[cfg(feature = "tls")]
pub use connect::ClientTlsConfigBuilder;
pub use connect::{
    Connected, ConnectionIo, Connector, ConnectorBuilder, DialError, DialFuture, Dialer,
    ResolveFuture, Resolver, SystemResolver, TcpConnected, TcpConnectionIo, TcpDialFuture,
    TcpDialer,
};
/// Futures-I/O traits accepted by [`TcpDialer`] streams.
pub use futures_io::{AsyncRead, AsyncWrite};

/// Errors are deliberately classified by the endpoint layer rather than
/// exposing connector/protocol implementation types in the public contract.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    source: Option<Box<dyn StdError + Send + Sync>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    Canceled,
    UnsupportedScheme,
    /// The per-request DNS phase exceeded its deadline.
    DnsTimeout,
    /// The per-request TCP establishment phase exceeded its deadline.
    ConnectTimeout,
    /// The per-request TLS and ALPN phase exceeded its deadline.
    TlsTimeout,
    /// The per-request request-dispatch and response-header phase exceeded its deadline.
    HeadersTimeout,
    Connect,
    Tls,
    Alpn,
    Handshake,
    SendRequest,
    UnsupportedMethod,
    UnsupportedVersion,
    AbsoluteUriRequired,
    ProtocolUnavailable,
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

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.kind {
            ErrorKind::Canceled => "request was cancelled before it could be sent",
            ErrorKind::UnsupportedScheme => "request URI scheme is unsupported",
            ErrorKind::DnsTimeout => "DNS resolution timed out",
            ErrorKind::ConnectTimeout => "TCP connection establishment timed out",
            ErrorKind::TlsTimeout => "TLS negotiation timed out",
            ErrorKind::HeadersTimeout => "request dispatch or response headers timed out",
            ErrorKind::Connect => "connection establishment failed",
            ErrorKind::Tls => "TLS negotiation or certificate validation failed",
            ErrorKind::Alpn => "TLS ALPN did not select an allowed HTTP protocol",
            ErrorKind::Handshake => "HTTP connection handshake failed",
            ErrorKind::SendRequest => "request dispatch failed",
            ErrorKind::UnsupportedMethod => "request method is unsupported for this HTTP version",
            ErrorKind::UnsupportedVersion => "request HTTP version is unsupported",
            ErrorKind::AbsoluteUriRequired => "client requests require an absolute URI",
            ErrorKind::ProtocolUnavailable => "the selected protocol was not enabled in this build",
        })
    }
}

/// Optional deadlines for one request's transport phases.
///
/// These values are never stored in the client or its origin pool. A reused
/// connection therefore skips DNS, TCP, TLS, and ALPN phases, while every
/// request receives its own `headers_timeout` race. h12tiny deliberately has
/// no whole-request deadline: callers that stream a response body must keep
/// the body-idle contract at their own boundary.
///
/// DNS, TCP, and TLS limits are enforced by h12tiny's default direct-origin
/// connector. A custom [`Dialer`] owns opaque establishment; a [`TcpDialer`]
/// similarly owns its DNS/TCP internals. Both receive these options and must
/// enforce the phases they own, while h12tiny continues to enforce its own
/// TLS and ALPN phase after a custom TCP dialer returns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestOptions {
    dns_timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    tls_timeout: Option<Duration>,
    headers_timeout: Option<Duration>,
}

impl RequestOptions {
    /// Starts with no request-scoped phase deadlines.
    pub const fn new() -> Self {
        Self {
            dns_timeout: None,
            connect_timeout: None,
            tls_timeout: None,
            headers_timeout: None,
        }
    }

    /// Limits DNS resolution for the default direct-origin connector.
    pub fn with_dns_timeout(mut self, timeout: Duration) -> Self {
        self.dns_timeout = Some(timeout);
        self
    }

    /// Returns the DNS deadline selected for this request.
    pub const fn dns_timeout(self) -> Option<Duration> {
        self.dns_timeout
    }

    /// Limits aggregate TCP establishment across all resolved addresses.
    ///
    /// A custom [`Dialer`] or [`TcpDialer`] receives this value and enforces
    /// its own TCP policy without h12tiny conflating it with DNS time.
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Returns the aggregate TCP-establishment deadline selected for this request.
    pub const fn connect_timeout(self) -> Option<Duration> {
        self.connect_timeout
    }

    /// Limits Rustls negotiation and ALPN after TCP is established.
    pub fn with_tls_timeout(mut self, timeout: Duration) -> Self {
        self.tls_timeout = Some(timeout);
        self
    }

    /// Returns the TLS and ALPN deadline selected for this request.
    pub const fn tls_timeout(self) -> Option<Duration> {
        self.tls_timeout
    }

    /// Limits request dispatch through receipt of response headers.
    pub fn with_headers_timeout(mut self, timeout: Duration) -> Self {
        self.headers_timeout = Some(timeout);
        self
    }

    /// Returns the request-dispatch and response-header deadline selected for this request.
    pub const fn headers_timeout(self) -> Option<Duration> {
        self.headers_timeout
    }

    fn has_connection_timeout(self) -> bool {
        self.dns_timeout.is_some() || self.connect_timeout.is_some() || self.tls_timeout.is_some()
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

/// Protocol selected for a client connection event.
///
/// This is intentionally separate from Hyper's version type: it describes a
/// reusable endpoint session, not an individual request or response message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectionProtocol {
    Http1,
    Http2,
}

/// Immutable facts about the connection that produced a response.
///
/// Default TCP connections populate both socket addresses. Custom [`Dialer`]
/// implementations can attach addresses through [`Connected::with_addresses`]
/// when their transport has that information. `connect_duration` covers the
/// whole connector operation, including default DNS, TCP, and TLS work;
/// `handshake_duration` covers h12tiny's HTTP/1 or HTTP/2 session handshake.
/// Neither duration includes request dispatch, response-header latency, or
/// response-body consumption.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionInfo {
    protocol: ConnectionProtocol,
    local_addr: Option<SocketAddr>,
    peer_addr: Option<SocketAddr>,
    connect_duration: Option<Duration>,
    handshake_duration: Option<Duration>,
}

impl ConnectionInfo {
    pub(crate) const fn new(protocol: ConnectionProtocol) -> Self {
        Self {
            protocol,
            local_addr: None,
            peer_addr: None,
            connect_duration: None,
            handshake_duration: None,
        }
    }

    /// Returns the HTTP capability negotiated for this connection.
    pub const fn protocol(self) -> ConnectionProtocol {
        self.protocol
    }

    /// Returns the local socket address when the connector reported one.
    pub const fn local_addr(self) -> Option<SocketAddr> {
        self.local_addr
    }

    /// Returns the peer socket address when the connector reported one.
    pub const fn peer_addr(self) -> Option<SocketAddr> {
        self.peer_addr
    }

    /// Returns the complete connection-establishment duration when measured.
    pub const fn connect_duration(self) -> Option<Duration> {
        self.connect_duration
    }

    /// Returns the HTTP session-handshake duration when measured.
    pub const fn handshake_duration(self) -> Option<Duration> {
        self.handshake_duration
    }
}

/// Per-response transport information stored in a response extension.
///
/// Read it with [`ResponseInfo::from_response`] immediately or after body
/// consumption; response extensions outlive the streaming body. It is
/// intentionally a narrow transport fact, not an application metrics API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResponseInfo {
    connection: ConnectionInfo,
    reused: bool,
}

impl ResponseInfo {
    /// Returns h12tiny's transport information attached to `response`.
    pub fn from_response<B>(response: &Response<B>) -> Option<&Self> {
        response.extensions().get()
    }

    /// Returns the connection that produced this response.
    pub const fn connection(self) -> ConnectionInfo {
        self.connection
    }

    /// Reports whether this request checked out a previously pooled session.
    pub const fn reused(self) -> bool {
        self.reused
    }
}

/// Stable HTTP/2 client settings independent of Hyper's builder API.
///
/// Every setting is opt-in: [`Http2Settings::new`] leaves Hyper's defaults
/// untouched. Explicit flow-control window sizes disable Hyper's adaptive
/// window unless [`Http2Settings::adaptive_window`] is set afterwards; an
/// explicit adaptive setting wins over either window size.
#[cfg(feature = "http2")]
#[derive(Clone, Debug, Default)]
pub struct Http2Settings {
    initial_stream_window_size: Option<u32>,
    initial_connection_window_size: Option<u32>,
    adaptive_window: Option<bool>,
    max_frame_size: Option<u32>,
    max_header_list_size: Option<u32>,
    header_table_size: Option<u32>,
    keep_alive_interval: Option<Duration>,
    keep_alive_timeout: Option<Duration>,
    keep_alive_while_idle: Option<bool>,
}

#[cfg(feature = "http2")]
impl Http2Settings {
    /// Starts with no overrides, preserving Hyper's current defaults.
    pub const fn new() -> Self {
        Self {
            initial_stream_window_size: None,
            initial_connection_window_size: None,
            adaptive_window: None,
            max_frame_size: None,
            max_header_list_size: None,
            header_table_size: None,
            keep_alive_interval: None,
            keep_alive_timeout: None,
            keep_alive_while_idle: None,
        }
    }

    /// Sets the initial per-stream flow-control receive window.
    pub const fn initial_stream_window_size(mut self, size: u32) -> Self {
        self.initial_stream_window_size = Some(size);
        self
    }

    /// Sets the initial connection-wide flow-control receive window.
    pub const fn initial_connection_window_size(mut self, size: u32) -> Self {
        self.initial_connection_window_size = Some(size);
        self
    }

    /// Enables or disables Hyper's adaptive receive-window algorithm.
    pub const fn adaptive_window(mut self, enabled: bool) -> Self {
        self.adaptive_window = Some(enabled);
        self
    }

    /// Sets the largest HTTP/2 frame accepted from the peer.
    pub const fn max_frame_size(mut self, size: u32) -> Self {
        self.max_frame_size = Some(size);
        self
    }

    /// Sets the largest decoded HTTP/2 header list accepted from the peer.
    pub const fn max_header_list_size(mut self, size: u32) -> Self {
        self.max_header_list_size = Some(size);
        self
    }

    /// Advertises the maximum HPACK dynamic-table size accepted from the peer.
    pub const fn header_table_size(mut self, size: u32) -> Self {
        self.header_table_size = Some(size);
        self
    }

    /// Sends HTTP/2 keep-alive pings at this interval.
    pub const fn keep_alive_interval(mut self, interval: Duration) -> Self {
        self.keep_alive_interval = Some(interval);
        self
    }

    /// Closes a connection when a keep-alive ping is not acknowledged in time.
    ///
    /// This has no effect until [`Self::keep_alive_interval`] is configured.
    pub const fn keep_alive_timeout(mut self, timeout: Duration) -> Self {
        self.keep_alive_timeout = Some(timeout);
        self
    }

    /// Controls whether keep-alive pings continue with no active streams.
    ///
    /// This has no effect until [`Self::keep_alive_interval`] is configured.
    pub const fn keep_alive_while_idle(mut self, enabled: bool) -> Self {
        self.keep_alive_while_idle = Some(enabled);
        self
    }

    fn apply(&self, builder: &mut hyper::client::conn::http2::Builder<BoxExecutor>) {
        if let Some(size) = self.initial_stream_window_size {
            builder.initial_stream_window_size(size);
        }
        if let Some(size) = self.initial_connection_window_size {
            builder.initial_connection_window_size(size);
        }
        if let Some(size) = self.max_frame_size {
            builder.max_frame_size(size);
        }
        if let Some(size) = self.max_header_list_size {
            builder.max_header_list_size(size);
        }
        if let Some(size) = self.header_table_size {
            builder.header_table_size(size);
        }
        if let Some(interval) = self.keep_alive_interval {
            builder.keep_alive_interval(interval);
        }
        if let Some(timeout) = self.keep_alive_timeout {
            builder.keep_alive_timeout(timeout);
        }
        if let Some(while_idle) = self.keep_alive_while_idle {
            builder.keep_alive_while_idle(while_idle);
        }
        if let Some(adaptive) = self.adaptive_window {
            builder.adaptive_window(adaptive);
        }
    }
}

/// A discrete endpoint-lifecycle observation recorded by [`DebugEventLog`].
///
/// Events are best-effort development diagnostics. They do not establish a
/// request-success guarantee and are deliberately not a metrics framework.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DebugEvent {
    PoolCheckout {
        origin: String,
    },
    ConnectionEstablished {
        origin: String,
        protocol: ConnectionProtocol,
    },
    AlpnSelected {
        origin: String,
        protocol: ConnectionProtocol,
    },
    ConnectionPooled {
        origin: String,
    },
    PoolEvicted {
        origin: String,
    },
    ConnectionClosed {
        origin: String,
    },
    StaleRetry {
        origin: String,
    },
}

/// An opt-in, dependency-free sink for client connection lifecycle events.
///
/// Clone this before passing it to [`Builder::debug_event_log`], then call
/// [`DebugEventLog::drain`] from a test harness or debugging control path.
/// It is intentionally pull-based: event recording never runs application
/// callbacks while the connection pool mutex is held.
#[derive(Clone, Default)]
pub struct DebugEventLog(Arc<Mutex<Vec<DebugEvent>>>);

impl DebugEventLog {
    /// Returns and clears all observations recorded since the previous drain.
    pub fn drain(&self) -> Vec<DebugEvent> {
        std::mem::take(&mut *self.0.lock().expect("debug event log mutex poisoned"))
    }

    pub fn is_empty(&self) -> bool {
        self.0
            .lock()
            .expect("debug event log mutex poisoned")
            .is_empty()
    }

    pub(crate) fn record(&self, event: DebugEvent) {
        self.0
            .lock()
            .expect("debug event log mutex poisoned")
            .push(event);
    }
}

impl fmt::Debug for DebugEventLog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DebugEventLog")
            .finish_non_exhaustive()
    }
}

/// A cheaply cloneable client. Clones share one per-origin pool and one TLS
/// policy. H2 origin coalescing is intentionally not implemented.
pub struct Client<B> {
    config: ClientConfig,
    connector: Connector,
    executor: BoxExecutor,
    #[cfg(feature = "http1")]
    h1_builder: hyper::client::conn::http1::Builder,
    #[cfg(feature = "http2")]
    h2_builder: hyper::client::conn::http2::Builder<BoxExecutor>,
    debug_events: Option<DebugEventLog>,
    pool: pool::Pool<PoolClient<B>, PoolKey>,
}

#[derive(Clone, Copy)]
struct ClientConfig {
    retry_canceled_requests: bool,
    set_host: bool,
    protocol: PoolProtocol,
}

/// A future returned by [`Client::request`]. Dropping it is supported
/// cancellation: Hyper closes an affected H1 session or resets only the H2
/// stream, and the pool's drop guards clean up waiters/connecting markers.
#[must_use = "futures do nothing unless polled"]
pub struct ResponseFuture {
    inner: Pin<Box<dyn Future<Output = Result<Response<Incoming>, Error>> + Send>>,
}

impl Future for ResponseFuture {
    type Output = Result<Response<Incoming>, Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.inner.as_mut().poll(cx)
    }
}

impl ResponseFuture {
    fn error(error: Error) -> Self {
        Self {
            inner: Box::pin(async move { Err(error) }),
        }
    }
}

impl fmt::Debug for ResponseFuture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResponseFuture")
    }
}

impl Client<()> {
    pub fn builder<E>(executor: E) -> Builder
    where
        E: hyper::rt::Executor<BoxSendFuture> + Send + Sync + 'static,
    {
        Builder::new(executor)
    }
}

impl<B> Client<B>
where
    B: Body + Send + Unpin + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn StdError + Send + Sync>>,
{
    pub fn get(&self, uri: http::Uri) -> ResponseFuture
    where
        B: Default,
    {
        let mut request = Request::new(B::default());
        *request.uri_mut() = uri;
        self.request(request)
    }

    /// Starts a request without request-scoped phase deadlines.
    ///
    /// This retains any legacy connector-wide establishment timeout configured
    /// through [`ConnectorBuilder::connect_timeout`]. New callers that need
    /// distinct phase deadlines should use [`Self::request_with_options`].
    pub fn request(&self, request: Request<B>) -> ResponseFuture {
        self.request_with_options(request, RequestOptions::new())
    }

    /// Starts a request with deadlines that belong only to this request.
    ///
    /// The options are carried through pool acquisition: a checked-out pooled
    /// connection avoids connection-establishment phases, while a new socket
    /// receives the DNS/TCP/TLS limits before request dispatch races the
    /// header limit. Options never participate in pool identity, so a caller
    /// cannot accidentally fragment reusable connections by choosing a
    /// different deadline.
    pub fn request_with_options(
        &self,
        mut request: Request<B>,
        options: RequestOptions,
    ) -> ResponseFuture {
        let is_connect = request.method() == Method::CONNECT;
        match request.version() {
            Version::HTTP_11 | Version::HTTP_2 => {}
            Version::HTTP_10 if !is_connect => {}
            Version::HTTP_10 => {
                return ResponseFuture::error(Error::new(ErrorKind::UnsupportedMethod))
            }
            _ => return ResponseFuture::error(Error::new(ErrorKind::UnsupportedVersion)),
        }

        let key = match normalize::extract_pool_key(request.uri_mut(), is_connect) {
            Ok(key) => key,
            Err(error) => {
                debug_assert_eq!(error, normalize::Error::AbsoluteUriRequired);
                return ResponseFuture::error(Error::with_source(
                    ErrorKind::AbsoluteUriRequired,
                    error,
                ));
            }
        };
        let client = self.clone();
        ResponseFuture {
            inner: Box::pin(async move { client.send_request(request, key, options).await }),
        }
    }

    async fn send_request(
        self,
        mut request: Request<B>,
        key: PoolKey,
        options: RequestOptions,
    ) -> Result<Response<Incoming>, Error> {
        let absolute_uri = request.uri().clone();
        loop {
            match self.try_send_request(request, key.clone(), options).await {
                Ok(response) => return Ok(response),
                Err(TrySendError::Final(error)) => return Err(error),
                Err(TrySendError::Retryable {
                    request: returned_request,
                    error,
                    reused,
                }) => {
                    if !self.config.retry_canceled_requests || !reused {
                        return Err(error);
                    }
                    if let Some(events) = &self.debug_events {
                        events.record(DebugEvent::StaleRetry {
                            origin: normalize::pool_key_origin(&key),
                        });
                    }
                    request = returned_request;
                    *request.uri_mut() = absolute_uri.clone();
                }
            }
        }
    }

    async fn try_send_request(
        &self,
        mut request: Request<B>,
        key: PoolKey,
        options: RequestOptions,
    ) -> Result<Response<Incoming>, TrySendError<B>> {
        let mut pooled = self
            .connection_for(key, options)
            .await
            .map_err(TrySendError::Final)?;
        if pooled.is_http1() {
            if request.version() == Version::HTTP_2 {
                return Err(TrySendError::Final(Error::new(
                    ErrorKind::UnsupportedVersion,
                )));
            }
            normalize::normalize_h1_request(&mut request, self.config.set_host);
        }

        let response_info = ResponseInfo {
            connection: pooled.connection_info,
            reused: pooled.is_reused(),
        };
        let sent = match options.headers_timeout {
            Some(timeout) => {
                let send = Box::pin(pooled.try_send_request(request));
                match future::select(send, self.connector.sleep(timeout)).await {
                    Either::Left((result, _)) => result,
                    Either::Right(_) => {
                        return Err(TrySendError::Final(Error::new(ErrorKind::HeadersTimeout)))
                    }
                }
            }
            None => pooled.try_send_request(request).await,
        };
        let mut response = match sent {
            Ok(response) => response,
            Err(mut error) => {
                if let Some(request) = error.take_message() {
                    return Err(TrySendError::Retryable {
                        request,
                        reused: pooled.is_reused(),
                        error: Error::with_source(ErrorKind::Canceled, error.into_error()),
                    });
                }
                return Err(TrySendError::Final(Error::with_source(
                    ErrorKind::SendRequest,
                    error.into_error(),
                )));
            }
        };
        response.extensions_mut().insert(response_info);

        // Hyper's H1 sender becomes reusable only once its connection driver
        // says it is ready again. Retaining this `Pooled` value in a driver
        // task is the legacy lifecycle invariant; response-body completion by
        // itself is not a sufficient signal.
        if pooled.is_http2() || !pooled.is_pool_enabled() || pooled.is_ready() {
            drop(pooled);
        } else {
            let executor = self.executor.clone();
            executor.execute(async move {
                let _ = futures_util::future::poll_fn(|cx| pooled.poll_ready(cx)).await;
            });
        }
        Ok(response)
    }

    async fn connection_for(
        &self,
        key: PoolKey,
        options: RequestOptions,
    ) -> Result<pool::Pooled<PoolClient<B>, PoolKey>, Error> {
        loop {
            match self.one_connection_for(key.clone(), options).await {
                Ok(pooled) => return Ok(pooled),
                Err(ConnectionError::Final(error)) => return Err(error),
                Err(ConnectionError::Retry) if self.config.retry_canceled_requests => continue,
                Err(ConnectionError::Retry) => return Err(Error::new(ErrorKind::Canceled)),
            }
        }
    }

    async fn one_connection_for(
        &self,
        key: PoolKey,
        options: RequestOptions,
    ) -> Result<pool::Pooled<PoolClient<B>, PoolKey>, ConnectionError> {
        if !self.pool.is_enabled() {
            return self
                .connect_to(key, options)
                .await
                .map_err(ConnectionError::Final);
        }
        let checkout = self.pool.checkout(key.clone());
        let connect = Box::pin(self.connect_to(key, options));
        match future::select(checkout, connect).await {
            Either::Left((Ok(pooled), _)) => Ok(pooled),
            Either::Right((Ok(pooled), _)) => Ok(pooled),
            Either::Left((Err(error), connect)) if error.is_cancellation() => {
                connect.await.map_err(ConnectionError::Final)
            }
            Either::Right((Err(error), checkout)) if error.kind() == ErrorKind::Canceled => {
                match checkout.await {
                    Ok(pooled) => Ok(pooled),
                    Err(CheckoutError::ClosedValue | CheckoutError::NoLongerWanted) => {
                        Err(ConnectionError::Retry)
                    }
                    Err(error) => Err(ConnectionError::Final(Error::with_source(
                        ErrorKind::Connect,
                        error,
                    ))),
                }
            }
            Either::Left((Err(error), _)) => Err(ConnectionError::Final(Error::with_source(
                ErrorKind::Connect,
                error,
            ))),
            Either::Right((Err(error), _)) => Err(ConnectionError::Final(error)),
        }
    }

    async fn connect_to(
        &self,
        key: PoolKey,
        options: RequestOptions,
    ) -> Result<pool::Pooled<PoolClient<B>, PoolKey>, Error> {
        let origin = normalize::pool_key_origin(&key);
        let mut connecting = self
            .pool
            .connecting(&key, self.config.protocol)
            .ok_or_else(|| Error::new(ErrorKind::Canceled))?;
        let connect_started = Instant::now();
        let connected = match self
            .connector
            .connect_with_options(
                normalize::pool_key_uri(key.clone()),
                self.config.protocol == PoolProtocol::Http2,
                options,
            )
            .await
        {
            Ok(connected) => connected,
            Err(error) => {
                let kind = error.client_error_kind();
                return Err(Error::with_source(kind, error));
            }
        };
        let connection_info = ConnectionInfo {
            connect_duration: Some(connect_started.elapsed()),
            ..connected.info
        };

        if let Some(events) = &self.debug_events {
            let protocol = connected.protocol;
            events.record(DebugEvent::ConnectionEstablished {
                origin: origin.clone(),
                protocol,
            });
            if key.scheme() == &http::uri::Scheme::HTTPS {
                events.record(DebugEvent::AlpnSelected { origin, protocol });
            }
        }

        if connected.protocol == ConnectionProtocol::Http2
            && self.config.protocol != PoolProtocol::Http2
        {
            if self.config.protocol == PoolProtocol::Http1 {
                return Err(Error::new(ErrorKind::Alpn));
            }
            connecting = connecting
                .alpn_h2(&self.pool)
                .ok_or_else(|| Error::new(ErrorKind::Canceled))?;
        }

        let handshake_started = Instant::now();
        let mut connection = self.handshake(connected, connection_info).await?;
        connection.connection_info.handshake_duration = Some(handshake_started.elapsed());
        Ok(self.pool.pooled(connecting, connection))
    }

    async fn handshake(
        &self,
        connected: Connected,
        connection_info: ConnectionInfo,
    ) -> Result<PoolClient<B>, Error> {
        match connected.protocol {
            ConnectionProtocol::Http1 => {
                #[cfg(feature = "http1")]
                {
                    let (mut sender, driver) = self
                        .h1_builder
                        .handshake(connected.io)
                        .await
                        .map_err(|error| Error::with_source(ErrorKind::Handshake, error))?;
                    self.executor.execute(async move {
                        let _ = driver.await;
                    });
                    sender
                        .ready()
                        .await
                        .map_err(|error| Error::with_source(ErrorKind::Handshake, error))?;
                    return Ok(PoolClient {
                        sender: Sender::Http1(sender),
                        connection_info,
                    });
                }
                #[cfg(not(feature = "http1"))]
                {
                    let _ = connected;
                    Err(Error::new(ErrorKind::ProtocolUnavailable))
                }
            }
            ConnectionProtocol::Http2 => {
                #[cfg(feature = "http2")]
                {
                    let (mut sender, driver) = self
                        .h2_builder
                        .handshake(connected.io)
                        .await
                        .map_err(|error| Error::with_source(ErrorKind::Handshake, error))?;
                    self.executor.execute(async move {
                        let _ = driver.await;
                    });
                    sender
                        .ready()
                        .await
                        .map_err(|error| Error::with_source(ErrorKind::Handshake, error))?;
                    return Ok(PoolClient {
                        sender: Sender::Http2(sender),
                        connection_info,
                    });
                }
                #[cfg(not(feature = "http2"))]
                {
                    let _ = connected;
                    Err(Error::new(ErrorKind::ProtocolUnavailable))
                }
            }
        }
    }
}

impl<B> Clone for Client<B> {
    fn clone(&self) -> Self {
        Self {
            config: self.config,
            connector: self.connector.clone(),
            executor: self.executor.clone(),
            #[cfg(feature = "http1")]
            h1_builder: self.h1_builder.clone(),
            #[cfg(feature = "http2")]
            h2_builder: self.h2_builder.clone(),
            debug_events: self.debug_events.clone(),
            pool: self.pool.clone(),
        }
    }
}

impl<B> fmt::Debug for Client<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Client")
    }
}

enum TrySendError<B> {
    Retryable {
        request: Request<B>,
        error: Error,
        reused: bool,
    },
    Final(Error),
}

enum ConnectionError {
    Final(Error),
    Retry,
}

struct PoolClient<B> {
    sender: Sender<B>,
    connection_info: ConnectionInfo,
}

enum Sender<B> {
    #[cfg(feature = "http1")]
    Http1(hyper::client::conn::http1::SendRequest<B>),
    #[cfg(feature = "http2")]
    Http2(hyper::client::conn::http2::SendRequest<B>),
}

impl<B> PoolClient<B> {
    fn is_http2(&self) -> bool {
        match self.sender {
            #[cfg(feature = "http1")]
            Sender::Http1(_) => false,
            #[cfg(feature = "http2")]
            Sender::Http2(_) => true,
        }
    }

    fn is_http1(&self) -> bool {
        !self.is_http2()
    }

    fn is_ready(&self) -> bool {
        match &self.sender {
            #[cfg(feature = "http1")]
            Sender::Http1(sender) => sender.is_ready(),
            #[cfg(feature = "http2")]
            Sender::Http2(sender) => sender.is_ready(),
        }
    }

    fn poll_ready(
        &mut self,
        #[allow(unused_variables)] cx: &mut Context<'_>,
    ) -> Poll<Result<(), hyper::Error>> {
        match &mut self.sender {
            #[cfg(feature = "http1")]
            Sender::Http1(sender) => sender.poll_ready(cx),
            #[cfg(feature = "http2")]
            Sender::Http2(_) => Poll::Ready(Ok(())),
        }
    }
}

impl<B> PoolClient<B>
where
    B: Body + Send + 'static,
{
    async fn try_send_request(
        &mut self,
        request: Request<B>,
    ) -> Result<Response<Incoming>, hyper::client::conn::TrySendError<Request<B>>> {
        match &mut self.sender {
            #[cfg(feature = "http1")]
            Sender::Http1(sender) => sender.try_send_request(request).await,
            #[cfg(feature = "http2")]
            Sender::Http2(sender) => sender.try_send_request(request).await,
        }
    }
}

impl<B> Poolable for PoolClient<B>
where
    B: Send + 'static,
{
    fn is_open(&self) -> bool {
        self.is_ready()
    }

    fn reserve(self) -> Reservation<Self> {
        match self.sender {
            #[cfg(feature = "http1")]
            Sender::Http1(sender) => Reservation::Unique(Self {
                sender: Sender::Http1(sender),
                connection_info: self.connection_info,
            }),
            #[cfg(feature = "http2")]
            Sender::Http2(sender) => Reservation::Shared(
                Self {
                    sender: Sender::Http2(sender.clone()),
                    connection_info: self.connection_info,
                },
                Self {
                    sender: Sender::Http2(sender),
                    connection_info: self.connection_info,
                },
            ),
        }
    }

    fn can_share(&self) -> bool {
        self.is_http2()
    }
}

/// Configures a client without exposing Hyper's protocol builders as a second
/// public configuration surface.
pub struct Builder {
    config: ClientConfig,
    connector: Connector,
    executor: BoxExecutor,
    #[cfg(feature = "http1")]
    h1_builder: hyper::client::conn::http1::Builder,
    #[cfg(feature = "http2")]
    h2_builder: hyper::client::conn::http2::Builder<BoxExecutor>,
    pool_config: pool::Config,
    pool_timer: Option<Arc<dyn hyper::rt::Timer + Send + Sync>>,
    debug_events: Option<DebugEventLog>,
}

impl Builder {
    pub fn new<E>(executor: E) -> Self
    where
        E: hyper::rt::Executor<BoxSendFuture> + Send + Sync + 'static,
    {
        let executor = BoxExecutor::new(executor);
        #[cfg(feature = "http1")]
        let h1_builder = {
            let mut builder = hyper::client::conn::http1::Builder::new();
            // Keep direct-origin wire output readable and stable for the
            // `Host` normalization contract exercised by raw-wire tests.
            builder.title_case_headers(true);
            builder
        };
        Self {
            config: ClientConfig {
                retry_canceled_requests: true,
                set_host: true,
                protocol: PoolProtocol::Auto,
            },
            connector: Connector::new(),
            #[cfg(feature = "http1")]
            h1_builder,
            #[cfg(feature = "http2")]
            h2_builder: {
                let mut builder = hyper::client::conn::http2::Builder::new(executor.clone());
                // Hyper requires a timer before HTTP/2 keepalive can be
                // enabled. `AsyncIoTimer` keeps that support runtime-neutral
                // and matches the timer already used for idle-pool eviction.
                builder.timer(AsyncIoTimer);
                builder
            },
            executor,
            pool_config: pool::Config {
                idle_timeout: Some(Duration::from_secs(90)),
                max_idle_per_host: usize::MAX,
                max_h1_connections_per_host: usize::MAX,
            },
            pool_timer: Some(Arc::new(AsyncIoTimer)),
            debug_events: None,
        }
    }

    pub fn connector(&mut self, connector: Connector) -> &mut Self {
        self.connector = connector;
        if self.config.protocol == PoolProtocol::Http1 {
            self.connector.force_http1();
        }
        self
    }

    pub fn pool_idle_timeout(&mut self, timeout: impl Into<Option<Duration>>) -> &mut Self {
        self.pool_config.idle_timeout = timeout.into();
        self
    }

    pub fn pool_max_idle_per_host(&mut self, max_idle: usize) -> &mut Self {
        self.pool_config.max_idle_per_host = max_idle;
        self
    }

    /// Limits open HTTP/1.1 connections for each origin.
    ///
    /// The bound includes connecting, in-use, and idle HTTP/1.1 sockets, so
    /// callers above the bound wait for a reusable connection instead of
    /// creating another socket. HTTP/2 remains multiplexed and is unaffected.
    /// Passing zero disables this additional bound; the default is unlimited.
    /// The bound applies only while the idle pool is enabled.
    pub fn pool_max_connections_per_host(&mut self, max_connections: usize) -> &mut Self {
        self.pool_config.max_h1_connections_per_host = if max_connections == 0 {
            usize::MAX
        } else {
            max_connections
        };
        self
    }

    /// Replaces the timer used only for idle-pool eviction. Request deadlines
    /// remain caller-controlled future races; this builder intentionally does
    /// not define a misleading whole-request timeout.
    pub fn pool_timer<T>(&mut self, timer: T) -> &mut Self
    where
        T: hyper::rt::Timer + Send + Sync + 'static,
    {
        self.pool_timer = Some(Arc::new(timer));
        self
    }

    /// Records endpoint lifecycle transitions in `log` without adding a
    /// logging-framework dependency. The caller retains a clone and drains
    /// it when inspection is useful.
    pub fn debug_event_log(&mut self, log: DebugEventLog) -> &mut Self {
        self.debug_events = Some(log);
        self
    }

    pub fn set_host(&mut self, enabled: bool) -> &mut Self {
        self.config.set_host = enabled;
        self
    }

    pub fn retry_canceled_requests(&mut self, enabled: bool) -> &mut Self {
        self.config.retry_canceled_requests = enabled;
        self
    }

    /// Restricts this client to HTTP/1.1 connections and TLS ALPN.
    ///
    /// The policy retains the H1 per-origin pool. It also overrides a custom
    /// TLS configuration's ALPN offerings so an HTTPS peer cannot select H2.
    #[cfg(feature = "http1")]
    pub fn http1_only(&mut self) -> &mut Self {
        self.config.protocol = PoolProtocol::Http1;
        self.connector.force_http1();
        self
    }

    #[cfg(feature = "http2")]
    pub fn http2_only(&mut self, enabled: bool) -> &mut Self {
        self.config.protocol = if enabled {
            PoolProtocol::Http2
        } else {
            PoolProtocol::Auto
        };
        self
    }

    /// Applies stable HTTP/2 client settings without exposing Hyper's builder
    /// as part of h12tiny's public API.
    #[cfg(feature = "http2")]
    pub fn http2_settings(&mut self, settings: Http2Settings) -> &mut Self {
        settings.apply(&mut self.h2_builder);
        self
    }

    pub fn build<B>(self) -> Client<B> {
        let mut connector = self.connector;
        if self.config.protocol == PoolProtocol::Http1 {
            connector.force_http1();
        }
        let debug_events = self.debug_events;
        Client {
            config: self.config,
            connector,
            executor: self.executor.clone(),
            #[cfg(feature = "http1")]
            h1_builder: self.h1_builder,
            #[cfg(feature = "http2")]
            h2_builder: self.h2_builder,
            debug_events: debug_events.clone(),
            pool: pool::Pool::new(
                self.pool_config,
                self.executor,
                self.pool_timer,
                debug_events,
            ),
        }
    }
}
