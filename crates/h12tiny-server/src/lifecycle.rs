//! Small listener and connection-task lifecycle helpers.
//!
//! [`serve`] and [`serve_tls`] helpers own the accepted connection drivers they start through the
//! caller's [`h12tiny_core::runtime::BoxExecutor`].  A shutdown future stops
//! accepting, asks every active Hyper connection to drain, and is not
//! complete until those drivers have finished.  The helpers deliberately do
//! not own process signals, routing, or request middleware. [`serve`] is for
//! already-usable (normally cleartext) streams; [`serve_tls`] owns the TLS
//! handshake and ALPN dispatch for a TLS listener.

use futures_channel::oneshot;
use futures_io::{AsyncRead, AsyncWrite};
use futures_util::future::{self, Either};
use futures_util::stream::{FuturesUnordered, StreamExt};
use h12tiny_core::io::FuturesIo;
use h12tiny_core::runtime::{AsyncIoTimer, BoxExecutor};
use hyper::body::Body;
use hyper::rt::Timer as _;
use hyper::service::Service;
use std::collections::HashMap;
use std::error::Error as StdError;
use std::future::Future;
use std::io;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

const ACCEPT_RETRY_BACKOFF: Duration = Duration::from_millis(10);

/// The error delivered to the optional connection error hook.
pub type ConnectionError = Box<dyn StdError + Send + Sync>;

// Boxed future returned by `Listener::accept`; named to keep the trait signature
// readable.
type AcceptFuture<'a, L> = Pin<
    Box<dyn Future<Output = io::Result<(<L as Listener>::Stream, <L as Listener>::Address)>> + Send + 'a>,
>;

/// A listener that yields runtime-neutral asynchronous streams.
///
/// The associated address is retained at the transport boundary so custom
/// listeners can preserve peer information while adapting a transport. The
/// built-in implementations are
/// [`async_net::TcpListener`] and, on Unix, [`async_net::unix::UnixListener`].
pub trait Listener {
    /// Raw accepted stream type handed to `serve` or to `TlsAcceptor` by
    /// `serve_tls`.
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;
    /// Peer-address type reported by the underlying listener.
    type Address: Send + 'static;

    /// Accept one stream.
    fn accept(
        &self,
    ) -> AcceptFuture<'_, Self>;
}

impl Listener for async_net::TcpListener {
    type Stream = async_net::TcpStream;
    type Address = std::net::SocketAddr;

    fn accept(
        &self,
    ) -> AcceptFuture<'_, Self> {
        Box::pin(async move { async_net::TcpListener::accept(self).await })
    }
}

#[cfg(unix)]
impl Listener for async_net::unix::UnixListener {
    type Stream = async_net::unix::UnixStream;
    type Address = async_net::unix::SocketAddr;

    fn accept(
        &self,
    ) -> AcceptFuture<'_, Self> {
        Box::pin(async move { async_net::unix::UnixListener::accept(self).await })
    }
}

/// Conservative listener limits applied by [`Serve::production`].
#[derive(Clone, Copy, Debug)]
pub struct ProductionBounds {
    /// Maximum active handshakes and connection drivers.
    pub max_connections: NonZeroUsize,
    /// Maximum time to receive complete HTTP/1 request headers.
    pub header_read_timeout: Option<Duration>,
    /// Maximum time to wait for active connections after shutdown.
    pub drain_timeout: Option<Duration>,
}

impl Default for ProductionBounds {
    fn default() -> Self {
        Self {
            max_connections: NonZeroUsize::new(1024).expect("production limit is nonzero"),
            header_read_timeout: Some(Duration::from_secs(10)),
            drain_timeout: Some(Duration::from_secs(30)),
        }
    }
}

impl ProductionBounds {
    fn validate(self) -> io::Result<Self> {
        if matches!(self.header_read_timeout, Some(timeout) if timeout.is_zero()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "header read timeout must be greater than zero",
            ));
        }
        Ok(self)
    }
}

/// Configured accept loop returned by [`serve`].
pub struct Serve<L, S> {
    listener: L,
    builder: crate::conn::auto::Builder<BoxExecutor>,
    service: S,
    executor: BoxExecutor,
    max_connections: Option<usize>,
    drain_timeout: Option<Duration>,
    on_connection_error: Option<Arc<dyn Fn(ConnectionError) + Send + Sync>>,
    on_connection_rejected: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Configured TLS accept loop returned by [`serve_tls`].
///
/// TLS handshakes are part of the tracked task, rather than being spawned as
/// a separate detached stage. This means shutdown cancels clients that have
/// connected but have not completed their handshake before resolving.
#[cfg(feature = "tls")]
pub struct TlsServe<L, S> {
    listener: L,
    acceptor: futures_rustls::TlsAcceptor,
    builder: crate::conn::auto::Builder<BoxExecutor>,
    service: S,
    executor: BoxExecutor,
    max_connections: Option<usize>,
    drain_timeout: Option<Duration>,
    on_connection_error: Option<Arc<dyn Fn(ConnectionError) + Send + Sync>>,
    on_connection_rejected: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Creates a compact async-net-compatible HTTP accept loop.
///
/// `executor` owns each connection driver.  Call [`Serve::shutdown_on`] with
/// an application-provided shutdown future to obtain the awaitable lifecycle
/// future:
///
/// ```ignore
/// serve(listener, service, executor)
///     .max_connections(256)
///     .shutdown_on(shutdown)
///     .await?;
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// This helper accepts cleartext streams. For a listener whose accepted
/// streams require a TLS handshake, use [`serve_tls`] so handshakes participate
/// in the same cancellation and drain lifecycle.
pub fn serve<L, S>(listener: L, service: S, executor: BoxExecutor) -> Serve<L, S>
where
    L: Listener,
    S: Service<http::Request<hyper::body::Incoming>>,
{
    Serve {
        listener,
        builder: crate::conn::auto::Builder::new(executor.clone()),
        service,
        executor,
        max_connections: None,
        drain_timeout: None,
        on_connection_error: None,
        on_connection_rejected: None,
    }
}

/// Creates a TLS accept loop with lifecycle-managed handshakes and
/// connections.
///
/// The listener yields raw streams. Each accepted stream is handed to the
/// supplied [`futures_rustls::TlsAcceptor`], and the negotiated ALPN protocol
/// selects HTTP/2 (`h2`) or HTTP/1.1 through
/// [`crate::conn::auto::Builder::serve_tls_connection`]. A missing ALPN
/// protocol follows the builder's HTTP/1.1 policy; an unsupported or unknown
/// protocol is reported through the connection-error hook.
#[cfg(feature = "tls")]
pub fn serve_tls<L, S>(
    listener: L,
    acceptor: futures_rustls::TlsAcceptor,
    service: S,
    executor: BoxExecutor,
) -> TlsServe<L, S>
where
    L: Listener,
    S: Service<http::Request<hyper::body::Incoming>>,
{
    TlsServe {
        listener,
        acceptor,
        builder: crate::conn::auto::Builder::new(executor.clone()),
        service,
        executor,
        max_connections: None,
        drain_timeout: None,
        on_connection_error: None,
        on_connection_rejected: None,
    }
}

impl<L, S> Serve<L, S> {
    /// Limits the number of active connection drivers.
    ///
    /// When the limit is reached, newly accepted streams are closed without
    /// starting a driver.  A limit of zero therefore rejects every accepted
    /// stream while keeping the listener alive.
    pub fn max_connections(mut self, limit: usize) -> Self {
        self.max_connections = Some(limit);
        self
    }

    /// Installs a hook for errors returned by connection drivers.
    ///
    /// The hook runs on the executor's connection task.  Accept errors are
    /// returned from the lifecycle future because they stop the accept loop.
    pub fn on_connection_error<H>(mut self, hook: H) -> Self
    where
        H: Fn(ConnectionError) + Send + Sync + 'static,
    {
        self.on_connection_error = Some(Arc::new(hook));
        self
    }

    /// Installs a hook for at-capacity rejection of an accepted stream.
    pub fn on_connection_rejected<H>(mut self, hook: H) -> Self
    where
        H: Fn() + Send + Sync + 'static,
    {
        self.on_connection_rejected = Some(Arc::new(hook));
        self
    }

    /// Applies the Internet-facing listener preset.
    ///
    /// Caps active connections at 1024 and closes a client that takes longer
    /// than 10 s to send complete HTTP/1 headers.
    pub fn production(self) -> Self {
        self.with_bounds(ProductionBounds::default())
            .expect("production bounds are valid")
    }

    /// Bounds how long shutdown waits for active connections.
    ///
    /// `None` waits indefinitely; a zero duration aborts immediately.
    pub fn drain_timeout(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.drain_timeout = timeout.into();
        self
    }

    /// Applies explicit bounds, including the timer needed by the header
    /// timeout.
    pub fn with_bounds(mut self, bounds: ProductionBounds) -> io::Result<Self> {
        let bounds = bounds.validate()?;
        self.max_connections = Some(bounds.max_connections.get());
        self.drain_timeout = bounds.drain_timeout;
        self.builder = configure_builder(self.builder, bounds);
        Ok(self)
    }
}

#[cfg(feature = "tls")]
impl<L, S> TlsServe<L, S> {
    /// Limits the number of active handshakes and connection drivers.
    ///
    /// When the limit is reached, newly accepted raw streams are closed
    /// without beginning a handshake. A limit of zero therefore rejects
    /// every accepted stream while keeping the listener alive.
    pub fn max_connections(mut self, limit: usize) -> Self {
        self.max_connections = Some(limit);
        self
    }

    /// Installs a hook for TLS handshake, ALPN-selection, and connection
    /// errors. Listener accept errors are returned from the lifecycle future.
    pub fn on_connection_error<H>(mut self, hook: H) -> Self
    where
        H: Fn(ConnectionError) + Send + Sync + 'static,
    {
        self.on_connection_error = Some(Arc::new(hook));
        self
    }

    /// Installs a hook for at-capacity rejection of an accepted stream.
    pub fn on_connection_rejected<H>(mut self, hook: H) -> Self
    where
        H: Fn() + Send + Sync + 'static,
    {
        self.on_connection_rejected = Some(Arc::new(hook));
        self
    }

    /// Applies the Internet-facing listener preset.
    ///
    /// Caps active handshakes and connections at 1024 and closes a client that
    /// takes longer than 10 s to send complete HTTP/1 headers.
    pub fn production(self) -> Self {
        self.with_bounds(ProductionBounds::default())
            .expect("production bounds are valid")
    }

    /// Bounds how long shutdown waits for active connections.
    ///
    /// `None` waits indefinitely; a zero duration aborts immediately.
    pub fn drain_timeout(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.drain_timeout = timeout.into();
        self
    }

    /// Applies explicit bounds, including the timer needed by the header
    /// timeout.
    pub fn with_bounds(mut self, bounds: ProductionBounds) -> io::Result<Self> {
        let bounds = bounds.validate()?;
        self.max_connections = Some(bounds.max_connections.get());
        self.drain_timeout = bounds.drain_timeout;
        self.builder = configure_builder(self.builder, bounds);
        Ok(self)
    }
}

fn configure_builder(
    mut builder: crate::conn::auto::Builder<BoxExecutor>,
    bounds: ProductionBounds,
) -> crate::conn::auto::Builder<BoxExecutor> {
    #[cfg(feature = "http1")]
    {
        builder.http1().timer(AsyncIoTimer);
        builder.http1().header_read_timeout(bounds.header_read_timeout);
    }
    #[cfg(feature = "http2")]
    {
        builder.http2().timer(AsyncIoTimer);
    }
    let _ = bounds;
    builder
}

impl<L, S, B> Serve<L, S>
where
    L: Listener + Send + Sync + 'static,
    S: Service<http::Request<hyper::body::Incoming>, Response = http::Response<B>>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<ConnectionError> + Send + 'static,
    B: Body + Send + 'static,
    B::Data: Send + 'static,
    B::Error: Into<ConnectionError> + Send + 'static,
{
    /// Stops accepting when `shutdown` resolves, cancels incomplete HTTP/2
    /// setup, then gracefully drains active connection drivers before this
    /// future completes.
    pub fn shutdown_on<F>(self, shutdown: F) -> impl Future<Output = io::Result<()>> + Send
    where
        F: Future + Send + 'static,
    {
        run(self, shutdown)
    }
}

#[cfg(feature = "tls")]
impl<L, S, B> TlsServe<L, S>
where
    L: Listener + Send + Sync + 'static,
    S: Service<http::Request<hyper::body::Incoming>, Response = http::Response<B>>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<ConnectionError> + Send + 'static,
    B: Body + Send + 'static,
    B::Data: Send + 'static,
    B::Error: Into<ConnectionError> + Send + 'static,
{
    /// Stops accepting, cancels TLS handshakes and incomplete HTTP/2 setup,
    /// then gracefully drains every established connection before this
    /// future completes.
    pub fn shutdown_on<F>(self, shutdown: F) -> impl Future<Output = io::Result<()>> + Send
    where
        F: Future + Send + 'static,
    {
        run_tls(self, shutdown)
    }
}

// Completion futures register only their own waker. Polling the listener must
// not inspect every live connection: most keep-alive connections are idle.
struct Completed {
    id: u64,
    done: oneshot::Receiver<()>,
}

impl Future for Completed {
    type Output = u64;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        Pin::new(&mut this.done).poll(cx).map(|_| this.id)
    }
}

struct ActiveSet {
    // Keep shutdown ownership separate from completion notifications so a
    // completed connection can be removed in O(1) rather than scanning all
    // receivers on every accept-loop wake.
    shutdowns: HashMap<u64, oneshot::Sender<()>>,
    aborts: HashMap<u64, oneshot::Sender<()>>,
    completed: FuturesUnordered<Completed>,
    next_id: u64,
}

impl ActiveSet {
    fn new() -> Self {
        Self {
            shutdowns: HashMap::new(),
            aborts: HashMap::new(),
            completed: FuturesUnordered::new(),
            next_id: 0,
        }
    }

    fn len(&self) -> usize {
        self.completed.len()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.completed.is_empty()
    }

    fn insert(
        &mut self,
        shutdown: oneshot::Sender<()>,
        abort: oneshot::Sender<()>,
        done: oneshot::Receiver<()>,
    ) {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).expect("connection id overflow");
        self.shutdowns.insert(id, shutdown);
        self.aborts.insert(id, abort);
        self.completed.push(Completed { id, done });
    }

    fn poll_done(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        match self.completed.poll_next_unpin(cx) {
            Poll::Ready(Some(id)) => {
                self.shutdowns.remove(&id);
                self.aborts.remove(&id);
                Poll::Ready(())
            }
            Poll::Ready(None) | Poll::Pending => Poll::Pending,
        }
    }

    fn shutdown(&mut self) {
        for (_, sender) in self.shutdowns.drain() {
            let _ = sender.send(());
        }
    }

    fn force_shutdown(&mut self) {
        for (_, sender) in self.aborts.drain() {
            let _ = sender.send(());
        }
    }
}

enum Event<S, A> {
    Shutdown,
    Accepted(io::Result<(S, A)>),
    Done,
}

async fn drain_active(active: &mut ActiveSet, timeout: Option<Duration>) -> bool {
    active.shutdown();
    match timeout {
        None => {
            while active.completed.next().await.is_some() {}
            true
        }
        Some(timeout) => {
            let drained = async {
                while active.completed.next().await.is_some() {}
            };
            match future::select(Box::pin(drained), AsyncIoTimer.sleep(timeout)).await {
                Either::Left(((), _timer)) => true,
                Either::Right((_elapsed, drained)) => {
                    drop(drained);
                    active.force_shutdown();
                    while active.completed.next().await.is_some() {}
                    false
                }
            }
        }
    }
}

fn is_transient_accept_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::TimedOut
    )
}

async fn run<L, S, B, F>(server: Serve<L, S>, shutdown: F) -> io::Result<()>
where
    L: Listener + Send + Sync + 'static,
    S: Service<http::Request<hyper::body::Incoming>, Response = http::Response<B>>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<ConnectionError> + Send + 'static,
    B: Body + Send + 'static,
    B::Data: Send + 'static,
    B::Error: Into<ConnectionError> + Send + 'static,
    F: Future + Send + 'static,
{
    let mut shutdown = Box::pin(shutdown);
    let mut accept = Some(server.listener.accept());
    let mut active = ActiveSet::new();

    loop {
        let event = future::poll_fn(|cx| {
            if shutdown.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Event::Shutdown);
            }

            if active.poll_done(cx).is_ready() {
                return Poll::Ready(Event::Done);
            }

            if let Some(accept_future) = accept.as_mut() {
                match accept_future.as_mut().poll(cx) {
                    Poll::Ready(result) => {
                        return Poll::Ready(Event::Accepted(result));
                    }
                    Poll::Pending => {}
                }
            }

            Poll::Pending
        })
        .await;

        match event {
            Event::Shutdown => {
                let drained = drain_active(&mut active, server.drain_timeout).await;
                return if drained {
                    Ok(())
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "graceful drain deadline expired",
                    ))
                };
            }
            Event::Accepted(result) => {
                let (stream, _address) = match result {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        if is_transient_accept_error(&error) {
                            AsyncIoTimer.sleep(ACCEPT_RETRY_BACKOFF).await;
                            accept = Some(server.listener.accept());
                            continue;
                        }
                        let _ = drain_active(&mut active, server.drain_timeout).await;
                        return Err(error);
                    }
                };
                let at_capacity = server
                    .max_connections
                    .is_some_and(|limit| active.len() >= limit);
                if at_capacity {
                    if let Some(hook) = &server.on_connection_rejected {
                        hook();
                    }
                } else {
                    let (shutdown_tx, shutdown_rx) = oneshot::channel();
                    let (abort_tx, abort_rx) = oneshot::channel();
                    let (done_tx, done_rx) = oneshot::channel();
                    let builder = server.builder.clone();
                    let service = server.service.clone();
                    let hook = server.on_connection_error.clone();
                    server.executor.execute(async move {
                        let connection = Box::pin(
                            builder
                                .serve_connection(FuturesIo::new(stream), service)
                                .into_owned(),
                        );
                        let shutdown = Box::pin(shutdown_rx);
                        let abort = Box::pin(abort_rx);
                        let result = match future::select(connection, shutdown).await {
                            Either::Left((result, _shutdown)) => result,
                            Either::Right((_shutdown, mut connection)) => {
                                connection.as_mut().graceful_shutdown();
                                match future::select(connection, abort).await {
                                    Either::Left((result, _abort)) => result,
                                    Either::Right((_abort, _connection)) => {
                                        let _ = done_tx.send(());
                                        return;
                                    }
                                }
                            }
                        };
                        if let Err(error) = result {
                            if let Some(hook) = hook {
                                hook(error);
                            }
                        }
                        let _ = done_tx.send(());
                    });
                    active.insert(shutdown_tx, abort_tx, done_rx);
                }
                accept = Some(server.listener.accept());
            }
            Event::Done => {}
        }
    }
}

#[cfg(feature = "tls")]
async fn run_tls<L, S, B, F>(server: TlsServe<L, S>, shutdown: F) -> io::Result<()>
where
    L: Listener + Send + Sync + 'static,
    S: Service<http::Request<hyper::body::Incoming>, Response = http::Response<B>>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<ConnectionError> + Send + 'static,
    B: Body + Send + 'static,
    B::Data: Send + 'static,
    B::Error: Into<ConnectionError> + Send + 'static,
    F: Future + Send + 'static,
{
    let mut shutdown = Box::pin(shutdown);
    let mut accept = Some(server.listener.accept());
    let mut active = ActiveSet::new();

    loop {
        let event = future::poll_fn(|cx| {
            if shutdown.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Event::Shutdown);
            }

            if active.poll_done(cx).is_ready() {
                return Poll::Ready(Event::Done);
            }

            if let Some(accept_future) = accept.as_mut() {
                match accept_future.as_mut().poll(cx) {
                    Poll::Ready(result) => return Poll::Ready(Event::Accepted(result)),
                    Poll::Pending => {}
                }
            }

            Poll::Pending
        })
        .await;

        match event {
            Event::Shutdown => {
                let drained = drain_active(&mut active, server.drain_timeout).await;
                return if drained {
                    Ok(())
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "graceful drain deadline expired",
                    ))
                };
            }
            Event::Accepted(result) => {
                let (stream, _address) = match result {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        if is_transient_accept_error(&error) {
                            AsyncIoTimer.sleep(ACCEPT_RETRY_BACKOFF).await;
                            accept = Some(server.listener.accept());
                            continue;
                        }
                        let _ = drain_active(&mut active, server.drain_timeout).await;
                        return Err(error);
                    }
                };
                let at_capacity = server
                    .max_connections
                    .is_some_and(|limit| active.len() >= limit);
                if at_capacity {
                    if let Some(hook) = &server.on_connection_rejected {
                        hook();
                    }
                } else {
                    let (shutdown_tx, shutdown_rx) = oneshot::channel();
                    let (abort_tx, abort_rx) = oneshot::channel();
                    let (done_tx, done_rx) = oneshot::channel();
                    let acceptor = server.acceptor.clone();
                    let builder = server.builder.clone();
                    let service = server.service.clone();
                    let hook = server.on_connection_error.clone();
                    server.executor.execute(async move {
                        // Keep the handshake and connection in one tracked
                        // task. The receiver returned by the first select is
                        // retained for the established connection.
                        let handshake = Box::pin(acceptor.accept(stream));
                        let shutdown = Box::pin(shutdown_rx);
                        let (tls, shutdown) = match future::select(handshake, shutdown).await {
                            Either::Left((Ok(tls), shutdown)) => (tls, shutdown),
                            Either::Left((Err(error), _shutdown)) => {
                                if let Some(hook) = hook {
                                    hook(error.into());
                                }
                                let _ = done_tx.send(());
                                return;
                            }
                            Either::Right((_shutdown, _handshake)) => {
                                let _ = done_tx.send(());
                                return;
                            }
                        };

                        let connection = match builder.serve_tls_connection(tls, service) {
                            Ok(connection) => Box::pin(connection),
                            Err(error) => {
                                if let Some(hook) = hook {
                                    hook(error);
                                }
                                let _ = done_tx.send(());
                                return;
                            }
                        };
                        let abort = Box::pin(abort_rx);
                        let result = match future::select(connection, shutdown).await {
                            Either::Left((result, _shutdown)) => result,
                            Either::Right((_shutdown, mut connection)) => {
                                connection.as_mut().graceful_shutdown();
                                match future::select(connection, abort).await {
                                    Either::Left((result, _abort)) => result,
                                    Either::Right((_abort, _connection)) => {
                                        let _ = done_tx.send(());
                                        return;
                                    }
                                }
                            }
                        };
                        if let Err(error) = result {
                            if let Some(hook) = hook {
                                hook(error);
                            }
                        }
                        let _ = done_tx.send(());
                    });
                    active.insert(shutdown_tx, abort_tx, done_rx);
                }
                accept = Some(server.listener.accept());
            }
            Event::Done => {}
        }
    }
}

#[cfg(all(test, feature = "http1"))]
mod tests {
    use super::{
        ActiveSet, ProductionBounds, drain_active, is_transient_accept_error, serve,
    };
    use bytes::Bytes;
    use futures_channel::oneshot;
    use futures_lite::prelude::*;
    use h12tiny_core::runtime::{BoxExecutor, BoxSendFuture};
    use http::{Request, Response};
    use http_body::{Body, Frame, SizeHint};
    use hyper::body::Incoming;
    use hyper::service::Service;
    use std::future::Future;
    use std::io;
    use std::num::NonZeroUsize;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;

    #[derive(Debug)]
    struct EmptyBody;

    impl Body for EmptyBody {
        type Data = Bytes;
        type Error = io::Error;

        fn poll_frame(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            Poll::Ready(None)
        }

        fn is_end_stream(&self) -> bool {
            true
        }

        fn size_hint(&self) -> SizeHint {
            SizeHint::with_exact(0)
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct EmptyService;

    impl Service<Request<Incoming>> for EmptyService {
        type Response = Response<EmptyBody>;
        type Error = io::Error;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn call(&self, _request: Request<Incoming>) -> Self::Future {
            Box::pin(async { Ok(Response::new(EmptyBody)) })
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct ThreadExecutor;

    impl hyper::rt::Executor<BoxSendFuture> for ThreadExecutor {
        fn execute(&self, future: BoxSendFuture) {
            std::thread::spawn(move || futures_lite::future::block_on(future));
        }
    }

    #[test]
    fn transient_accept_errors_are_classified() {
        for kind in [
            io::ErrorKind::Interrupted,
            io::ErrorKind::WouldBlock,
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::TimedOut,
        ] {
            assert!(is_transient_accept_error(&io::Error::from(kind)));
        }
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::InvalidInput] {
            assert!(!is_transient_accept_error(&io::Error::from(kind)));
        }
    }

    #[test]
    fn production_bounds_are_conservative_and_reject_zero_header_timeout() {
        let bounds = ProductionBounds::default();
        assert_eq!(bounds.max_connections.get(), 1024);
        assert_eq!(bounds.header_read_timeout, Some(Duration::from_secs(10)));
        assert!(bounds.validate().is_ok());

        let invalid = ProductionBounds {
            max_connections: NonZeroUsize::new(1).expect("nonzero"),
            header_read_timeout: Some(Duration::ZERO),
            drain_timeout: None,
        };
        assert_eq!(
            invalid.validate().expect_err("zero timeout must fail").kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn completed_connections_are_removed_without_scanning_peers() {
        let mut active = ActiveSet::new();
        let mut senders = Vec::new();
        for _ in 0..128 {
            let (shutdown_tx, _shutdown_rx) = oneshot::channel();
            let (abort_tx, _abort_rx) = oneshot::channel();
            let (done_tx, done_rx) = oneshot::channel();
            active.insert(shutdown_tx, abort_tx, done_rx);
            senders.push(done_tx);
        }
        assert_eq!(active.len(), 128);
        // The completion queue must handle out-of-order connection teardown.
        for sender in senders.into_iter().rev() {
            sender.send(()).expect("completion receiver still live");
        }
        futures_lite::future::block_on(async {
            for remaining in (0..128).rev() {
                futures_util::future::poll_fn(|cx| active.poll_done(cx)).await;
                assert_eq!(active.len(), remaining);
            }
        });
        assert!(active.is_empty());
    }

    #[test]
    fn drain_signals_all_connections_and_waits_for_completion() {
        let mut active = ActiveSet::new();
        let mut workers = Vec::new();
        for _ in 0..8 {
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            let (abort_tx, _abort_rx) = oneshot::channel();
            let (done_tx, done_rx) = oneshot::channel();
            active.insert(shutdown_tx, abort_tx, done_rx);
            workers.push(std::thread::spawn(move || {
                futures_lite::future::block_on(shutdown_rx).expect("shutdown signaled");
                done_tx.send(()).expect("completion receiver still live");
            }));
        }
        assert!(futures_lite::future::block_on(drain_active(&mut active, None)));
        for worker in workers {
            worker.join().expect("connection task completed");
        }
        assert!(active.is_empty());
    }

    #[test]
    fn drain_timeout_forces_the_remaining_connections() {
        let mut active = ActiveSet::new();
        let (shutdown_tx, _shutdown_rx) = oneshot::channel();
        let (abort_tx, abort_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        active.insert(shutdown_tx, abort_tx, done_rx);
        let worker = std::thread::spawn(move || {
            futures_lite::future::block_on(abort_rx).expect("abort signaled");
            done_tx.send(()).expect("completion receiver still live");
        });

        assert!(!futures_lite::future::block_on(drain_active(
            &mut active,
            Some(Duration::from_millis(10))
        )));
        worker.join().expect("connection task completed");
        assert!(active.is_empty());
    }

    #[cfg(feature = "http1")]
    #[test]
    fn tcp_accept_loop_stops_and_drains_on_shutdown() {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (address, server) = futures_lite::future::block_on(async {
            let listener = async_net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind test listener");
            let address = listener.local_addr().expect("listener address");
            let server = std::thread::spawn(move || {
                let executor = BoxExecutor::new(ThreadExecutor);
                futures_lite::future::block_on(
                    serve(listener, EmptyService, executor).shutdown_on(async move {
                        let _ = shutdown_rx.await;
                    }),
                )
                .expect("accept loop completed");
            });
            (address, server)
        });

        futures_lite::future::block_on(async {
            let mut client = async_net::TcpStream::connect(address)
                .await
                .expect("connect test listener");
            client
                .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n")
                .await
                .expect("write request");
            let mut response = Vec::new();
            let mut buffer = [0u8; 128];
            while !response.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = client.read(&mut buffer).await.expect("read response");
                assert!(count > 0, "server closed before response headers");
                response.extend_from_slice(&buffer[..count]);
            }
            assert!(
                response.starts_with(b"HTTP/1.1 200"),
                "response: {response:?}"
            );
            shutdown_tx.send(()).expect("signal shutdown");
        });

        server.join().expect("join accept loop");
    }
}
