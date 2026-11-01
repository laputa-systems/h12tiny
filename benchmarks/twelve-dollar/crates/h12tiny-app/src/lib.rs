mod metrics;
mod router;

use std::error::Error;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use app_core::{Config, Store};
use h12tiny::runtime::{BoxExecutor, BoxSendFuture, FnExecutor};
use h12tiny::server::{Listener, ProductionBounds, serve};
use h12tiny::web::{HandlerFuture, Request, Router};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use crate::metrics::Metrics;
use crate::router::AppState;

const MAX_CONNECTIONS: usize = 30_000;
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(75);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_READER_THREADS: usize = 2;
const MIN_READER_THREADS: usize = 1;
const MAX_READER_THREADS: usize = 4;

pub fn run() -> Result<(), Box<dyn Error>> {
    let config = Config::from_env()?;
    let address = config.socket_addr();
    let reader_threads = reader_threads()?;
    let executor_threads = executor_threads();
    let metrics = Arc::new(Metrics::new());
    let store = Store::open(&config, reader_threads)?;
    let state = Arc::new(AppState {
        store,
        secret: config.jwt_secret,
    });
    let service = router::build(Arc::clone(&state)).service_with(dispatch(Arc::clone(&metrics)));

    let listener = smol::block_on(smol::net::TcpListener::bind(address))?;
    let local_address = listener.local_addr()?;

    let executor: Arc<smol::Executor<'static>> = Arc::new(smol::Executor::new());
    let box_executor = BoxExecutor::new(FnExecutor::new({
        let executor = Arc::clone(&executor);
        move |future: BoxSendFuture| {
            executor.spawn(future).detach();
        }
    }));

    let (stop, stopped) = smol::channel::unbounded::<()>();
    let mut workers = Vec::with_capacity(executor_threads);
    for index in 0..executor_threads {
        let executor = Arc::clone(&executor);
        let stopped = stopped.clone();
        workers.push(
            std::thread::Builder::new()
                .name(format!("h12tiny-executor-{index}"))
                .spawn(move || {
                    smol::block_on(executor.run(async move {
                        let _ = stopped.recv().await;
                    }));
                })?,
        );
    }

    let bounds = ProductionBounds {
        max_connections: NonZeroUsize::new(MAX_CONNECTIONS).expect("max connections is nonzero"),
        header_read_timeout: Some(HEADER_READ_TIMEOUT),
        drain_timeout: Some(DRAIN_TIMEOUT),
    };
    let rejected = Arc::clone(&metrics);
    let server = serve(
        CountingListener {
            inner: listener,
            metrics: Arc::clone(&metrics),
        },
        service,
        box_executor,
    )
    .with_bounds(bounds)?
    .on_connection_rejected(move || rejected.rejected());
    let shutdown = shutdown_signal()?;

    println!(
        "h12tiny-twelve listening on http://{local_address} reader_threads={reader_threads} executor_threads={executor_threads} max_connections={MAX_CONNECTIONS} header_read_timeout_s={} drain_timeout_s={}",
        HEADER_READ_TIMEOUT.as_secs(),
        DRAIN_TIMEOUT.as_secs(),
    );

    let result = smol::block_on(server.shutdown_on(shutdown));
    println!("h12tiny-twelve shutdown {}", metrics.summary());
    drop(stop);
    for worker in workers {
        let _ = worker.join();
    }
    result?;
    Ok(())
}

struct CountingListener {
    inner: smol::net::TcpListener,
    metrics: Arc<Metrics>,
}

impl Listener for CountingListener {
    type Stream = smol::net::TcpStream;
    type Address = SocketAddr;

    fn accept(
        &self,
    ) -> Pin<Box<dyn Future<Output = io::Result<(Self::Stream, Self::Address)>> + Send + '_>> {
        let metrics = Arc::clone(&self.metrics);
        Box::pin(async move {
            let accepted = self.inner.accept().await;
            if accepted.is_ok() {
                metrics.accepted();
            }
            accepted
        })
    }
}

fn dispatch(
    metrics: Arc<Metrics>,
) -> impl Fn(Request, Router<Arc<AppState>>) -> HandlerFuture + Clone + Send + Sync + 'static {
    move |request, router| {
        let metrics = Arc::clone(&metrics);
        Box::pin(async move {
            let response = router.call_boxed(request).await;
            metrics.response(response.status());
            response
        })
    }
}

fn shutdown_signal() -> io::Result<impl Future<Output = ()> + Send + 'static> {
    let mut signals = Signals::new([SIGTERM, SIGINT])?;
    let (sender, receiver) = smol::channel::bounded(1);
    std::thread::Builder::new()
        .name("h12tiny-signal".to_owned())
        .spawn(move || {
            if signals.forever().next().is_some() {
                let _ = sender.send_blocking(());
            }
        })?;
    Ok(async move {
        let _ = receiver.recv().await;
    })
}

fn reader_threads() -> Result<usize, Box<dyn Error>> {
    match std::env::var("TWELVE_DOLLAR_READERS") {
        Ok(raw) => {
            let readers: usize = raw.parse().map_err(|_| {
                format!(
                    "TWELVE_DOLLAR_READERS must be an integer between {MIN_READER_THREADS} and {MAX_READER_THREADS}, got {raw:?}"
                )
            })?;
            if !(MIN_READER_THREADS..=MAX_READER_THREADS).contains(&readers) {
                return Err(format!(
                    "TWELVE_DOLLAR_READERS must be between {MIN_READER_THREADS} and {MAX_READER_THREADS}, got {readers}"
                )
                .into());
            }
            Ok(readers)
        }
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_READER_THREADS),
        Err(error) => Err(error.into()),
    }
}

fn executor_threads() -> usize {
    std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(1)
}
