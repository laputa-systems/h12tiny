use std::error::Error;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use h12tiny::runtime::{BoxExecutor, BoxSendFuture, FnExecutor};
use h12tiny::server::{Listener, ProductionBounds, serve};
use h12tiny::web::{
    Bytes, FromRequest, HandlerFuture, HeaderValue, IntoResponse, Path, Rejection, Request,
    RequestMeta, Response, Router, StatusCode, get, post,
};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

const MAX_CONNECTIONS: usize = 30_000;
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(75);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const BODY_LIMIT: usize = 64 * 1024;

const HEALTH_BODY: &[u8] = br#"{"status":"ok","db":"ok","uptime_s":0}"#;
const NOT_FOUND_BODY: &[u8] = br#"{"error":"not found"}"#;
const POST_NOT_FOUND_BODY: &[u8] = br#"{"error":"post not found"}"#;
const INVALID_ID_BODY: &[u8] = br#"{"error":"invalid post id"}"#;
const MISSING_TOKEN_BODY: &[u8] = br#"{"error":"missing bearer token"}"#;
const CREATE_BODY: &[u8] = br#"{"post":{"id":500001,"body":"swift_ember_22228 says hi at 2026-11-01T12:00:00.000Z (VU 12000, iter 42)","created_at":"2026-11-01T12:00:00.000Z","author":"swift_ember_22228","like_count":0}}"#;

const FEED_BODY: &[u8] = br#"{"posts":[{"id":500000,"body":"TIL So keepalive the an watching lucky measuring usually everywhere droplet waiting... Nowhere building test those here keepalive network someone and of load? Server big learning ubuntu deploy careful coffee.","created_at":"2025-12-31T23:59:19.409Z","author":"swift_ember_22228","like_count":5},{"id":499999,"body":"Something latency boring after users pool lucky linux usually. Cheap because lucky never of index feature cpu those someone! Really writing waiting rewrite fast just while broken surprising weird request...","created_at":"2025-12-31T23:58:21.902Z","author":"quiet_thistle_7672","like_count":18},{"id":499998,"body":"Index surprising cheap latency watching!","created_at":"2025-12-31T23:56:51.393Z","author":"sharp_nebula_115","like_count":6},{"id":499997,"body":"Honestly, Node merge bug today throughput tomorrow again ready latency shipping tonight really droplet this. Of shipping watching this users done release bug probably while once users!","created_at":"2025-12-31T23:56:31.189Z","author":"rusty_glacier_23474","like_count":8},{"id":499996,"body":"Question: This finally sometimes refactor and yesterday cheap ready branch anything debugging surprising... Postgres kernel steal request keepalive ready benchmark careful of tomorrow bug request! Express building load after express node because here.","created_at":"2025-12-31T23:55:08.752Z","author":"cosmic_nebula_25516","like_count":6},{"id":499995,"body":"TIL These droplet project benchmark sort nowhere ubuntu?","created_at":"2025-12-31T23:54:00.056Z","author":"golden_falcon_8818","like_count":2},{"id":499994,"body":"Socket server debugging merge someone. Memory fast cpu there there keepalive server again before fast something after feature... Really honest something of sort ubuntu socket building never benchmark surprising request network these?","created_at":"2025-12-31T23:53:00.608Z","author":"rusty_quartz_402","like_count":0},{"id":499993,"body":"Postgres tired tired someone express project because nowhere cpu today of nginx of today.","created_at":"2025-12-31T23:52:19.685Z","author":"golden_sparrow_1928","like_count":5},{"id":499992,"body":"Linux today network pool review running. Sometimes project ready deploy usually writing ready there anything...","created_at":"2025-12-31T23:51:13.831Z","author":"curious_falcon_17531","like_count":1},{"id":499991,"body":"Query merge broken tonight maybe because just keepalive nginx index these typescript weird morning.","created_at":"2025-12-31T23:49:33.087Z","author":"grumpy_meadow_21370","like_count":9},{"id":499990,"body":"So because nowhere always cpu here weird of systemd. Kernel server ready honest query kernel review branch small walking again systemd thinking!","created_at":"2025-12-31T23:48:47.363Z","author":"sunny_glacier_16639","like_count":10},{"id":499989,"body":"Sometimes socket release small careful steal careful review debugging fast.","created_at":"2025-12-31T23:48:18.271Z","author":"clever_comet_7958","like_count":6},{"id":499988,"body":"Droplet database definitely project these always feature yesterday twice broken while systemd twice? Systemd definitely shipping typescript. Someone throughput and load shipping of rarely.","created_at":"2025-12-31T23:46:59.354Z","author":"quiet_sparrow_499","like_count":0},{"id":499987,"body":"Honestly, Refactor big anything nowhere someone... Ready there steal big so tired rarely. Boring merge merge of test bug and this because load.","created_at":"2025-12-31T23:45:45.155Z","author":"wild_badger_40287","like_count":1},{"id":499986,"body":"Database yesterday or never these so something tired an linux everywhere an commit! Maybe done request of.","created_at":"2025-12-31T23:44:35.397Z","author":"cosmic_walrus_3278","like_count":2},{"id":499985,"body":"Unpopular opinion: Finally rarely deploy honest nowhere today very! While nginx again honest once rarely steal debugging release. Fast disk steal review slow someone rarely!","created_at":"2025-12-31T23:43:55.871Z","author":"quiet_quartz_3271","like_count":0},{"id":499984,"body":"Usually done these cpu everything or once rarely benchmark done boring? Lucky writing review an maybe kind? Rewrite slow this never deploy kind yesterday coffee shipping.","created_at":"2025-12-31T23:42:30.285Z","author":"grumpy_otter_11118","like_count":4},{"id":499983,"body":"Review big of again. Morning cache yesterday running after socket everywhere postgres steal the usually.","created_at":"2025-12-31T23:41:40.595Z","author":"zesty_nebula_8232","like_count":1},{"id":499982,"body":"Note to self: But that broken small that test there debugging yesterday very shipping deploy cheap. Slow careful weird everywhere pool.","created_at":"2025-12-31T23:40:02.341Z","author":"sharp_falcon_2327","like_count":3},{"id":499981,"body":"PSA: Thinking index tonight running careful deploy that everywhere thinking branch waiting of boring anything. Tomorrow these postgres thinking shipping after just reading usually throughput simple.","created_at":"2025-12-31T23:38:58.882Z","author":"lucky_orchid_22025","like_count":0}]}"#;

const POST_BODIES: &[(i64, &[u8])] = &[
    (500000, br#"{"post":{"id":500000,"body":"TIL So keepalive the an watching lucky measuring usually everywhere droplet waiting... Nowhere building test those here keepalive network someone and of load? Server big learning ubuntu deploy careful coffee.","created_at":"2025-12-31T23:59:19.409Z","author":"swift_ember_22228","like_count":5}}"#),
    (499999, br#"{"post":{"id":499999,"body":"Something latency boring after users pool lucky linux usually. Cheap because lucky never of index feature cpu those someone! Really writing waiting rewrite fast just while broken surprising weird request...","created_at":"2025-12-31T23:58:21.902Z","author":"quiet_thistle_7672","like_count":18}}"#),
    (499998, br#"{"post":{"id":499998,"body":"Index surprising cheap latency watching!","created_at":"2025-12-31T23:56:51.393Z","author":"sharp_nebula_115","like_count":6}}"#),
    (499997, br#"{"post":{"id":499997,"body":"Honestly, Node merge bug today throughput tomorrow again ready latency shipping tonight really droplet this. Of shipping watching this users done release bug probably while once users!","created_at":"2025-12-31T23:56:31.189Z","author":"rusty_glacier_23474","like_count":8}}"#),
    (499996, br#"{"post":{"id":499996,"body":"Question: This finally sometimes refactor and yesterday cheap ready branch anything debugging surprising... Postgres kernel steal request keepalive ready benchmark careful of tomorrow bug request! Express building load after express node because here.","created_at":"2025-12-31T23:55:08.752Z","author":"cosmic_nebula_25516","like_count":6}}"#),
    (499995, br#"{"post":{"id":499995,"body":"TIL These droplet project benchmark sort nowhere ubuntu?","created_at":"2025-12-31T23:54:00.056Z","author":"golden_falcon_8818","like_count":2}}"#),
    (499994, br#"{"post":{"id":499994,"body":"Socket server debugging merge someone. Memory fast cpu there there keepalive server again before fast something after feature... Really honest something of sort ubuntu socket building never benchmark surprising request network these?","created_at":"2025-12-31T23:53:00.608Z","author":"rusty_quartz_402","like_count":0}}"#),
    (499993, br#"{"post":{"id":499993,"body":"Postgres tired tired someone express project because nowhere cpu today of nginx of today.","created_at":"2025-12-31T23:52:19.685Z","author":"golden_sparrow_1928","like_count":5}}"#),
    (499992, br#"{"post":{"id":499992,"body":"Linux today network pool review running. Sometimes project ready deploy usually writing ready there anything...","created_at":"2025-12-31T23:51:13.831Z","author":"curious_falcon_17531","like_count":1}}"#),
    (499991, br#"{"post":{"id":499991,"body":"Query merge broken tonight maybe because just keepalive nginx index these typescript weird morning.","created_at":"2025-12-31T23:49:33.087Z","author":"grumpy_meadow_21370","like_count":9}}"#),
    (499990, br#"{"post":{"id":499990,"body":"So because nowhere always cpu here weird of systemd. Kernel server ready honest query kernel review branch small walking again systemd thinking!","created_at":"2025-12-31T23:48:47.363Z","author":"sunny_glacier_16639","like_count":10}}"#),
    (499989, br#"{"post":{"id":499989,"body":"Sometimes socket release small careful steal careful review debugging fast.","created_at":"2025-12-31T23:48:18.271Z","author":"clever_comet_7958","like_count":6}}"#),
    (499988, br#"{"post":{"id":499988,"body":"Droplet database definitely project these always feature yesterday twice broken while systemd twice? Systemd definitely shipping typescript. Someone throughput and load shipping of rarely.","created_at":"2025-12-31T23:46:59.354Z","author":"quiet_sparrow_499","like_count":0}}"#),
    (499987, br#"{"post":{"id":499987,"body":"Honestly, Refactor big anything nowhere someone... Ready there steal big so tired rarely. Boring merge merge of test bug and this because load.","created_at":"2025-12-31T23:45:45.155Z","author":"wild_badger_40287","like_count":1}}"#),
    (499986, br#"{"post":{"id":499986,"body":"Database yesterday or never these so something tired an linux everywhere an commit! Maybe done request of.","created_at":"2025-12-31T23:44:35.397Z","author":"cosmic_walrus_3278","like_count":2}}"#),
    (499985, br#"{"post":{"id":499985,"body":"Unpopular opinion: Finally rarely deploy honest nowhere today very! While nginx again honest once rarely steal debugging release. Fast disk steal review slow someone rarely!","created_at":"2025-12-31T23:43:55.871Z","author":"quiet_quartz_3271","like_count":0}}"#),
    (499984, br#"{"post":{"id":499984,"body":"Usually done these cpu everything or once rarely benchmark done boring? Lucky writing review an maybe kind? Rewrite slow this never deploy kind yesterday coffee shipping.","created_at":"2025-12-31T23:42:30.285Z","author":"grumpy_otter_11118","like_count":4}}"#),
    (499983, br#"{"post":{"id":499983,"body":"Review big of again. Morning cache yesterday running after socket everywhere postgres steal the usually.","created_at":"2025-12-31T23:41:40.595Z","author":"zesty_nebula_8232","like_count":1}}"#),
    (499982, br#"{"post":{"id":499982,"body":"Note to self: But that broken small that test there debugging yesterday very shipping deploy cheap. Slow careful weird everywhere pool.","created_at":"2025-12-31T23:40:02.341Z","author":"sharp_falcon_2327","like_count":3}}"#),
    (499981, br#"{"post":{"id":499981,"body":"PSA: Thinking index tonight running careful deploy that everywhere thinking branch waiting of boring anything. Tomorrow these postgres thinking shipping after just reading usually throughput simple.","created_at":"2025-12-31T23:38:58.882Z","author":"lucky_orchid_22025","like_count":0}}"#),
];

fn main() {
    if let Err(error) = run() {
        eprintln!("h12tiny-static failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let address = socket_addr()?;
    let metrics = Arc::new(Metrics::new());
    let executor_threads = executor_threads();
    let service = router().service_with(dispatch(Arc::clone(&metrics)));

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
                .name(format!("h12tiny-static-executor-{index}"))
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
        "h12tiny-static listening on http://{local_address} executor_threads={executor_threads} max_connections={MAX_CONNECTIONS} header_read_timeout_s={} drain_timeout_s={}",
        HEADER_READ_TIMEOUT.as_secs(),
        DRAIN_TIMEOUT.as_secs(),
    );

    let result = smol::block_on(server.shutdown_on(shutdown));
    println!("h12tiny-static shutdown {}", metrics.summary());
    drop(stop);
    for worker in workers {
        let _ = worker.join();
    }
    result?;
    Ok(())
}

fn socket_addr() -> Result<SocketAddr, Box<dyn Error>> {
    let host: IpAddr = std::env::var("HOST")?.parse()?;
    let port: u16 = std::env::var("PORT")?.parse()?;
    Ok(SocketAddr::new(host, port))
}

fn executor_threads() -> usize {
    std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(1)
}

fn router() -> Router<()> {
    Router::new()
        .route("/health", get(health))
        .route("/feed", get(feed))
        .route("/posts", post(create_post).body_limit(BODY_LIMIT))
        .route("/posts/{id}", get(post_by_id))
        .route("/posts/{id}/like", post(like_post).body_limit(BODY_LIMIT))
        .fallback(not_found)
}

async fn health() -> Response {
    json(StatusCode::OK, Bytes::from_static(HEALTH_BODY))
}

async fn feed() -> Response {
    json(StatusCode::OK, Bytes::from_static(FEED_BODY))
}

async fn post_by_id(Path(raw): Path<String>) -> Response {
    let id = match parse_post_id(&raw) {
        Some(id) => id,
        None => return json(StatusCode::BAD_REQUEST, Bytes::from_static(INVALID_ID_BODY)),
    };
    match post_body(id) {
        Some(body) => json(StatusCode::OK, Bytes::from_static(body)),
        None => json(
            StatusCode::NOT_FOUND,
            Bytes::from_static(POST_NOT_FOUND_BODY),
        ),
    }
}

async fn create_post(authorization: Authorization, _body: Bytes) -> Response {
    if bearer_token(&authorization).is_none() {
        return json(
            StatusCode::UNAUTHORIZED,
            Bytes::from_static(MISSING_TOKEN_BODY),
        );
    }
    json(StatusCode::CREATED, Bytes::from_static(CREATE_BODY))
}

async fn like_post(authorization: Authorization, Path(raw): Path<String>) -> Response {
    if bearer_token(&authorization).is_none() {
        return json(
            StatusCode::UNAUTHORIZED,
            Bytes::from_static(MISSING_TOKEN_BODY),
        );
    }
    let id = match parse_post_id(&raw) {
        Some(id) => id,
        None => return json(StatusCode::BAD_REQUEST, Bytes::from_static(INVALID_ID_BODY)),
    };
    if post_body(id).is_none() {
        return json(
            StatusCode::NOT_FOUND,
            Bytes::from_static(POST_NOT_FOUND_BODY),
        );
    }
    let body = format!(r#"{{"liked":true,"already_liked":false,"post_id":{id}}}"#);
    json(StatusCode::CREATED, Bytes::from(body))
}

async fn not_found(_request: Request) -> Response {
    json(StatusCode::NOT_FOUND, Bytes::from_static(NOT_FOUND_BODY))
}

fn json(status: StatusCode, body: Bytes) -> Response {
    let mut response = body.into_response();
    *response.status_mut() = status;
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/json"),
    );
    response
}

fn parse_post_id(raw: &str) -> Option<i64> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let id: i64 = raw.parse().ok()?;
    if id <= 0 {
        return None;
    }
    Some(id)
}

fn post_body(id: i64) -> Option<&'static [u8]> {
    POST_BODIES
        .iter()
        .find(|(candidate, _)| *candidate == id)
        .map(|(_, body)| *body)
}

struct Authorization(Option<HeaderValue>);

fn bearer_token(authorization: &Authorization) -> Option<&str> {
    authorization
        .0
        .as_ref()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

impl<S> FromRequest<S> for Authorization
where
    S: Send + Sync + 'static,
{
    type Rejection = Rejection;

    fn from_request(
        request: Request,
        _state: &Option<S>,
        _meta: &RequestMeta,
    ) -> Pin<Box<dyn Future<Output = Result<(Self, Request), Self::Rejection>> + Send>> {
        let value = request.headers().get("authorization").cloned();
        Box::pin(async move { Ok((Self(value), request)) })
    }
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
) -> impl Fn(Request, Router<()>) -> HandlerFuture + Clone + Send + Sync + 'static {
    move |request, router| {
        let metrics = Arc::clone(&metrics);
        Box::pin(async move {
            let response = router.call_boxed(request).await;
            metrics.response();
            response
        })
    }
}

fn shutdown_signal() -> io::Result<impl Future<Output = ()> + Send + 'static> {
    let mut signals = Signals::new([SIGTERM, SIGINT])?;
    let (sender, receiver) = smol::channel::bounded(1);
    std::thread::Builder::new()
        .name("h12tiny-static-signal".to_owned())
        .spawn(move || {
            if signals.forever().next().is_some() {
                let _ = sender.send_blocking(());
            }
        })?;
    Ok(async move {
        let _ = receiver.recv().await;
    })
}

struct Metrics {
    accepted: AtomicU64,
    rejected: AtomicU64,
    responses: AtomicU64,
}

impl Metrics {
    fn new() -> Self {
        Self {
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            responses: AtomicU64::new(0),
        }
    }

    fn accepted(&self) {
        self.accepted.fetch_add(1, Ordering::Relaxed);
    }

    fn rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    fn response(&self) {
        self.responses.fetch_add(1, Ordering::Relaxed);
    }

    fn summary(&self) -> String {
        format!(
            "accepted={} rejected={} responses={}",
            self.accepted.load(Ordering::Relaxed),
            self.rejected.load(Ordering::Relaxed),
            self.responses.load(Ordering::Relaxed),
        )
    }
}
