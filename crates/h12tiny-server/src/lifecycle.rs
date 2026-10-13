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
use h12tiny_core::io::FuturesIo;
use h12tiny_core::runtime::BoxExecutor;
use hyper::body::Body;
use hyper::service::Service;
use std::error::Error as StdError;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

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

/// Configured accept loop returned by [`serve`].
pub struct Serve<L, S> {
    listener: L,
    builder: crate::conn::auto::Builder<BoxExecutor>,
    service: S,
    executor: BoxExecutor,
    max_connections: Option<usize>,
    on_connection_error: Option<Arc<dyn Fn(ConnectionError) + Send + Sync>>,
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
    on_connection_error: Option<Arc<dyn Fn(ConnectionError) + Send + Sync>>,
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
        on_connection_error: None,
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
        on_connection_error: None,
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

struct Active {
    shutdown: Option<oneshot::Sender<()>>,
    done: oneshot::Receiver<()>,
}

enum Event<S, A> {
    Shutdown,
    Accepted(io::Result<(S, A)>),
    Done(usize),
}

async fn drain_active(active: &mut Vec<Active>) {
    for connection in active.iter_mut() {
        if let Some(shutdown) = connection.shutdown.take() {
            let _ = shutdown.send(());
        }
    }

    while !active.is_empty() {
        let index = future::poll_fn(|cx| {
            for (index, connection) in active.iter_mut().enumerate() {
                if Pin::new(&mut connection.done).poll(cx).is_ready() {
                    return Poll::Ready(index);
                }
            }
            Poll::Pending
        })
        .await;
        active.swap_remove(index);
    }
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
    let mut active: Vec<Active> = Vec::new();
    let mut accepting = true;

    while accepting || !active.is_empty() {
        let event = future::poll_fn(|cx| {
            if accepting && shutdown.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Event::Shutdown);
            }

            for (index, connection) in active.iter_mut().enumerate() {
                match Pin::new(&mut connection.done).poll(cx) {
                    Poll::Ready(_) => return Poll::Ready(Event::Done(index)),
                    Poll::Pending => {}
                }
            }

            if accepting {
                if let Some(accept_future) = accept.as_mut() {
                    match accept_future.as_mut().poll(cx) {
                        Poll::Ready(result) => {
                            return Poll::Ready(Event::Accepted(result));
                        }
                        Poll::Pending => {}
                    }
                }
            }

            Poll::Pending
        })
        .await;

        match event {
            Event::Shutdown => {
                accepting = false;
                for connection in &mut active {
                    if let Some(shutdown) = connection.shutdown.take() {
                        let _ = shutdown.send(());
                    }
                }
            }
            Event::Accepted(result) => {
                let (stream, _address) = match result {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        drain_active(&mut active).await;
                        return Err(error);
                    }
                };
                let at_capacity = server
                    .max_connections
                    .is_some_and(|limit| active.len() >= limit);
                if !at_capacity {
                    let (shutdown_tx, shutdown_rx) = oneshot::channel();
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
                        let result = match future::select(connection, shutdown).await {
                            Either::Left((result, _shutdown)) => result,
                            Either::Right((_shutdown, mut connection)) => {
                                connection.as_mut().graceful_shutdown();
                                connection.await
                            }
                        };
                        if let Err(error) = result {
                            if let Some(hook) = hook {
                                hook(error);
                            }
                        }
                        let _ = done_tx.send(());
                    });
                    active.push(Active {
                        shutdown: Some(shutdown_tx),
                        done: done_rx,
                    });
                }
                accept = Some(server.listener.accept());
            }
            Event::Done(index) => {
                active.swap_remove(index);
                if !accepting {
                    continue;
                }
            }
        }
    }

    Ok(())
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
    let mut active: Vec<Active> = Vec::new();
    let mut accepting = true;

    while accepting || !active.is_empty() {
        let event = future::poll_fn(|cx| {
            if accepting && shutdown.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Event::Shutdown);
            }

            for (index, connection) in active.iter_mut().enumerate() {
                if Pin::new(&mut connection.done).poll(cx).is_ready() {
                    return Poll::Ready(Event::Done(index));
                }
            }

            if accepting {
                if let Some(accept_future) = accept.as_mut() {
                    match accept_future.as_mut().poll(cx) {
                        Poll::Ready(result) => return Poll::Ready(Event::Accepted(result)),
                        Poll::Pending => {}
                    }
                }
            }

            Poll::Pending
        })
        .await;

        match event {
            Event::Shutdown => {
                accepting = false;
                for connection in &mut active {
                    if let Some(shutdown) = connection.shutdown.take() {
                        let _ = shutdown.send(());
                    }
                }
            }
            Event::Accepted(result) => {
                let (stream, _address) = match result {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        drain_active(&mut active).await;
                        return Err(error);
                    }
                };
                let at_capacity = server
                    .max_connections
                    .is_some_and(|limit| active.len() >= limit);
                if !at_capacity {
                    let (shutdown_tx, shutdown_rx) = oneshot::channel();
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
                        let result = match future::select(connection, shutdown).await {
                            Either::Left((result, _shutdown)) => result,
                            Either::Right((_shutdown, mut connection)) => {
                                connection.as_mut().graceful_shutdown();
                                connection.await
                            }
                        };
                        if let Err(error) = result {
                            if let Some(hook) = hook {
                                hook(error);
                            }
                        }
                        let _ = done_tx.send(());
                    });
                    active.push(Active {
                        shutdown: Some(shutdown_tx),
                        done: done_rx,
                    });
                }
                accept = Some(server.listener.accept());
            }
            Event::Done(index) => {
                active.swap_remove(index);
            }
        }
    }

    Ok(())
}

#[cfg(all(test, feature = "http1"))]
mod tests {
    use super::serve;
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
    use std::pin::Pin;
    use std::task::{Context, Poll};

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
