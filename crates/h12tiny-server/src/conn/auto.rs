//! Automatic HTTP/1 or HTTP/2 server connection selection.
//!
//! This is a deliberately small port of the protocol-selection portion of
//! `hyper-util 0.1.20`'s `server::conn::auto` implementation.  Hyper still
//! owns all HTTP framing and dispatch.  This module only performs progressive
//! cleartext preface detection, rewinds the bytes it consumed, and starts the
//! selected Hyper connection.
//!
//! Upstream provenance: <https://github.com/hyperium/hyper-util>, tag
//! `v0.1.20`, `src/server/conn/auto/mod.rs`.  HTTP/2 prior knowledge is
//! supported; HTTP/1.1 `h2c` upgrades remain intentionally out of scope.

use bytes::{Buf, Bytes};
use hyper::rt::{Read, ReadBuf, ReadBufCursor, Write};
use std::cmp;
use std::future::Future;
use std::io;
#[cfg(any(not(feature = "http1"), not(feature = "http2")))]
use std::marker::PhantomData;
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

type Error = Box<dyn std::error::Error + Send + Sync>;
type Result<T> = std::result::Result<T, Error>;

/// I/O accepted by the automatic connection driver.
///
/// Upgrade-enabled H1 drivers require a sendable stream because Hyper moves
/// the raw upgraded stream into the application-owned upgrade future.  The
/// non-upgrade build keeps the historical `Read + Write + Unpin` contract.
#[cfg(feature = "upgrade")]
pub trait ConnectionIo: Read + Write + Unpin + Send {}

#[cfg(feature = "upgrade")]
impl<T: Read + Write + Unpin + Send> ConnectionIo for T {}

#[cfg(not(feature = "upgrade"))]
pub trait ConnectionIo: Read + Write + Unpin {}

#[cfg(not(feature = "upgrade"))]
impl<T: Read + Write + Unpin> ConnectionIo for T {}

/// TLS transport accepted by [`Builder::serve_tls_connection`].
///
/// The upgrade build requires a sendable TLS stream for the same reason as
/// [`ConnectionIo`]; non-upgrade builds preserve the original async-I/O
/// bounds.
#[cfg(feature = "upgrade")]
pub trait TlsIo: futures_io::AsyncRead + futures_io::AsyncWrite + Unpin + Send {}

#[cfg(feature = "upgrade")]
impl<T: futures_io::AsyncRead + futures_io::AsyncWrite + Unpin + Send> TlsIo for T {}

#[cfg(not(feature = "upgrade"))]
pub trait TlsIo: futures_io::AsyncRead + futures_io::AsyncWrite + Unpin {}

#[cfg(not(feature = "upgrade"))]
impl<T: futures_io::AsyncRead + futures_io::AsyncWrite + Unpin> TlsIo for T {}

/// A read buffer that replays bytes consumed by protocol detection before
/// reading from the accepted connection.
///
/// The buffer is read-only state owned by this wrapper.  Writes always go to
/// the underlying transport, so protocol detection cannot reorder outbound
/// bytes or accidentally write into the replay buffer.
#[derive(Debug)]
struct Rewind<T> {
    pre: Option<Bytes>,
    inner: T,
}

impl<T> Rewind<T> {
    fn new_buffered(inner: T, pre: Bytes) -> Self {
        Self {
            pre: Some(pre),
            inner,
        }
    }
}

impl<T> Read for Rewind<T>
where
    T: Read + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        if let Some(mut prefix) = self.pre.take() {
            if !prefix.is_empty() {
                let copy_len = cmp::min(prefix.len(), buf.remaining());
                buf.put_slice(&prefix[..copy_len]);
                prefix.advance(copy_len);
                if !prefix.is_empty() {
                    self.pre = Some(prefix);
                }
                return Poll::Ready(Ok(()));
            }
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T> Write for Rewind<T>
where
    T: Write + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

fn read_version<I>(io: I) -> ReadVersion<I>
where
    I: Read + Unpin,
{
    ReadVersion {
        io: Some(io),
        buf: [std::mem::MaybeUninit::uninit(); 24],
        filled: 0,
        version: Version::H2,
        cancelled: false,
        // Keep the detector !Unpin.  This matches Hyper's async connection
        // futures and prevents moving the state while an IO poll is active.
        _pin: PhantomPinned,
    }
}

pin_project_lite::pin_project! {
    struct ReadVersion<I> {
        io: Option<I>,
        buf: [std::mem::MaybeUninit<u8>; 24],
        filled: usize,
        version: Version,
        cancelled: bool,
        #[pin]
        _pin: PhantomPinned,
    }
}

impl<I> ReadVersion<I> {
    fn cancel(self: Pin<&mut Self>) {
        *self.project().cancelled = true;
    }
}

impl<I> Future for ReadVersion<I>
where
    I: Read + Unpin,
{
    type Output = io::Result<(Version, Rewind<I>)>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        if *this.cancelled {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "HTTP protocol detection cancelled",
            )));
        }

        let mut buf = ReadBuf::uninit(&mut *this.buf);
        // SAFETY: `filled` only advances after Hyper's `Read` implementation
        // reports bytes written into the cursor, so those bytes are
        // initialized and remain within the 24-byte detector buffer.
        unsafe {
            buf.unfilled().advance(*this.filled);
        }

        // Start optimistically as H2, switching to H1 at the first mismatch.
        // In particular, an ordinary H1 request does not make us wait for all
        // 24 preface bytes when its request line diverges early.
        while buf.filled().len() < H2_PREFACE.len() {
            let before = buf.filled().len();
            ready!(Pin::new(this.io.as_mut().expect("detector IO present"))
                .poll_read(cx, buf.unfilled()))?;
            *this.filled = buf.filled().len();

            if buf.filled().len() == before
                || buf.filled()[before..] != H2_PREFACE[before..buf.filled().len()]
            {
                *this.version = Version::H1;
                break;
            }
        }

        let io = this.io.take().expect("detector IO present after read");
        let replay = Bytes::copy_from_slice(buf.filled());
        Poll::Ready(Ok((*this.version, Rewind::new_buffered(io, replay))))
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Version {
    H1,
    H2,
}

#[cfg(any(not(feature = "http1"), not(feature = "http2")))]
impl Version {
    fn unsupported(self) -> Error {
        let message = match self {
            Self::H1 => "HTTP/1 is not supported",
            Self::H2 => "HTTP/2 is not supported",
        };
        io::Error::new(io::ErrorKind::Unsupported, message).into()
    }
}

#[cfg(feature = "http2")]
/// Executor boundary used by the HTTP/2 Hyper server connection.
pub trait HttpServerConnExec<A, B: http_body::Body>:
    hyper::rt::bounds::Http2ServerConnExec<A, B>
{
}

#[cfg(feature = "http2")]
impl<A, B: http_body::Body, T: hyper::rt::bounds::Http2ServerConnExec<A, B>>
    HttpServerConnExec<A, B> for T
{
}

#[cfg(not(feature = "http2"))]
/// Executor boundary retained for H1-only builds, where Hyper has no H2
/// executor requirement.
pub trait HttpServerConnExec<A, B: http_body::Body> {}

#[cfg(not(feature = "http2"))]
impl<A, B: http_body::Body, T> HttpServerConnExec<A, B> for T {}

#[cfg(any(feature = "http1", feature = "http2"))]
#[derive(Clone, Debug)]
pub struct Builder<E> {
    #[cfg(feature = "http1")]
    http1: hyper::server::conn::http1::Builder,
    #[cfg(feature = "http2")]
    http2: hyper::server::conn::http2::Builder<E>,
    version: Option<Version>,
    #[cfg(not(feature = "http2"))]
    _executor: E,
}

#[cfg(any(feature = "http1", feature = "http2"))]
impl<E: Default> Default for Builder<E> {
    fn default() -> Self {
        Self::new(E::default())
    }
}

#[cfg(any(feature = "http1", feature = "http2"))]
impl<E> Builder<E> {
    /// Creates an automatic server connection builder.
    pub fn new(executor: E) -> Self {
        Self {
            #[cfg(feature = "http1")]
            http1: hyper::server::conn::http1::Builder::new(),
            #[cfg(feature = "http2")]
            http2: hyper::server::conn::http2::Builder::new(executor),
            version: None,
            #[cfg(not(feature = "http2"))]
            _executor: executor,
        }
    }

    /// Returns a mutable view of the HTTP/1 configuration.
    #[cfg(feature = "http1")]
    pub fn http1(&mut self) -> Http1Builder<'_, E> {
        Http1Builder { inner: self }
    }

    /// Returns a mutable view of the HTTP/2 configuration.
    #[cfg(feature = "http2")]
    pub fn http2(&mut self) -> Http2Builder<'_, E> {
        Http2Builder { inner: self }
    }

    /// Restricts this builder to HTTP/1.1 connections.
    #[cfg(feature = "http1")]
    pub fn http1_only(mut self) -> Self {
        assert!(self.version.is_none());
        self.version = Some(Version::H1);
        self
    }

    /// Restricts this builder to HTTP/2 prior-knowledge connections.
    #[cfg(feature = "http2")]
    pub fn http2_only(mut self) -> Self {
        assert!(self.version.is_none());
        self.version = Some(Version::H2);
        self
    }

    /// Returns whether HTTP/1.1 is available on this builder.
    pub fn is_http1_available(&self) -> bool {
        match self.version {
            #[cfg(feature = "http1")]
            Some(Version::H1) => true,
            #[cfg(feature = "http2")]
            Some(Version::H2) => false,
            #[cfg(feature = "http1")]
            None => true,
            #[cfg(all(not(feature = "http1"), feature = "http2"))]
            None => false,
            #[cfg(not(feature = "http2"))]
            Some(Version::H2) => false,
            #[cfg(not(feature = "http1"))]
            Some(Version::H1) => false,
        }
    }

    /// Returns whether HTTP/2 prior knowledge is available on this builder.
    pub fn is_http2_available(&self) -> bool {
        match self.version {
            #[cfg(feature = "http1")]
            Some(Version::H1) => false,
            #[cfg(feature = "http2")]
            Some(Version::H2) => true,
            #[cfg(all(feature = "http1", feature = "http2"))]
            None => true,
            #[cfg(all(not(feature = "http1"), feature = "http2"))]
            None => true,
            #[cfg(all(feature = "http1", not(feature = "http2")))]
            None => false,
            #[cfg(not(feature = "http1"))]
            Some(Version::H1) => false,
            #[cfg(not(feature = "http2"))]
            Some(Version::H2) => false,
        }
    }

    /// Binds one accepted IO stream to a Hyper service.
    ///
    /// With both protocol features enabled, cleartext traffic is detected by
    /// progressively matching the HTTP/2 prior-knowledge preface.  Bytes read
    /// during detection are replayed to the selected Hyper parser exactly as
    /// received.  No HTTP/1.1 `Upgrade: h2c` handling is performed.
    pub fn serve_connection<I, S, B>(&self, io: I, service: S) -> Connection<'_, I, S, E>
    where
        S: hyper::service::Service<
            http::Request<hyper::body::Incoming>,
            Response = http::Response<B>,
        >,
        S::Future: 'static,
        S::Error: Into<Error>,
        B: http_body::Body + 'static,
        B::Error: Into<Error>,
        I: ConnectionIo + 'static,
        E: HttpServerConnExec<S::Future, B>,
    {
        let state = match self.version {
            #[cfg(feature = "http1")]
            Some(Version::H1) => {
                let io = Rewind::new_buffered(io, Bytes::new());
                #[cfg(feature = "upgrade")]
                let conn = self.http1.serve_connection(io, service).with_upgrades();
                #[cfg(not(feature = "upgrade"))]
                let conn = self.http1.serve_connection(io, service);
                ConnState::H1 { conn }
            }
            #[cfg(feature = "http2")]
            Some(Version::H2) => {
                let io = Rewind::new_buffered(io, Bytes::new());
                ConnState::H2 {
                    conn: self.http2.serve_connection(io, service),
                }
            }
            #[cfg(not(feature = "http1"))]
            Some(Version::H1) => ConnState::Unsupported {
                error: Some(Version::H1.unsupported()),
            },
            #[cfg(not(feature = "http2"))]
            Some(Version::H2) => ConnState::Unsupported {
                error: Some(Version::H2.unsupported()),
            },
            None => ConnState::ReadVersion {
                read_version: read_version(io),
                builder: Cow::Borrowed(self),
                service: Some(service),
            },
        };

        Connection { state }
    }

    /// Binds an already-handshaken TLS stream using its negotiated ALPN
    /// protocol. No ALPN falls back to HTTP/1.1, while an unknown value is an
    /// endpoint-policy error rather than a best-effort protocol guess.
    ///
    /// The caller owns TLS acceptance (for example, by passing an
    /// `async_net::TcpStream` to `TlsAcceptor::accept`). This method applies
    /// [`h12tiny_core::io::FuturesIo`] after the handshake, preserving the low-level,
    /// runtime-neutral server surface while making ALPN dispatch explicit.
    #[cfg(feature = "tls")]
    pub fn serve_tls_connection<I, S, B>(
        &self,
        io: futures_rustls::server::TlsStream<I>,
        service: S,
    ) -> Result<
        Connection<
            'static,
            h12tiny_core::io::FuturesIo<futures_rustls::server::TlsStream<I>>,
            S,
            E,
        >,
    >
    where
        E: Clone + HttpServerConnExec<S::Future, B>,
        S: hyper::service::Service<
            http::Request<hyper::body::Incoming>,
            Response = http::Response<B>,
        >,
        S::Future: 'static,
        S::Error: Into<Error>,
        B: http_body::Body + 'static,
        B::Error: Into<Error>,
        I: TlsIo + 'static,
    {
        match io.get_ref().1.alpn_protocol() {
            Some(b"h2") => {
                #[cfg(feature = "http2")]
                {
                    let builder = self.clone().http2_only();
                    Ok(builder
                        .serve_connection(h12tiny_core::io::FuturesIo::new(io), service)
                        .into_owned())
                }
                #[cfg(not(feature = "http2"))]
                {
                    let _ = service;
                    return Err(Version::H2.unsupported());
                }
            }
            Some(b"http/1.1") | None => {
                #[cfg(feature = "http1")]
                {
                    let builder = self.clone().http1_only();
                    Ok(builder
                        .serve_connection(h12tiny_core::io::FuturesIo::new(io), service)
                        .into_owned())
                }
                #[cfg(not(feature = "http1"))]
                {
                    let _ = service;
                    return Err(Version::H1.unsupported());
                }
            }
            Some(other) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected TLS ALPN protocol {other:?}"),
            )
            .into()),
        }
    }
}

#[cfg(all(feature = "http1", not(feature = "upgrade")))]
type Http1Connection<I, S> = hyper::server::conn::http1::Connection<Rewind<I>, S>;

#[cfg(all(feature = "http1", feature = "upgrade"))]
type Http1Connection<I, S> = hyper::server::conn::http1::UpgradeableConnection<Rewind<I>, S>;

#[cfg(not(feature = "http1"))]
type Http1Connection<I, S> = (PhantomData<I>, PhantomData<S>);

#[cfg(feature = "http2")]
type Http2Connection<I, S, E> = hyper::server::conn::http2::Connection<Rewind<I>, S, E>;

#[cfg(not(feature = "http2"))]
type Http2Connection<I, S, E> = (PhantomData<I>, PhantomData<S>, PhantomData<E>);

#[cfg(any(feature = "http1", feature = "http2"))]
pin_project_lite::pin_project! {
    /// Future returned by [`Builder::serve_connection`].
    #[must_use = "futures do nothing unless polled"]
    pub struct Connection<'a, I, S, E>
    where
        S: hyper::service::HttpService<hyper::body::Incoming>,
    {
        #[pin]
        state: ConnState<'a, I, S, E>,
    }
}

#[cfg(any(feature = "http1", feature = "http2"))]
enum Cow<'a, T> {
    Borrowed(&'a T),
    Owned(T),
}

#[cfg(any(feature = "http1", feature = "http2"))]
impl<T> std::ops::Deref for Cow<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        match self {
            Self::Borrowed(value) => value,
            Self::Owned(value) => value,
        }
    }
}

#[cfg(any(feature = "http1", feature = "http2"))]
pin_project_lite::pin_project! {
    #[project = ConnStateProj]
    enum ConnState<'a, I, S, E>
    where
        S: hyper::service::HttpService<hyper::body::Incoming>,
    {
        ReadVersion {
            #[pin]
            read_version: ReadVersion<I>,
            builder: Cow<'a, Builder<E>>,
            service: Option<S>,
        },
        H1 {
            #[pin]
            conn: Http1Connection<I, S>,
        },
        H2 {
            #[pin]
            conn: Http2Connection<I, S, E>,
        },
        Unsupported {
            error: Option<Error>,
        },
    }
}

#[cfg(any(feature = "http1", feature = "http2"))]
impl<I, S, E, B> Connection<'_, I, S, E>
where
    S: hyper::service::HttpService<hyper::body::Incoming, ResBody = B>,
    S::Error: Into<Error>,
    I: ConnectionIo,
    B: http_body::Body + 'static,
    B::Error: Into<Error>,
    E: HttpServerConnExec<S::Future, B>,
{
    /// Requests the connection to stop gracefully.  The returned future must
    /// continue to be polled for the shutdown to complete.
    pub fn graceful_shutdown(self: Pin<&mut Self>) {
        match self.project().state.project() {
            ConnStateProj::ReadVersion { read_version, .. } => read_version.cancel(),
            #[cfg(feature = "http1")]
            ConnStateProj::H1 { conn } => conn.graceful_shutdown(),
            #[cfg(not(feature = "http1"))]
            ConnStateProj::H1 { .. } => {}
            #[cfg(feature = "http2")]
            ConnStateProj::H2 { conn } => conn.graceful_shutdown(),
            #[cfg(not(feature = "http2"))]
            ConnStateProj::H2 { .. } => {}
            ConnStateProj::Unsupported { .. } => {}
        }
    }

    /// Detaches the connection from its borrowed builder configuration.
    pub fn into_owned(self) -> Connection<'static, I, S, E>
    where
        Builder<E>: Clone,
    {
        Connection {
            state: match self.state {
                ConnState::ReadVersion {
                    read_version,
                    builder,
                    service,
                } => ConnState::ReadVersion {
                    read_version,
                    service,
                    builder: Cow::Owned(match builder {
                        Cow::Borrowed(builder) => builder.clone(),
                        Cow::Owned(builder) => builder,
                    }),
                },
                ConnState::H1 { conn } => ConnState::H1 { conn },
                ConnState::H2 { conn } => ConnState::H2 { conn },
                ConnState::Unsupported { error } => ConnState::Unsupported { error },
            },
        }
    }
}

#[cfg(any(feature = "http1", feature = "http2"))]
impl<I, S, E, B> Future for Connection<'_, I, S, E>
where
    S: hyper::service::Service<http::Request<hyper::body::Incoming>, Response = http::Response<B>>,
    S::Future: 'static,
    S::Error: Into<Error>,
    B: http_body::Body + 'static,
    B::Error: Into<Error>,
    I: ConnectionIo + 'static,
    E: HttpServerConnExec<S::Future, B>,
{
    type Output = Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            let mut this = self.as_mut().project();
            match this.state.as_mut().project() {
                ConnStateProj::ReadVersion {
                    read_version,
                    builder,
                    service,
                } => {
                    let (version, io) = ready!(read_version.poll(cx))?;
                    let service = service
                        .take()
                        .expect("service present until protocol choice");
                    match version {
                        #[cfg(feature = "http1")]
                        Version::H1 => {
                            #[cfg(feature = "upgrade")]
                            let conn = builder.http1.serve_connection(io, service).with_upgrades();
                            #[cfg(not(feature = "upgrade"))]
                            let conn = builder.http1.serve_connection(io, service);
                            this.state.set(ConnState::H1 { conn });
                        }
                        #[cfg(feature = "http2")]
                        Version::H2 => {
                            let conn = builder.http2.serve_connection(io, service);
                            this.state.set(ConnState::H2 { conn });
                        }
                        #[cfg(not(feature = "http1"))]
                        Version::H1 => this.state.set(ConnState::Unsupported {
                            error: Some(Version::H1.unsupported()),
                        }),
                        #[cfg(not(feature = "http2"))]
                        Version::H2 => this.state.set(ConnState::Unsupported {
                            error: Some(Version::H2.unsupported()),
                        }),
                    }
                }
                #[cfg(feature = "http1")]
                ConnStateProj::H1 { conn } => return conn.poll(cx).map_err(Into::into),
                #[cfg(not(feature = "http1"))]
                ConnStateProj::H1 { .. } => return Poll::Ready(Err(Version::H1.unsupported())),
                #[cfg(feature = "http2")]
                ConnStateProj::H2 { conn } => return conn.poll(cx).map_err(Into::into),
                #[cfg(not(feature = "http2"))]
                ConnStateProj::H2 { .. } => return Poll::Ready(Err(Version::H2.unsupported())),
                ConnStateProj::Unsupported { error } => {
                    return Poll::Ready(Err(error.take().expect("unsupported error present")))
                }
            }
        }
    }
}

#[cfg(feature = "http1")]
pub struct Http1Builder<'a, E> {
    inner: &'a mut Builder<E>,
}

#[cfg(feature = "http1")]
impl<E> Http1Builder<'_, E> {
    /// Enables or disables HTTP/1 keep-alive.
    pub fn keep_alive(&mut self, enabled: bool) -> &mut Self {
        self.inner.http1.keep_alive(enabled);
        self
    }

    /// Enables HTTP/1 half-close handling.
    pub fn half_close(&mut self, enabled: bool) -> &mut Self {
        self.inner.http1.half_close(enabled);
        self
    }

    /// Writes HTTP/1 response header names in title case when enabled.
    pub fn title_case_headers(&mut self, enabled: bool) -> &mut Self {
        self.inner.http1.title_case_headers(enabled);
        self
    }

    /// Preserves original HTTP/1 header casing when enabled.
    pub fn preserve_header_case(&mut self, enabled: bool) -> &mut Self {
        self.inner.http1.preserve_header_case(enabled);
        self
    }

    /// Sets the maximum HTTP/1 connection buffer size.
    pub fn max_buf_size(&mut self, max: usize) -> &mut Self {
        self.inner.http1.max_buf_size(max);
        self
    }

    /// Controls automatic `Date` response headers.
    pub fn auto_date_header(&mut self, enabled: bool) -> &mut Self {
        self.inner.http1.auto_date_header(enabled);
        self
    }

    /// Configures the Hyper timer used by HTTP/1 header timeouts.
    pub fn timer<M>(&mut self, timer: M) -> &mut Self
    where
        M: hyper::rt::Timer + Send + Sync + 'static,
    {
        self.inner.http1.timer(timer);
        self
    }
}

#[cfg(feature = "http2")]
pub struct Http2Builder<'a, E> {
    inner: &'a mut Builder<E>,
}

#[cfg(feature = "http2")]
impl<E> Http2Builder<'_, E> {
    /// Sets the initial HTTP/2 stream window size.
    pub fn initial_stream_window_size(&mut self, size: impl Into<Option<u32>>) -> &mut Self {
        self.inner.http2.initial_stream_window_size(size);
        self
    }

    /// Sets the initial HTTP/2 connection window size.
    pub fn initial_connection_window_size(&mut self, size: impl Into<Option<u32>>) -> &mut Self {
        self.inner.http2.initial_connection_window_size(size);
        self
    }

    /// Enables or disables adaptive HTTP/2 flow control.
    pub fn adaptive_window(&mut self, enabled: bool) -> &mut Self {
        self.inner.http2.adaptive_window(enabled);
        self
    }

    /// Sets the maximum number of concurrent HTTP/2 streams.
    pub fn max_concurrent_streams(&mut self, max: impl Into<Option<u32>>) -> &mut Self {
        self.inner.http2.max_concurrent_streams(max);
        self
    }

    /// Controls automatic `Date` response headers.
    pub fn auto_date_header(&mut self, enabled: bool) -> &mut Self {
        self.inner.http2.auto_date_header(enabled);
        self
    }

    /// Configures the Hyper timer used by the HTTP/2 connection.
    pub fn timer<M>(&mut self, timer: M) -> &mut Self
    where
        M: hyper::rt::Timer + Send + Sync + 'static,
    {
        self.inner.http2.timer(timer);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{read_version, Rewind, Version, H2_PREFACE};
    use bytes::Bytes;
    use hyper::rt::{Read, ReadBufCursor, Write};
    use std::collections::VecDeque;
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    #[derive(Debug)]
    struct Chunks {
        chunks: VecDeque<Vec<u8>>,
        writes: Vec<u8>,
    }

    impl Chunks {
        fn new(chunks: impl IntoIterator<Item = &'static [u8]>) -> Self {
            Self {
                chunks: chunks.into_iter().map(|chunk| chunk.to_vec()).collect(),
                writes: Vec::new(),
            }
        }
    }

    impl Read for Chunks {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            mut buf: ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            let Some(chunk) = self.chunks.pop_front() else {
                return Poll::Ready(Ok(()));
            };
            let len = chunk.len().min(buf.remaining());
            buf.put_slice(&chunk[..len]);
            if len < chunk.len() {
                self.chunks.push_front(chunk[len..].to_vec());
            }
            Poll::Ready(Ok(()))
        }
    }

    impl Write for Chunks {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.writes.extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn poll_once<F: std::future::Future>(future: &mut Pin<&mut F>) -> Poll<F::Output> {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        future.as_mut().poll(&mut cx)
    }

    #[test]
    fn diverging_prefix_is_selected_progressively_and_replayed() {
        let io = Chunks::new(vec![&b"GET "[..], &b"/ HTTP/1.1\r\n\r\n"[..]]);
        let mut detector = Box::pin(read_version(io));
        let (version, mut replay) = match poll_once(&mut detector.as_mut()) {
            Poll::Ready(Ok(result)) => result,
            other => panic!("detector did not complete: {other:?}"),
        };
        assert_eq!(version, Version::H1);

        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut replayed = Vec::new();
        for _ in 0..2 {
            let mut bytes = [std::mem::MaybeUninit::<u8>::uninit(); 24];
            let mut read = hyper::rt::ReadBuf::uninit(&mut bytes);
            assert!(matches!(
                Pin::new(&mut replay).poll_read(&mut cx, read.unfilled()),
                Poll::Ready(Ok(()))
            ));
            replayed.extend_from_slice(read.filled());
        }
        assert_eq!(replayed, b"GET / HTTP/1.1\r\n\r\n");
    }

    #[test]
    fn complete_preface_selects_h2_and_replays_every_byte() {
        let io = Chunks::new(vec![&H2_PREFACE[..8], &H2_PREFACE[8..]]);
        let mut detector = Box::pin(read_version(io));
        let (version, mut replay) = match poll_once(&mut detector.as_mut()) {
            Poll::Ready(Ok(result)) => result,
            other => panic!("detector did not complete: {other:?}"),
        };
        assert_eq!(version, Version::H2);

        let mut bytes = [std::mem::MaybeUninit::<u8>::uninit(); 24];
        let mut read = hyper::rt::ReadBuf::uninit(&mut bytes);
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert!(matches!(
            Pin::new(&mut replay).poll_read(&mut cx, read.unfilled()),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(read.filled(), H2_PREFACE);
    }

    #[test]
    fn rewind_preserves_partial_reads_and_delegates_writes() {
        let mut rewind = Rewind::new_buffered(
            Chunks::new(vec![&b"socket"[..]]),
            Bytes::from_static(b"prefix"),
        );
        let mut bytes = [std::mem::MaybeUninit::<u8>::uninit(); 3];
        let mut read = hyper::rt::ReadBuf::uninit(&mut bytes);
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert!(matches!(
            Pin::new(&mut rewind).poll_read(&mut cx, read.unfilled()),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(read.filled(), b"pre");

        let mut bytes = [std::mem::MaybeUninit::<u8>::uninit(); 3];
        let mut read = hyper::rt::ReadBuf::uninit(&mut bytes);
        assert!(matches!(
            Pin::new(&mut rewind).poll_read(&mut cx, read.unfilled()),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(read.filled(), b"fix");

        let mut pin = Pin::new(&mut rewind);
        assert!(matches!(
            pin.as_mut().poll_write(&mut cx, b"out"),
            Poll::Ready(Ok(3))
        ));
    }
}
