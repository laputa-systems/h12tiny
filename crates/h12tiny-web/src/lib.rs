//! A small, protocol-neutral HTTP application substrate.
//!
//! `Router` owns application state and dispatches ordinary HTTP requests to
//! async functions. It deliberately stops at the `http`/`http-body` layer;
//! connection drivers and raw upgrade mechanics belong to `h12tiny-server`.
//! The optional `websocket` feature adds only RFC 6455's standard HTTP/1.1
//! handshake and futures-lite frame adaptation, never application message
//! policy.

use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

pub use bytes::Bytes;
use futures_util::future::{select, Either};
pub use http::header::{HeaderMap, HeaderValue};
#[cfg(feature = "cors")]
use http::header::{HeaderName, ORIGIN, VARY};
use http::header::{ALLOW, CONTENT_TYPE};
pub use http::{Method, StatusCode, Version};
use http::{Request as HttpRequest, Response as HttpResponse};
use http_body::{Body, Frame, SizeHint};
use matchit::Router as MatchRouter;

use h12tiny_util::{boxed_body, collect_bytes_limited, BoxBody, BoxError};

#[cfg(feature = "websocket")]
use base64::Engine as _;
#[cfg(feature = "websocket")]
use fastwebsockets::{after_handshake_split, FragmentCollectorRead, Role, WebSocketWrite};
/// WebSocket frame types exposed by the optional RFC 6455 adapter.
#[cfg(feature = "websocket")]
pub use fastwebsockets::{
    Frame as WebSocketFrame, OpCode as WebSocketOpCode, Payload as WebSocketPayload, WebSocketError,
};
#[cfg(feature = "websocket")]
use h12tiny_core::io::HyperIo;
#[cfg(feature = "websocket")]
use sha1::{Digest as _, Sha1};

/// The request body accepted by handlers after the router erases the
/// transport's concrete body type.
pub type RequestBody = BoxBody;

/// A request with the router's erased body type by default.
pub type Request<B = RequestBody> = HttpRequest<B>;

/// The response body emitted by handlers.
pub type ResponseBody = BoxBody;

/// A response with the router's erased body type by default.
pub type Response<B = ResponseBody> = HttpResponse<B>;

/// A boxed handler future.
pub type HandlerFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

type ServiceFuture =
    Pin<Box<dyn Future<Output = Result<Response, std::convert::Infallible>> + Send>>;

/// Route parameters copied out of `matchit` so they can outlive the match.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RouteParams(Vec<(String, String)>);

impl RouteParams {
    /// Return the value for a named parameter.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value.as_str()))
    }

    /// Iterate over parameters in route declaration order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    fn values(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(_, value)| value.as_str())
    }
}

/// Metadata made available to request extractors.
#[derive(Clone, Debug)]
pub struct RequestMeta {
    params: RouteParams,
    body_limit: Option<usize>,
}

impl RequestMeta {
    /// Parameters captured by the matched route.
    pub fn params(&self) -> &RouteParams {
        &self.params
    }

    /// The route body limit, if one was configured.
    pub fn body_limit(&self) -> Option<usize> {
        self.body_limit
    }
}

/// A normal extraction rejection. Applications can use it directly for small
/// custom extractors or return their own `IntoResponse` error type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    status: StatusCode,
    message: String,
}

impl Rejection {
    /// Build a rejection with an explicit status and short text message.
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    /// Return the response status represented by this rejection.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Return the human-readable rejection message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl StdError for Rejection {}

/// Convert an application value or error into an HTTP response.
pub trait IntoResponse {
    /// Convert this value without coupling it to a protocol driver.
    fn into_response(self) -> Response;
}

fn response_with_body(status: StatusCode, body: impl Into<Bytes>) -> Response {
    HttpResponse::builder()
        .status(status)
        .body(boxed_body(h12tiny_util::bytes_body(body.into())))
        .expect("valid response")
}

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        response_with_body(self.status, self.message)
    }
}

impl IntoResponse for StatusCode {
    fn into_response(self) -> Response {
        HttpResponse::builder()
            .status(self)
            .body(boxed_body(h12tiny_util::empty_body()))
            .expect("valid response")
    }
}

impl IntoResponse for Bytes {
    fn into_response(self) -> Response {
        HttpResponse::new(boxed_body(h12tiny_util::bytes_body(self)))
    }
}

impl IntoResponse for Vec<u8> {
    fn into_response(self) -> Response {
        Bytes::from(self).into_response()
    }
}

impl IntoResponse for String {
    fn into_response(self) -> Response {
        let mut response = response_with_body(StatusCode::OK, self);
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        response
    }
}

impl IntoResponse for &str {
    fn into_response(self) -> Response {
        self.to_owned().into_response()
    }
}

impl<B> IntoResponse for HttpResponse<B>
where
    B: Body<Data = Bytes> + Send + Sync + 'static,
    B::Error: Into<BoxError>,
{
    fn into_response(self) -> Response {
        self.map(boxed_body)
    }
}

impl<T, E> IntoResponse for Result<T, E>
where
    T: IntoResponse,
    E: IntoResponse,
{
    fn into_response(self) -> Response {
        match self {
            Ok(value) => value.into_response(),
            Err(error) => error.into_response(),
        }
    }
}

impl<R> IntoResponse for (StatusCode, R)
where
    R: IntoResponse,
{
    fn into_response(self) -> Response {
        let mut response = self.1.into_response();
        *response.status_mut() = self.0;
        response
    }
}

impl<R> IntoResponse for (HeaderMap, R)
where
    R: IntoResponse,
{
    fn into_response(self) -> Response {
        let mut response = self.1.into_response();
        response.headers_mut().extend(self.0);
        response
    }
}

impl<R> IntoResponse for (StatusCode, HeaderMap, R)
where
    R: IntoResponse,
{
    fn into_response(self) -> Response {
        let mut response = self.2.into_response();
        *response.status_mut() = self.0;
        response.headers_mut().extend(self.1);
        response
    }
}

/// The request body limit was crossed while a handler was reading it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyLimitExceeded {
    limit: usize,
    received: usize,
}

impl BodyLimitExceeded {
    /// Configured byte limit.
    pub fn limit(self) -> usize {
        self.limit
    }

    /// Bytes observed when the limit was crossed.
    pub fn received(self) -> usize {
        self.received
    }
}

impl fmt::Display for BodyLimitExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "request body exceeded limit of {} bytes (received at least {} bytes)",
            self.limit, self.received
        )
    }
}

impl StdError for BodyLimitExceeded {}

struct LimitedBody<B> {
    inner: B,
    limit: usize,
    received: usize,
}

impl<B> LimitedBody<B> {
    fn new(inner: B, limit: usize) -> Self {
        Self {
            inner,
            limit,
            received: 0,
        }
    }
}

impl<B> Body for LimitedBody<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let frame = match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(frame) => frame,
        };
        match frame {
            Some(Ok(frame)) => match frame.into_data() {
                Ok(data) => {
                    let next = self.received.saturating_add(data.len());
                    if next > self.limit {
                        self.received = next;
                        let error: BoxError = Box::new(BodyLimitExceeded {
                            limit: self.limit,
                            received: next,
                        });
                        Poll::Ready(Some(Err(error)))
                    } else {
                        self.received = next;
                        Poll::Ready(Some(Ok(Frame::data(data))))
                    }
                }
                Err(frame) => Poll::Ready(Some(Ok(frame))),
            },
            Some(Err(error)) => Poll::Ready(Some(Err(error.into()))),
            None => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        let mut hint = self.inner.size_hint();
        if hint.lower() > self.limit as u64 {
            hint.set_lower(self.limit as u64);
        }
        if let Some(upper) = hint.upper() {
            hint.set_upper(upper.min(self.limit as u64));
        }
        hint
    }
}

// Boxed future returned by `FromRequest::from_request`.
type ExtractFuture<T, R> = Pin<Box<dyn Future<Output = Result<(T, Request), R>> + Send>>;

/// Request extractor trait used by handler glue.
pub trait FromRequest<S>: Sized + Send + 'static {
    /// The response returned when extraction fails.
    type Rejection: IntoResponse + Send + 'static;

    /// Extract a value and return the request remainder for subsequent
    /// extractors. A body-consuming extractor should be the final extractor.
    fn from_request(
        request: Request,
        state: &Option<S>,
        meta: &RequestMeta,
    ) -> ExtractFuture<Self, Self::Rejection>;
}

fn empty_body() -> RequestBody {
    boxed_body(h12tiny_util::empty_body())
}

fn split_request(request: Request) -> (http::request::Parts, RequestBody) {
    request.into_parts()
}

/// Extract router-owned state by cloning it for the handler future.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State<T>(pub T);

impl<T> State<T> {
    /// Construct a state extractor value.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Consume the wrapper and return the state.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<S> FromRequest<S> for State<S>
where
    S: Clone + Send + Sync + 'static,
{
    type Rejection = Rejection;

    fn from_request(
        request: Request,
        state: &Option<S>,
        _meta: &RequestMeta,
    ) -> ExtractFuture<Self, Self::Rejection> {
        let state = state.clone();
        Box::pin(async move {
            state.map(|state| (Self(state), request)).ok_or_else(|| {
                Rejection::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "router state is not configured",
                )
            })
        })
    }
}

/// Extract a cloned value from `Request::extensions`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extension<T>(pub T);

impl<T> Extension<T> {
    /// Construct an extension extractor value.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Consume the wrapper and return the extension.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<S, T> FromRequest<S> for Extension<T>
where
    T: Clone + Send + Sync + 'static,
    S: Send + Sync + 'static,
{
    type Rejection = Rejection;

    fn from_request(
        request: Request,
        _state: &Option<S>,
        _meta: &RequestMeta,
    ) -> ExtractFuture<Self, Self::Rejection> {
        Box::pin(async move {
            let (parts, body) = split_request(request);
            let value = parts.extensions.get::<T>().cloned().ok_or_else(|| {
                Rejection::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "request extension is missing",
                )
            })?;
            Ok((Self(value), Request::from_parts(parts, body)))
        })
    }
}

/// Optionally extract a cloned value from `Request::extensions`.
///
/// This is useful for request-scoped context such as trace IDs: a handler can
/// remain usable in direct tests while a production service wrapper injects
/// the extension at the transport boundary.
impl<S, T> FromRequest<S> for Option<Extension<T>>
where
    T: Clone + Send + Sync + 'static,
    S: Send + Sync + 'static,
{
    type Rejection = Rejection;

    fn from_request(
        request: Request,
        _state: &Option<S>,
        _meta: &RequestMeta,
    ) -> ExtractFuture<Self, Self::Rejection> {
        Box::pin(async move {
            let (parts, body) = split_request(request);
            let value = parts.extensions.get::<T>().cloned().map(Extension);
            Ok((value, Request::from_parts(parts, body)))
        })
    }
}

impl<S> FromRequest<S> for Bytes
where
    S: Send + Sync + 'static,
{
    type Rejection = Rejection;

    fn from_request(
        request: Request,
        _state: &Option<S>,
        meta: &RequestMeta,
    ) -> ExtractFuture<Self, Self::Rejection> {
        let limit = meta.body_limit.unwrap_or(usize::MAX);
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            match collect_bytes_limited(body, limit).await {
                Ok(bytes) => Ok((bytes, Request::from_parts(parts, empty_body()))),
                Err(error) => {
                    if let Some(error) = body_limit_from_collection(&error) {
                        Err(Rejection::new(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            error.to_string(),
                        ))
                    } else {
                        Err(Rejection::new(
                            StatusCode::BAD_REQUEST,
                            "failed to read request body",
                        ))
                    }
                }
            }
        })
    }
}

fn body_limit_from_collection(
    error: &h12tiny_util::BodyCollectionError<BoxError>,
) -> Option<BodyLimitExceeded> {
    match error {
        h12tiny_util::BodyCollectionError::Body(error) => {
            error.downcast_ref::<BodyLimitExceeded>().copied()
        }
        h12tiny_util::BodyCollectionError::LimitExceeded(error) => Some(BodyLimitExceeded {
            limit: error.limit(),
            received: error.received(),
        }),
        _ => None,
    }
}

/// Extract path parameters into a scalar or a small tuple.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path<T>(pub T);

impl<T> Path<T> {
    /// Construct a path extractor value.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Consume the wrapper and return the parsed value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

/// Conversion used by [`Path`]. Implement this trait for application-specific
/// path structs when a route needs more than built-in scalar/tuple forms.
pub trait PathValue: Sized + Send + 'static {
    /// Parse route values in declaration order.
    fn from_path(params: &RouteParams) -> Result<Self, Rejection>;
}

impl PathValue for String {
    fn from_path(params: &RouteParams) -> Result<Self, Rejection> {
        let mut values = params.values();
        match (values.next(), values.next()) {
            (Some(value), None) => Ok(value.to_owned()),
            _ => Err(Rejection::new(
                StatusCode::BAD_REQUEST,
                "expected one path parameter",
            )),
        }
    }
}

trait ScalarPathValue: Sized + Send + 'static {
    fn parse_scalar(value: &str) -> Result<Self, Rejection>;
}

impl ScalarPathValue for String {
    fn parse_scalar(value: &str) -> Result<Self, Rejection> {
        Ok(value.to_owned())
    }
}

macro_rules! scalar_path_value {
    ($($type:ty),* $(,)?) => {
        $(impl ScalarPathValue for $type {
            fn parse_scalar(value: &str) -> Result<Self, Rejection> {
                value.parse::<$type>().map_err(|_| Rejection::new(
                    StatusCode::BAD_REQUEST,
                    format!("invalid path parameter: {value}"),
                ))
            }
        }

        impl PathValue for $type {
            fn from_path(params: &RouteParams) -> Result<Self, Rejection> {
                let mut values = params.values();
                match (values.next(), values.next()) {
                    (Some(value), None) => Self::parse_scalar(value),
                    _ => Err(Rejection::new(StatusCode::BAD_REQUEST, "expected one path parameter")),
                }
            }
        })*
    };
}

scalar_path_value!(u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize, bool);

impl<A, B> PathValue for (A, B)
where
    A: ScalarPathValue,
    B: ScalarPathValue,
{
    fn from_path(params: &RouteParams) -> Result<Self, Rejection> {
        let mut values = params.values();
        let first = values.next().ok_or_else(|| {
            Rejection::new(StatusCode::BAD_REQUEST, "missing first path parameter")
        })?;
        let second = values.next().ok_or_else(|| {
            Rejection::new(StatusCode::BAD_REQUEST, "missing second path parameter")
        })?;
        if values.next().is_some() {
            return Err(Rejection::new(
                StatusCode::BAD_REQUEST,
                "too many path parameters",
            ));
        }
        Ok((A::parse_scalar(first)?, B::parse_scalar(second)?))
    }
}

impl<A, B, C> PathValue for (A, B, C)
where
    A: ScalarPathValue,
    B: ScalarPathValue,
    C: ScalarPathValue,
{
    fn from_path(params: &RouteParams) -> Result<Self, Rejection> {
        let values: Vec<_> = params.values().collect();
        if values.len() != 3 {
            return Err(Rejection::new(
                StatusCode::BAD_REQUEST,
                "expected three path parameters",
            ));
        }
        Ok((
            A::parse_scalar(values[0])?,
            B::parse_scalar(values[1])?,
            C::parse_scalar(values[2])?,
        ))
    }
}

impl<S, T> FromRequest<S> for Path<T>
where
    T: PathValue,
    S: Send + Sync + 'static,
{
    type Rejection = Rejection;

    fn from_request(
        request: Request,
        _state: &Option<S>,
        meta: &RequestMeta,
    ) -> ExtractFuture<Self, Self::Rejection> {
        let params = meta.params.clone();
        Box::pin(async move { Ok((Self(T::from_path(&params)?), request)) })
    }
}

impl<S> FromRequest<S> for Request
where
    S: Send + Sync + 'static,
{
    type Rejection = Rejection;

    fn from_request(
        request: Request,
        _state: &Option<S>,
        _meta: &RequestMeta,
    ) -> ExtractFuture<Self, Self::Rejection> {
        Box::pin(async move { Ok((request, Request::new(empty_body()))) })
    }
}

/// Raw URI query text, available without enabling typed query deserialization.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawQuery(pub Option<String>);

impl<S> FromRequest<S> for RawQuery
where
    S: Send + Sync + 'static,
{
    type Rejection = Rejection;

    fn from_request(
        request: Request,
        _state: &Option<S>,
        _meta: &RequestMeta,
    ) -> ExtractFuture<Self, Self::Rejection> {
        let query = request.uri().query().map(str::to_owned);
        Box::pin(async move { Ok((Self(query), request)) })
    }
}

/// A handler implementation for a concrete extractor tuple.
pub trait Handler<T, S>: Clone + Send + Sync + 'static {
    /// Invoke the handler and convert its output to a response.
    fn call(&self, request: Request, state: Option<S>, meta: RequestMeta) -> HandlerFuture;
}

macro_rules! handler_impl {
    ($($name:ident),*) => {
        #[allow(non_snake_case, unused_assignments)]
        impl<Hnd, Fut, R, S, $($name),*> Handler<($($name,)*), S> for Hnd
        where
            Hnd: Fn($($name),*) -> Fut + Clone + Send + Sync + 'static,
            Fut: Future<Output = R> + Send + 'static,
            R: IntoResponse,
            S: Clone + Send + Sync + 'static,
            $($name: FromRequest<S>,)*
        {
            fn call(&self, request: Request, state: Option<S>, meta: RequestMeta) -> HandlerFuture {
                let handler = self.clone();
                Box::pin(async move {
                    let mut request = request;
                    $(
                        let ($name, remainder) = match <$name as FromRequest<S>>::from_request(request, &state, &meta).await {
                            Ok(value) => value,
                            Err(error) => return error.into_response(),
                        };
                        request = remainder;
                    )*
                    handler($($name),*).await.into_response()
                })
            }
        }
    };
}

impl<Hnd, Fut, R> Handler<(), ()> for Hnd
where
    Hnd: Fn() -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: IntoResponse,
{
    fn call(&self, _request: Request, _state: Option<()>, _meta: RequestMeta) -> HandlerFuture {
        let handler = self.clone();
        Box::pin(async move { handler().await.into_response() })
    }
}
handler_impl!(A);
handler_impl!(A, B);
handler_impl!(A, B, C);
handler_impl!(A, B, C, D);
handler_impl!(A, B, C, D, E);
handler_impl!(A, B, C, D, E, F);
handler_impl!(A, B, C, D, E, F, G);
handler_impl!(A, B, C, D, E, F, G, H);

trait ErasedHandler<S>: Send + Sync {
    fn call(&self, request: Request, state: Option<S>, meta: RequestMeta) -> HandlerFuture;
}

struct HandlerAdapter<F, T, S> {
    handler: F,
    marker: PhantomData<fn(T, S)>,
}

impl<F, T, S> ErasedHandler<S> for HandlerAdapter<F, T, S>
where
    F: Handler<T, S>,
    S: Clone + Send + Sync + 'static,
    T: Send + 'static,
{
    fn call(&self, request: Request, state: Option<S>, meta: RequestMeta) -> HandlerFuture {
        self.handler.call(request, state, meta)
    }
}

struct Endpoint<S> {
    handler: Arc<dyn ErasedHandler<S>>,
    body_limit: Option<usize>,
    timeout: Option<Duration>,
}

impl<S> Clone for Endpoint<S> {
    fn clone(&self) -> Self {
        Self {
            handler: self.handler.clone(),
            body_limit: self.body_limit,
            timeout: self.timeout,
        }
    }
}

impl<S> Endpoint<S>
where
    S: Clone + Send + Sync + 'static,
{
    fn call(&self, request: Request, state: Option<S>, params: RouteParams) -> HandlerFuture {
        let body_limit = self.body_limit;
        let timeout = self.timeout;
        let request = match body_limit {
            Some(limit) => request.map(|body| boxed_body(LimitedBody::new(body, limit))),
            None => request,
        };
        let future = self
            .handler
            .call(request, state, RequestMeta { params, body_limit });
        match timeout {
            Some(duration) => Box::pin(async move {
                let timer = async_io::Timer::after(duration);
                futures_util::pin_mut!(future);
                futures_util::pin_mut!(timer);
                match select(future, timer).await {
                    Either::Left((response, _timer)) => response,
                    Either::Right((_elapsed, _future)) => response_with_body(
                        StatusCode::REQUEST_TIMEOUT,
                        "request handler deadline exceeded",
                    ),
                }
            }),
            None => future,
        }
    }
}

/// A set of method-specific handlers for one route.
pub struct MethodRouter<S = ()> {
    methods: HashMap<Method, Endpoint<S>>,
}

impl<S> Clone for MethodRouter<S> {
    fn clone(&self) -> Self {
        Self {
            methods: self.methods.clone(),
        }
    }
}

impl<S> MethodRouter<S> {
    fn new(method: Method, endpoint: Endpoint<S>) -> Self {
        let mut methods = HashMap::new();
        methods.insert(method, endpoint);
        Self { methods }
    }

    /// Limit request bytes for every method in this route.
    pub fn body_limit(mut self, limit: usize) -> Self {
        for endpoint in self.methods.values_mut() {
            endpoint.body_limit = Some(limit);
        }
        self
    }

    /// Set an application-handler deadline for every method in this route.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        for endpoint in self.methods.values_mut() {
            endpoint.timeout = Some(timeout);
        }
        self
    }

    /// Disable a previously configured handler deadline.
    pub fn without_timeout(mut self) -> Self {
        for endpoint in self.methods.values_mut() {
            endpoint.timeout = None;
        }
        self
    }

    fn merge(&mut self, other: Self) {
        self.methods.extend(other.methods);
    }

    fn allow_header_value(&self) -> HeaderValue {
        let mut methods: Vec<_> = self.methods.keys().map(Method::as_str).collect();
        methods.sort_unstable();
        HeaderValue::from_str(&methods.join(", "))
            .expect("HTTP method names form a valid Allow header")
    }
}

fn method_router<S, T, F>(method: Method, handler: F) -> MethodRouter<S>
where
    F: Handler<T, S>,
    S: Clone + Send + Sync + 'static,
    T: Send + 'static,
{
    MethodRouter::new(
        method,
        Endpoint {
            handler: Arc::new(HandlerAdapter {
                handler,
                marker: PhantomData,
            }),
            body_limit: None,
            timeout: None,
        },
    )
}

/// Build a GET route handler.
pub fn get<S, T, F>(handler: F) -> MethodRouter<S>
where
    F: Handler<T, S>,
    S: Clone + Send + Sync + 'static,
    T: Send + 'static,
{
    method_router(Method::GET, handler)
}

/// Build a POST route handler.
pub fn post<S, T, F>(handler: F) -> MethodRouter<S>
where
    F: Handler<T, S>,
    S: Clone + Send + Sync + 'static,
    T: Send + 'static,
{
    method_router(Method::POST, handler)
}

/// Build a PUT route handler.
pub fn put<S, T, F>(handler: F) -> MethodRouter<S>
where
    F: Handler<T, S>,
    S: Clone + Send + Sync + 'static,
    T: Send + 'static,
{
    method_router(Method::PUT, handler)
}

/// Build a PATCH route handler.
pub fn patch<S, T, F>(handler: F) -> MethodRouter<S>
where
    F: Handler<T, S>,
    S: Clone + Send + Sync + 'static,
    T: Send + 'static,
{
    method_router(Method::PATCH, handler)
}

/// Build a DELETE route handler.
pub fn delete<S, T, F>(handler: F) -> MethodRouter<S>
where
    F: Handler<T, S>,
    S: Clone + Send + Sync + 'static,
    T: Send + 'static,
{
    method_router(Method::DELETE, handler)
}

/// Build a HEAD route handler.
pub fn head<S, T, F>(handler: F) -> MethodRouter<S>
where
    F: Handler<T, S>,
    S: Clone + Send + Sync + 'static,
    T: Send + 'static,
{
    method_router(Method::HEAD, handler)
}

/// Build an OPTIONS route handler.
pub fn options<S, T, F>(handler: F) -> MethodRouter<S>
where
    F: Handler<T, S>,
    S: Clone + Send + Sync + 'static,
    T: Send + 'static,
{
    method_router(Method::OPTIONS, handler)
}

struct RouteEntry<S> {
    methods: MethodRouter<S>,
}

impl<S> Clone for RouteEntry<S> {
    fn clone(&self) -> Self {
        Self {
            methods: self.methods.clone(),
        }
    }
}

struct RouterRoutes<S> {
    routes: Vec<(String, RouteEntry<S>)>,
    matcher: MatchRouter<usize>,
}

impl<S> Clone for RouterRoutes<S> {
    fn clone(&self) -> Self {
        Self {
            routes: self.routes.clone(),
            matcher: self.matcher.clone(),
        }
    }
}

/// A small path-and-method router.
pub struct Router<S = ()> {
    routes: Arc<RouterRoutes<S>>,
    fallback: Option<Endpoint<S>>,
    state: Option<S>,
    #[cfg(feature = "cors")]
    cors: Option<Arc<Cors>>,
}

impl<S> Clone for Router<S>
where
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            routes: Arc::clone(&self.routes),
            fallback: self.fallback.clone(),
            state: self.state.clone(),
            #[cfg(feature = "cors")]
            cors: self.cors.clone(),
        }
    }
}

impl<S> Router<S> {
    /// Create an empty router. State is optional so stateless handlers can be
    /// used directly, while stateful routers call [`Router::with_state`].
    pub fn new() -> Self {
        Self {
            routes: Arc::new(RouterRoutes {
                routes: Vec::new(),
                matcher: MatchRouter::new(),
            }),
            fallback: None,
            state: None,
            #[cfg(feature = "cors")]
            cors: None,
        }
    }

    /// Attach the shared state used by [`State`] extractors.
    pub fn with_state(mut self, state: S) -> Self {
        self.state = Some(state);
        self
    }

    /// Add or extend a path route.
    pub fn route(mut self, path: &str, methods: MethodRouter<S>) -> Self {
        let routes = Arc::make_mut(&mut self.routes);
        if let Some((_, entry)) = routes.routes.iter_mut().find(|(route, _)| route == path) {
            entry.methods.merge(methods);
        } else {
            let index = routes.routes.len();
            routes
                .matcher
                .insert(path, index)
                .unwrap_or_else(|error| panic!("invalid or conflicting route {path:?}: {error}"));
            routes.routes.push((path.to_owned(), RouteEntry { methods }));
        }
        self
    }

    /// Set an application-handler deadline for every currently configured
    /// route. Routes added after this call retain their own timeout setting.
    ///
    /// This mirrors applying a timeout at a router boundary while keeping the
    /// deadline explicit in the route tree. Streaming routes should be built
    /// outside this subtree (or call [`Router::without_timeout`]).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        for (_, entry) in &mut Arc::make_mut(&mut self.routes).routes {
            entry.methods = entry.methods.clone().timeout(timeout);
        }
        self
    }

    /// Remove handler deadlines from every currently configured route.
    pub fn without_timeout(mut self) -> Self {
        for (_, entry) in &mut Arc::make_mut(&mut self.routes).routes {
            entry.methods = entry.methods.clone().without_timeout();
        }
        self
    }

    /// Limit request bytes for every currently configured route.
    ///
    /// Use [`MethodRouter::body_limit`] when only one method on a route needs
    /// the limit.
    pub fn body_limit(mut self, limit: usize) -> Self {
        for (_, entry) in &mut Arc::make_mut(&mut self.routes).routes {
            entry.methods = entry.methods.clone().body_limit(limit);
        }
        self
    }

    /// Add a fallback handler used when no path matches. A matched path with
    /// no handler for the request method returns a method-not-allowed response.
    pub fn fallback<T, F>(mut self, handler: F) -> Self
    where
        F: Handler<T, S>,
        S: Clone + Send + Sync + 'static,
        T: Send + 'static,
    {
        self.fallback = Some(Endpoint {
            handler: Arc::new(HandlerAdapter {
                handler,
                marker: PhantomData,
            }),
            body_limit: None,
            timeout: None,
        });
        self
    }

    /// Prefix every route in `nested` and add it to this router.
    pub fn nest(mut self, prefix: &str, nested: Router<S>) -> Self {
        for (path, entry) in Arc::unwrap_or_clone(nested.routes).routes {
            let nested_root = path == "/";
            let path = join_paths(prefix, &path);
            // A nested root represents the collection resource itself. APIs
            // commonly document that resource without a trailing slash (for
            // example `/api/v1/machines`), while callers that retain a slash
            // should remain compatible too. Register both spellings here so
            // a router never silently exposes only its non-root children.
            if nested_root && path != "/" {
                self = self.route(&format!("{path}/"), entry.methods.clone());
            }
            self = self.route(&path, entry.methods);
        }
        if self.fallback.is_none() {
            self.fallback = nested.fallback;
        }
        if self.state.is_none() {
            self.state = nested.state;
        }
        self
    }

    /// Merge routes from another router with the same state type.
    pub fn merge(mut self, other: Router<S>) -> Self {
        for (path, entry) in Arc::unwrap_or_clone(other.routes).routes {
            self = self.route(&path, entry.methods);
        }
        if self.fallback.is_none() {
            self.fallback = other.fallback;
        }
        if self.state.is_none() {
            self.state = other.state;
        }
        self
    }

    #[cfg(feature = "cors")]
    /// Attach a structural CORS policy.
    pub fn cors(mut self, cors: Cors) -> Self {
        self.cors = Some(Arc::new(cors));
        self
    }

    /// Dispatch a request after converting its body into the protocol-neutral
    /// `BoxBody` used by extractors.
    pub fn call<B>(&self, request: Request<B>) -> HandlerFuture
    where
        B: Body<Data = Bytes> + Send + Sync + 'static,
        B::Error: Into<BoxError>,
        S: Clone + Send + Sync + 'static,
    {
        self.call_boxed(request.map(boxed_body))
    }

    /// Dispatch a request whose body is already erased.
    pub fn call_boxed(&self, request: Request) -> HandlerFuture
    where
        S: Clone + Send + Sync + 'static,
    {
        #[cfg(feature = "cors")]
        let request_origin = request.headers().get(ORIGIN).cloned();
        #[cfg(feature = "cors")]
        if let Some(cors) = &self.cors {
            if request.method() == Method::OPTIONS
                && request_origin.is_some()
                && request.headers().contains_key("access-control-request-method")
            {
                let cors = cors.clone();
                let allow = self
                    .routes
                    .matcher
                    .at(request.uri().path())
                    .ok()
                    .map(|matched| {
                        self.routes.routes[*matched.value]
                            .1
                            .methods
                            .allow_header_value()
                    });
                return Box::pin(async move {
                    cors.preflight_response(request_origin.as_ref(), allow)
                });
            }
        }

        let path = request.uri().path();
        let method = request.method().clone();
        let matched = self.routes.matcher.at(path).ok();
        let future: HandlerFuture = match matched {
            Some(matched) => {
                let params = RouteParams(
                    matched
                        .params
                        .iter()
                        .map(|(name, value)| (name.to_owned(), value.to_owned()))
                        .collect(),
                );
                let entry = &self.routes.routes[*matched.value];
                match entry.1.methods.methods.get(&method) {
                    Some(endpoint) => endpoint.call(request, self.state.clone(), params),
                    None => {
                        let allow = entry.1.methods.allow_header_value();
                        Box::pin(async move {
                            let mut response = response_with_body(
                                StatusCode::METHOD_NOT_ALLOWED,
                                "method not allowed",
                            );
                            response.headers_mut().insert(ALLOW, allow);
                            response
                        })
                    }
                }
            }
            None => match self.fallback.as_ref() {
                Some(endpoint) => {
                    endpoint.call(request, self.state.clone(), RouteParams::default())
                }
                None => Box::pin(async { response_with_body(StatusCode::NOT_FOUND, "not found") }),
            },
        };
        #[cfg(feature = "cors")]
        if let Some(cors) = &self.cors {
            let cors = cors.clone();
            return Box::pin(
                async move { cors.apply_response(future.await, request_origin.as_ref()) },
            );
        }
        future
    }
}

/// Lets a router be passed directly to Hyper's H1 or H2 server connection
/// drivers. The implementation is generic over the incoming body, so routing
/// itself remains unaware of the negotiated HTTP protocol.
impl<S> Default for Router<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, B> hyper::service::Service<HttpRequest<B>> for Router<S>
where
    S: Clone + Send + Sync + 'static,
    B: Body<Data = Bytes> + Send + Sync + 'static,
    B::Error: Into<BoxError>,
{
    type Response = Response;
    type Error = std::convert::Infallible;
    type Future = ServiceFuture;

    fn call(&self, request: HttpRequest<B>) -> Self::Future {
        let response = Router::call(self, request);
        Box::pin(async move { Ok(response.await) })
    }
}

/// An application-owned dispatch wrapper around a [`Router`].
///
/// The router deliberately does not define a middleware framework. This small
/// adapter instead gives an application one explicit request boundary for
/// request-local context and response accounting while retaining the same
/// direct Hyper service implementation as [`Router`].
pub struct RouterService<S, F> {
    router: Router<S>,
    dispatch: F,
}

impl<S, F> Clone for RouterService<S, F>
where
    Router<S>: Clone,
    F: Clone,
{
    fn clone(&self) -> Self {
        Self {
            router: self.router.clone(),
            dispatch: self.dispatch.clone(),
        }
    }
}

impl<S, F> RouterService<S, F>
where
    S: Clone + Send + Sync + 'static,
    F: Fn(Request, Router<S>) -> HandlerFuture + Clone + Send + Sync + 'static,
{
    /// Dispatch a request that already has the router's erased body type.
    ///
    /// This keeps in-memory application tests on the same explicit transport
    /// boundary as a real Hyper connection.
    pub fn call_boxed(&self, request: Request) -> HandlerFuture {
        (self.dispatch)(request, self.router.clone())
    }
}

impl<S> Router<S> {
    /// Wrap this router in one application-defined transport boundary.
    ///
    /// `dispatch` receives a boxed request and a clone of the router. It is
    /// responsible for calling [`Router::call_boxed`] exactly once when it
    /// wishes to route the request. This supports request extensions, tracing,
    /// and metrics without coupling the web crate to a Tower-style stack.
    pub fn service_with<F>(self, dispatch: F) -> RouterService<S, F>
    where
        F: Clone + Send + Sync + 'static,
    {
        RouterService {
            router: self,
            dispatch,
        }
    }
}

impl<S, F, B> hyper::service::Service<HttpRequest<B>> for RouterService<S, F>
where
    S: Clone + Send + Sync + 'static,
    F: Fn(Request, Router<S>) -> HandlerFuture + Clone + Send + Sync + 'static,
    B: Body<Data = Bytes> + Send + Sync + 'static,
    B::Error: Into<BoxError>,
{
    type Response = Response;
    type Error = std::convert::Infallible;
    type Future = ServiceFuture;

    fn call(&self, request: HttpRequest<B>) -> Self::Future {
        let request = request.map(boxed_body);
        let response = self.call_boxed(request);
        Box::pin(async move { Ok(response.await) })
    }
}

fn join_paths(prefix: &str, path: &str) -> String {
    if prefix == "/" {
        return path.to_owned();
    }
    let prefix = prefix.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix}/{path}")
    }
}

/// Typed query extraction, enabled by the `query` feature.
#[cfg(feature = "query")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query<T>(pub T);

#[cfg(feature = "query")]
impl<T> Query<T> {
    /// Construct a query extractor value.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Consume the wrapper and return the decoded value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

#[cfg(feature = "query")]
fn decode_query_component(input: &str) -> Result<String, &'static str> {
    fn hex_digit(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => output.push(b' '),
            b'%' => {
                let high = bytes
                    .get(index + 1)
                    .and_then(|byte| hex_digit(*byte))
                    .ok_or("invalid percent escape")?;
                let low = bytes
                    .get(index + 2)
                    .and_then(|byte| hex_digit(*byte))
                    .ok_or("invalid percent escape")?;
                output.push(high << 4 | low);
                index += 2;
            }
            byte => output.push(byte),
        }
        index += 1;
    }

    String::from_utf8(output).map_err(|_| "query is not valid UTF-8")
}

#[cfg(feature = "query")]
fn query_value_as_json(value: &str) -> String {
    if matches!(value, "true" | "false" | "null") {
        return value.to_owned();
    }
    if let Ok(number) = value.parse::<i64>() {
        return number.to_string();
    }
    if let Ok(number) = value.parse::<u64>() {
        return number.to_string();
    }
    if let Ok(number) = value.parse::<f64>() {
        if number.is_finite() {
            return number.to_string();
        }
    }
    miniserde::json::to_string(&value)
}

#[cfg(feature = "query")]
/// Convert URL-encoded query pairs into a miniserde JSON object.
///
/// Miniserde intentionally supports JSON rather than form encoding. Query
/// values that are valid JSON primitives retain their primitive type so typed
/// extractors such as `Query<Filters { page: u32 }>` continue to work.
fn query_as_json(query: &str) -> Result<String, &'static str> {
    let mut output = String::from("{");
    let mut has_pair = false;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode_query_component(raw_key)?;
        let value = decode_query_component(raw_value)?;
        if has_pair {
            output.push(',');
        }
        has_pair = true;
        output.push_str(&miniserde::json::to_string(&key));
        output.push(':');
        output.push_str(&query_value_as_json(&value));
    }
    output.push('}');
    Ok(output)
}

#[cfg(feature = "query")]
impl<S, T> FromRequest<S> for Query<T>
where
    T: miniserde::Deserialize + Send + 'static,
    S: Send + Sync + 'static,
{
    type Rejection = Rejection;

    fn from_request(
        request: Request,
        _state: &Option<S>,
        _meta: &RequestMeta,
    ) -> ExtractFuture<Self, Self::Rejection> {
        let query = request.uri().query().unwrap_or_default().to_owned();
        Box::pin(async move {
            query_as_json(&query)
                .and_then(|json| miniserde::json::from_str(&json).map_err(|_| "invalid query"))
                .map(|value| (Self(value), request))
                .map_err(|error| {
                    Rejection::new(StatusCode::BAD_REQUEST, format!("invalid query: {error}"))
                })
        })
    }
}

/// JSON request/response wrapper, enabled by the `json` feature.
#[cfg(feature = "json")]
#[derive(Clone, Debug, PartialEq)]
pub struct Json<T>(pub T);

#[cfg(feature = "json")]
impl<T> Json<T> {
    /// Construct a JSON response or extractor value.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Consume the wrapper and return the decoded value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

#[cfg(feature = "json")]
impl<T> IntoResponse for Json<T>
where
    T: miniserde::Serialize,
{
    fn into_response(self) -> Response {
        let mut response = Bytes::from(miniserde::json::to_string(&self.0)).into_response();
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        response
    }
}

#[cfg(feature = "json")]
impl<S, T> FromRequest<S> for Json<T>
where
    T: miniserde::Deserialize + Send + 'static,
    S: Send + Sync + 'static,
{
    type Rejection = Rejection;

    fn from_request(
        request: Request,
        _state: &Option<S>,
        meta: &RequestMeta,
    ) -> ExtractFuture<Self, Self::Rejection> {
        let limit = meta.body_limit.unwrap_or(usize::MAX);
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let bytes = collect_bytes_limited(body, limit).await.map_err(|error| {
                if let Some(error) = body_limit_from_collection(&error) {
                    Rejection::new(StatusCode::PAYLOAD_TOO_LARGE, error.to_string())
                } else {
                    Rejection::new(StatusCode::BAD_REQUEST, "failed to read JSON body")
                }
            })?;
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                Rejection::new(StatusCode::BAD_REQUEST, "invalid JSON: miniserde error")
            })?;
            let value = miniserde::json::from_str(text).map_err(|error| {
                Rejection::new(StatusCode::BAD_REQUEST, format!("invalid JSON: {error}"))
            })?;
            Ok((Self(value), Request::from_parts(parts, empty_body())))
        })
    }
}

/// Optional JSON request extractor. An empty body becomes `None`; a nonempty
/// body is subject to the same route limit and JSON rejection contract as
/// [`Json`]. This keeps endpoints that genuinely accept an absent body from
/// hand-parsing a raw request.
#[cfg(feature = "json")]
impl<S, T> FromRequest<S> for Option<Json<T>>
where
    T: miniserde::Deserialize + Send + 'static,
    S: Send + Sync + 'static,
{
    type Rejection = Rejection;

    fn from_request(
        request: Request,
        _state: &Option<S>,
        meta: &RequestMeta,
    ) -> ExtractFuture<Self, Self::Rejection> {
        let limit = meta.body_limit.unwrap_or(usize::MAX);
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let bytes = collect_bytes_limited(body, limit).await.map_err(|error| {
                if let Some(error) = body_limit_from_collection(&error) {
                    Rejection::new(StatusCode::PAYLOAD_TOO_LARGE, error.to_string())
                } else {
                    Rejection::new(StatusCode::BAD_REQUEST, "failed to read optional JSON body")
                }
            })?;
            let value = if bytes.is_empty() {
                None
            } else {
                let text = std::str::from_utf8(&bytes).map_err(|_| {
                    Rejection::new(StatusCode::BAD_REQUEST, "invalid JSON: miniserde error")
                })?;
                Some(Json(miniserde::json::from_str(text).map_err(|error| {
                    Rejection::new(StatusCode::BAD_REQUEST, format!("invalid JSON: {error}"))
                })?))
            };
            Ok((value, Request::from_parts(parts, empty_body())))
        })
    }
}

/// Server-sent event value, enabled by the `sse` feature.
#[cfg(feature = "sse")]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Event {
    data: Option<String>,
    event: Option<String>,
    id: Option<String>,
    retry: Option<u64>,
    comments: Vec<String>,
}

/// A value contained a character that is invalid in its SSE field.
#[cfg(feature = "sse")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventFieldError {
    /// Event types cannot contain carriage returns or line feeds.
    EventContainsLineBreak,
    /// Event identifiers cannot contain carriage returns or line feeds.
    IdContainsLineBreak,
    /// Event identifiers cannot contain NUL.
    IdContainsNul,
}

#[cfg(feature = "sse")]
impl fmt::Display for EventFieldError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EventContainsLineBreak => {
                formatter.write_str("SSE event type cannot contain a line break")
            }
            Self::IdContainsLineBreak => {
                formatter.write_str("SSE event identifier cannot contain a line break")
            }
            Self::IdContainsNul => formatter.write_str("SSE event identifier cannot contain NUL"),
        }
    }
}

#[cfg(feature = "sse")]
impl StdError for EventFieldError {}

#[cfg(feature = "sse")]
impl Event {
    /// Construct an empty event.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set event data. Line breaks are framed as multiple `data:` fields.
    pub fn data(mut self, data: impl Into<String>) -> Self {
        self.data = Some(data.into());
        self
    }

    /// Set the event type.
    ///
    /// The value cannot contain a carriage return or line feed, which would
    /// otherwise add another field to the event stream.
    pub fn event(mut self, event: impl Into<String>) -> Result<Self, EventFieldError> {
        let event = event.into();
        if event.contains(['\r', '\n']) {
            return Err(EventFieldError::EventContainsLineBreak);
        }
        self.event = Some(event);
        Ok(self)
    }

    /// Set the event identifier.
    ///
    /// The value cannot contain a carriage return, line feed, or NUL because
    /// those characters have special meaning to SSE clients.
    pub fn id(mut self, id: impl Into<String>) -> Result<Self, EventFieldError> {
        let id = id.into();
        if id.contains(['\r', '\n']) {
            return Err(EventFieldError::IdContainsLineBreak);
        }
        if id.contains('\0') {
            return Err(EventFieldError::IdContainsNul);
        }
        self.id = Some(id);
        Ok(self)
    }

    /// Set the reconnection delay in milliseconds.
    pub fn retry(mut self, milliseconds: u64) -> Self {
        self.retry = Some(milliseconds);
        self
    }

    /// Add an SSE comment (`:<text>`).
    ///
    /// Comments are ignored by SSE clients, which makes them suitable for
    /// keeping an otherwise idle connection active. Newlines are rejected so
    /// one comment cannot accidentally create additional SSE fields.
    pub fn comment(mut self, comment: impl Into<String>) -> Self {
        let comment = comment.into();
        assert!(
            !comment.contains(['\n', '\r']),
            "SSE comments cannot contain newlines"
        );
        self.comments.push(comment);
        self
    }

    fn encode(&self) -> Bytes {
        let mut output = String::new();
        for comment in &self.comments {
            output.push(':');
            output.push_str(comment);
            output.push('\n');
        }
        if let Some(data) = &self.data {
            let bytes = data.as_bytes();
            let mut line_start = 0;
            let mut index = 0;
            while index < bytes.len() {
                if matches!(bytes[index], b'\r' | b'\n') {
                    output.push_str("data: ");
                    output.push_str(&data[line_start..index]);
                    output.push('\n');
                    if bytes[index] == b'\r'
                        && bytes.get(index + 1).is_some_and(|byte| *byte == b'\n')
                    {
                        index += 1;
                    }
                    line_start = index + 1;
                }
                index += 1;
            }
            output.push_str("data: ");
            output.push_str(&data[line_start..]);
            output.push('\n');
        }
        if let Some(event) = &self.event {
            output.push_str("event: ");
            output.push_str(event);
            output.push('\n');
        }
        if let Some(id) = &self.id {
            output.push_str("id: ");
            output.push_str(id);
            output.push('\n');
        }
        if let Some(retry) = self.retry {
            output.push_str("retry: ");
            output.push_str(&retry.to_string());
            output.push('\n');
        }
        output.push('\n');
        Bytes::from(output)
    }
}

#[cfg(feature = "sse")]
trait IntoSseEvent {
    fn into_sse_event(self) -> Result<Event, BoxError>;
}

#[cfg(feature = "sse")]
impl IntoSseEvent for Event {
    fn into_sse_event(self) -> Result<Event, BoxError> {
        Ok(self)
    }
}

#[cfg(feature = "sse")]
impl<E> IntoSseEvent for Result<Event, E>
where
    E: StdError + Send + Sync + 'static,
{
    fn into_sse_event(self) -> Result<Event, BoxError> {
        self.map_err(|error| Box::new(error) as BoxError)
    }
}

#[cfg(feature = "sse")]
trait KeepAliveItem: IntoSseEvent {
    fn keep_alive(event: Event) -> Self;
}

#[cfg(feature = "sse")]
impl KeepAliveItem for Event {
    fn keep_alive(event: Event) -> Self {
        event
    }
}

#[cfg(feature = "sse")]
impl<E> KeepAliveItem for Result<Event, E>
where
    E: StdError + Send + Sync + 'static,
{
    fn keep_alive(event: Event) -> Self {
        Ok(event)
    }
}

/// Configuration for periodic SSE comment frames.
#[cfg(feature = "sse")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeepAlive {
    event: Event,
    interval: Duration,
}

#[cfg(feature = "sse")]
impl KeepAlive {
    /// Construct a keepalive that emits an empty comment every 15 seconds.
    pub fn new() -> Self {
        Self {
            event: Event::default().comment(""),
            interval: Duration::from_secs(15),
        }
    }

    /// Set the maximum idle interval between keepalive comments.
    pub fn interval(mut self, interval: Duration) -> Self {
        assert!(
            !interval.is_zero(),
            "SSE keepalive interval must be non-zero"
        );
        self.interval = interval;
        self
    }

    /// Set the text of the keepalive comment.
    ///
    /// The text cannot contain a newline or carriage return because those
    /// characters would make it more than one SSE comment.
    pub fn text(self, text: impl AsRef<str>) -> Self {
        self.event(Event::default().comment(text.as_ref()))
    }

    /// Set the complete event emitted as the keepalive frame.
    ///
    /// The default event is an empty SSE comment. Applications should use
    /// [`KeepAlive::text`] when only comment text needs to change.
    pub fn event(mut self, event: Event) -> Self {
        self.event = event;
        self
    }

    fn event_value(&self) -> Event {
        self.event.clone()
    }
}

#[cfg(feature = "sse")]
impl Default for KeepAlive {
    fn default() -> Self {
        Self::new()
    }
}

/// A stream wrapper that inserts comment frames when its source is idle.
#[cfg(feature = "sse")]
pub struct KeepAliveStream<S> {
    inner: S,
    timer: async_io::Timer,
    keep_alive: KeepAlive,
}

#[cfg(feature = "sse")]
impl<S> KeepAliveStream<S> {
    /// Wrap a stream with the supplied keepalive policy.
    pub fn new(inner: S, keep_alive: KeepAlive) -> Self {
        let interval = keep_alive.interval;
        Self {
            inner,
            timer: async_io::Timer::after(interval),
            keep_alive,
        }
    }

    fn reset_timer(&mut self) {
        self.timer.set_after(self.keep_alive.interval);
    }
}

#[cfg(feature = "sse")]
impl<S> futures_util::Stream for KeepAliveStream<S>
where
    S: futures_util::Stream,
    S::Item: KeepAliveItem,
{
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // `inner` is pinned for the lifetime of this wrapper. The timer and
        // policy are not pinned fields and are only accessed in place.
        // SAFETY: projecting the pinned wrapper to `inner` never moves it;
        // `inner` remains pinned until this wrapper is dropped. The timer and
        // keepalive policy are accessed in place and are not pinned fields.
        let this = unsafe { self.get_unchecked_mut() };
        let inner = unsafe { Pin::new_unchecked(&mut this.inner) };

        // Upstream activity wins if it becomes ready in the same poll as the
        // timer. This both preserves stream ordering and resets idle time from
        // the activity that actually reached the response.
        match inner.poll_next(cx) {
            Poll::Ready(Some(item)) => {
                this.reset_timer();
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => match Pin::new(&mut this.timer).poll(cx) {
                Poll::Ready(_) => {
                    this.reset_timer();
                    Poll::Ready(Some(S::Item::keep_alive(this.keep_alive.event_value())))
                }
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

/// A streaming SSE response.
#[cfg(feature = "sse")]
pub struct Sse<S> {
    stream: S,
}

#[cfg(feature = "sse")]
impl<S> Sse<S> {
    /// Wrap a stream of [`Event`] values or `Result<Event, E>` values.
    pub fn new(stream: S) -> Self {
        Self { stream }
    }

    /// Insert comment frames whenever the upstream stream is idle.
    pub fn keep_alive(self, keep_alive: KeepAlive) -> Sse<KeepAliveStream<S>> {
        Sse {
            stream: KeepAliveStream::new(self.stream, keep_alive),
        }
    }
}

#[cfg(feature = "sse")]
impl<S> IntoResponse for Sse<S>
where
    S: futures_util::Stream + Send + Sync + 'static,
    S::Item: IntoSseEvent,
{
    fn into_response(self) -> Response {
        let stream = futures_util::StreamExt::map(self.stream, |item| {
            item.into_sse_event()
                .map(|event| Frame::data(event.encode()))
        });
        let body = h12tiny_util::frame_stream_body(stream);
        let mut response = HttpResponse::new(boxed_body(body));
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
        response.headers_mut().insert(
            http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        );
        response
    }
}

/// A tiny structural CORS policy, enabled by the `cors` feature.
#[cfg(feature = "cors")]
#[derive(Clone, Debug)]
pub struct Cors {
    origins: Vec<HeaderValue>,
    methods: HeaderValue,
    headers: HeaderValue,
}

#[cfg(feature = "cors")]
impl Cors {
    /// Start an empty policy; add origins with [`Cors::allow_origin`].
    pub fn new() -> Self {
        Self {
            origins: Vec::new(),
            methods: HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS"),
            headers: HeaderValue::from_static("content-type, authorization"),
        }
    }

    /// Allow one origin. `*` permits all origins.
    pub fn allow_origin(mut self, origin: impl AsRef<str>) -> Self {
        if let Ok(value) = HeaderValue::try_from(origin.as_ref()) {
            self.origins.push(value);
        }
        self
    }

    /// Configure the preflight method field.
    pub fn allow_methods(mut self, methods: impl AsRef<str>) -> Self {
        if let Ok(value) = HeaderValue::try_from(methods.as_ref()) {
            self.methods = value;
        }
        self
    }

    /// Configure the preflight header field.
    pub fn allow_headers(mut self, headers: impl AsRef<str>) -> Self {
        if let Ok(value) = HeaderValue::try_from(headers.as_ref()) {
            self.headers = value;
        }
        self
    }

    fn origin_allowed(&self, origin: &HeaderValue) -> bool {
        self.origins
            .iter()
            .any(|allowed| allowed == "*" || allowed == origin)
    }

    fn preflight_response(
        &self,
        origin: Option<&HeaderValue>,
        allow: Option<HeaderValue>,
    ) -> Response {
        let mut response = StatusCode::NO_CONTENT.into_response();
        if let Some(allow) = allow {
            response.headers_mut().insert(ALLOW, allow);
        }
        response.headers_mut().insert(
            HeaderName::from_static("access-control-allow-methods"),
            self.methods.clone(),
        );
        response.headers_mut().insert(
            HeaderName::from_static("access-control-allow-headers"),
            self.headers.clone(),
        );
        if let Some(origin) = origin.filter(|origin| self.origin_allowed(origin)) {
            response.headers_mut().insert(
                HeaderName::from_static("access-control-allow-origin"),
                origin.clone(),
            );
        }
        vary_on_origin(response.headers_mut());
        response
    }

    fn apply_response(&self, mut response: Response, origin: Option<&HeaderValue>) -> Response {
        if let Some(origin) = origin.filter(|origin| self.origin_allowed(origin)) {
            response.headers_mut().insert(
                HeaderName::from_static("access-control-allow-origin"),
                origin.clone(),
            );
        }
        vary_on_origin(response.headers_mut());
        response
    }
}

#[cfg(feature = "cors")]
fn vary_on_origin(headers: &mut HeaderMap) {
    let already_varies_by_origin = headers.get_all(VARY).iter().any(|value| {
        value.as_bytes().split(|byte| *byte == b',').any(|token| {
            let token = token.trim_ascii();
            token == b"*" || token.eq_ignore_ascii_case(b"origin")
        })
    });
    if !already_varies_by_origin {
        headers.append(VARY, HeaderValue::from_static("Origin"));
    }
}

#[cfg(feature = "cors")]
impl Default for Cors {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "upgrade")]
/// Raw HTTP upgrade future captured from a request.
pub struct HttpUpgrade {
    /// The server-owned future that resolves after the protocol switch.
    pub on_upgrade: h12tiny_server::upgrade::OnUpgrade,
}

#[cfg(feature = "upgrade")]
impl<S> FromRequest<S> for HttpUpgrade
where
    S: Send + Sync + 'static,
{
    type Rejection = Rejection;

    fn from_request(
        mut request: Request,
        _state: &Option<S>,
        _meta: &RequestMeta,
    ) -> ExtractFuture<Self, Self::Rejection> {
        let on_upgrade = h12tiny_server::upgrade::on(&mut request);
        Box::pin(async move { Ok((Self { on_upgrade }, request)) })
    }
}

/// The reader half of an accepted server-role WebSocket connection.
///
/// This is a [`FragmentCollectorRead`] so applications receive complete text
/// and binary messages and validated UTF-8 text. Control-frame replies remain
/// explicit through the callback passed to `read_frame`.
#[cfg(feature = "websocket")]
pub type WebSocketReader =
    FragmentCollectorRead<futures_lite::io::ReadHalf<HyperIo<h12tiny_server::upgrade::Upgraded>>>;

/// The writer half of an accepted server-role WebSocket connection.
#[cfg(feature = "websocket")]
pub type WebSocketWriter =
    WebSocketWrite<futures_lite::io::WriteHalf<HyperIo<h12tiny_server::upgrade::Upgraded>>>;

/// A framed, server-role RFC 6455 connection obtained from a successful HTTP
/// upgrade.
///
/// h12tiny owns only HTTP validation, the switching response, and I/O/frame
/// adaptation. Application protocol semantics—including subprotocols,
/// authentication, message handling, backpressure, and task ownership—remain
/// with the caller.
#[cfg(feature = "websocket")]
pub struct WebSocketConnection {
    reader: WebSocketReader,
    writer: WebSocketWriter,
}

#[cfg(feature = "websocket")]
impl WebSocketConnection {
    /// Split the connection into independently owned reader and writer halves.
    pub fn split(self) -> (WebSocketReader, WebSocketWriter) {
        (self.reader, self.writer)
    }
}

/// A malformed or unsupported RFC 6455 HTTP upgrade request.
#[cfg(feature = "websocket")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebSocketUpgradeError {
    /// RFC 6455's HTTP upgrade handshake begins with an HTTP `GET` request.
    Method,
    /// RFC 6455's HTTP upgrade handshake is only valid over HTTP/1.1 here.
    HttpVersion,
    /// HTTP/1.1's mandatory `Host` header is absent or malformed.
    Host,
    /// `Connection` does not include the `Upgrade` token.
    Connection,
    /// `Upgrade` does not include the `websocket` token.
    Upgrade,
    /// `Sec-WebSocket-Version` is absent or is not version 13.
    Version,
    /// `Sec-WebSocket-Key` is absent.
    MissingKey,
    /// `Sec-WebSocket-Key` is not base64 for exactly 16 bytes.
    InvalidKey,
}

#[cfg(feature = "websocket")]
impl fmt::Display for WebSocketUpgradeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Method => "WebSocket upgrades require GET",
            Self::HttpVersion => "WebSocket upgrades require HTTP/1.1",
            Self::Host => "WebSocket upgrades require Host",
            Self::Connection => "WebSocket upgrades require Connection: Upgrade",
            Self::Upgrade => "WebSocket upgrades require Upgrade: websocket",
            Self::Version => "WebSocket upgrades require Sec-WebSocket-Version: 13",
            Self::MissingKey => "WebSocket upgrades require Sec-WebSocket-Key",
            Self::InvalidKey => "Sec-WebSocket-Key must decode to exactly 16 bytes",
        })
    }
}

#[cfg(feature = "websocket")]
impl StdError for WebSocketUpgradeError {}

/// A validated RFC 6455 WebSocket request whose upgraded connection can be
/// awaited after its switching response has been returned.
///
/// [`HttpUpgrade`] remains the smaller raw escape hatch for any other upgrade
/// protocol. Use this type only when h12tiny should own the standard WebSocket
/// handshake and server-role frame adaptation.
#[cfg(feature = "websocket")]
pub struct WebSocketUpgrade {
    accept: String,
    on_upgrade: h12tiny_server::upgrade::OnUpgrade,
}

#[cfg(feature = "websocket")]
impl WebSocketUpgrade {
    /// Validate and capture the upgrade from an erased router request.
    ///
    /// This constructor lets an application map malformed-handshake errors to
    /// its own error response. The [`FromRequest`] implementation below maps
    /// the same errors to h12tiny's standard 400 rejection.
    pub fn try_from_request(request: &mut Request) -> Result<Self, WebSocketUpgradeError> {
        let key = validate_websocket_request(request)?;
        let on_upgrade = h12tiny_server::upgrade::on(request);
        Ok(Self {
            accept: websocket_accept(&key),
            on_upgrade,
        })
    }

    /// Build the RFC 6455 `101 Switching Protocols` response.
    ///
    /// Call this before moving the upgrade into a task that awaits
    /// [`Self::into_connection`]; Hyper resolves the raw upgrade only after
    /// this response has been accepted by the connection driver.
    pub fn response(&self) -> Response {
        Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header("connection", "Upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-accept", &self.accept)
            .body(boxed_body(h12tiny_util::empty_body()))
            .expect("RFC 6455's static response headers are valid")
    }

    /// Await the HTTP protocol switch and adapt the stream to complete-message
    /// WebSocket reads plus server-role frame writes.
    pub async fn into_connection(self) -> Result<WebSocketConnection, hyper::Error> {
        let upgraded = self.on_upgrade.await?;
        let (read, write) = futures_lite::io::split(HyperIo::new(upgraded));
        let (reader, writer) = after_handshake_split(read, write, Role::Server);
        Ok(WebSocketConnection {
            reader: FragmentCollectorRead::new(reader),
            writer,
        })
    }
}

#[cfg(feature = "websocket")]
impl<S> FromRequest<S> for WebSocketUpgrade
where
    S: Send + Sync + 'static,
{
    type Rejection = Rejection;

    fn from_request(
        mut request: Request,
        _state: &Option<S>,
        _meta: &RequestMeta,
    ) -> ExtractFuture<Self, Self::Rejection> {
        Box::pin(async move {
            let upgrade = Self::try_from_request(&mut request)
                .map_err(|error| Rejection::new(StatusCode::BAD_REQUEST, error.to_string()))?;
            Ok((upgrade, request))
        })
    }
}

#[cfg(feature = "websocket")]
fn validate_websocket_request(request: &Request) -> Result<String, WebSocketUpgradeError> {
    if request.method() != Method::GET {
        return Err(WebSocketUpgradeError::Method);
    }
    if request.version() != Version::HTTP_11 {
        return Err(WebSocketUpgradeError::HttpVersion);
    }
    if request.headers().get_all("host").iter().count() != 1
        || request
            .headers()
            .get("host")
            .and_then(|value| value.to_str().ok())
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err(WebSocketUpgradeError::Host);
    }
    if !header_has_token(request, "connection", "upgrade") {
        return Err(WebSocketUpgradeError::Connection);
    }
    if !header_has_token(request, "upgrade", "websocket") {
        return Err(WebSocketUpgradeError::Upgrade);
    }
    let versions = request.headers().get_all("sec-websocket-version");
    if versions.iter().count() != 1
        || versions
            .iter()
            .next()
            .and_then(|value| value.to_str().ok())
            .is_none_or(|value| value.trim() != "13")
    {
        return Err(WebSocketUpgradeError::Version);
    }

    let mut keys = request
        .headers()
        .get_all("sec-websocket-key")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::trim);
    let Some(key) = keys.next() else {
        return Err(WebSocketUpgradeError::MissingKey);
    };
    if keys.next().is_some()
        || !base64::engine::general_purpose::STANDARD
            .decode(key)
            .is_ok_and(|decoded| decoded.len() == 16)
    {
        return Err(WebSocketUpgradeError::InvalidKey);
    }
    Ok(key.to_owned())
}

#[cfg(feature = "websocket")]
fn header_has_token(request: &Request, header: &str, expected: &str) -> bool {
    request
        .headers()
        .get_all(header)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case(expected))
}

#[cfg(feature = "websocket")]
fn websocket_accept(key: &str) -> String {
    let mut digest = Sha1::new();
    digest.update(key.as_bytes());
    digest.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64::engine::general_purpose::STANDARD.encode(digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_lite::future::block_on;
    use http_body_util::BodyExt;

    async fn hello() -> &'static str {
        "hello"
    }

    #[test]
    fn routes_methods_and_wildcards() {
        let router = Router::new()
            .route("/hello", get(hello))
            .route(
                "/files/{*rest}",
                get(|Path(path): Path<String>| async move { path }),
            )
            .route(
                "/pairs/{name}/{id}",
                get(|Path((name, id)): Path<(String, u64)>| async move { format!("{name}:{id}") }),
            )
            .route(
                "/items/{id}",
                post(|Path(id): Path<u64>| async move { id.to_string() }),
            );
        let response = block_on(
            router.call(
                Request::get("/hello")
                    .body(h12tiny_util::empty_body())
                    .unwrap(),
            ),
        );
        assert_eq!(response.status(), StatusCode::OK);
        let body = block_on(response.into_body().collect()).unwrap().to_bytes();
        assert_eq!(&body[..], b"hello");

        let response = block_on(
            router.call(
                Request::get("/files/a/b")
                    .body(h12tiny_util::empty_body())
                    .unwrap(),
            ),
        );
        let body = block_on(response.into_body().collect()).unwrap().to_bytes();
        assert_eq!(&body[..], b"a/b");
    }

    #[test]
    fn state_extension_and_body_limit_are_in_memory() {
        let mut request = Request::post("/upload")
            .body(h12tiny_util::bytes_body("12345"))
            .unwrap();
        request.extensions_mut().insert(7_u32);
        let router = Router::new()
            .route(
                "/upload",
                post(
                    |State(state): State<String>,
                     Extension(number): Extension<u32>,
                     body: Bytes| async move {
                        format!("{state}:{number}:{}", body.len())
                    },
                )
                .body_limit(4),
            )
            .with_state("state".to_owned());
        let response = block_on(router.call(request));
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn raw_request_handler_can_classify_body_limit_errors() {
        let router = Router::<()>::new().route(
            "/upload",
            post(|request: Request| async move {
                match request.into_body().collect().await {
                    Ok(_) => StatusCode::OK.into_response(),
                    Err(error) => match error.downcast_ref::<BodyLimitExceeded>() {
                        Some(limit) => (
                            StatusCode::PAYLOAD_TOO_LARGE,
                            format!("{}:{}", limit.limit(), limit.received()),
                        )
                            .into_response(),
                        None => StatusCode::BAD_REQUEST.into_response(),
                    },
                }
            })
            .body_limit(1),
        );
        let response = block_on(router.call(
            Request::post("/upload")
                .body(h12tiny_util::bytes_body("123"))
                .unwrap(),
        ));
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            &block_on(response.into_body().collect()).unwrap().to_bytes()[..],
            b"1:3"
        );
    }

    #[test]
    fn raw_request_handler_can_inspect_typed_body_errors() {
        let router = Router::<()>::new().route(
            "/upload",
            post(|request: Request| async move {
                match request.into_body().collect().await {
                    Ok(_) => StatusCode::OK,
                    Err(error) => match error.downcast_ref::<std::io::Error>() {
                        Some(error)
                            if error.kind() == std::io::ErrorKind::ConnectionReset =>
                        {
                            StatusCode::BAD_GATEWAY
                        }
                        _ => StatusCode::BAD_REQUEST,
                    },
                }
            })
            .body_limit(16),
        );
        let body = h12tiny_util::frame_stream_body(futures_util::stream::iter([
            Err::<Frame<Bytes>, _>(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "transport failure",
            )),
        ]));
        let response = block_on(router.call(
            Request::post("/upload").body(body).unwrap(),
        ));
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn nest_merge_fallback_and_timeout_work() {
        let nested = Router::new()
            .route("/", get(|| async { "root" }))
            .route("/health", get(|| async { "ok" }));
        let router = Router::new()
            .nest("/api", nested)
            .fallback(|| async { (StatusCode::NOT_FOUND, "fallback") });
        let response = block_on(
            router.call(
                Request::get("/api")
                    .body(h12tiny_util::empty_body())
                    .unwrap(),
            ),
        );
        assert_eq!(response.status(), StatusCode::OK);
        let response = block_on(
            router.call(
                Request::get("/api/")
                    .body(h12tiny_util::empty_body())
                    .unwrap(),
            ),
        );
        assert_eq!(response.status(), StatusCode::OK);
        let response = block_on(
            router.call(
                Request::get("/api/health")
                    .body(h12tiny_util::empty_body())
                    .unwrap(),
            ),
        );
        assert_eq!(response.status(), StatusCode::OK);
        let response = block_on(
            router.call(
                Request::get("/missing")
                    .body(h12tiny_util::empty_body())
                    .unwrap(),
            ),
        );
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let router = Router::new().route(
            "/slow",
            get(|| async {
                async_io::Timer::after(Duration::from_millis(50)).await;
                "late"
            })
            .timeout(Duration::from_millis(1)),
        );
        let response = block_on(
            router.call(
                Request::get("/slow")
                    .body(h12tiny_util::empty_body())
                    .unwrap(),
            ),
        );
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    }

    #[cfg(feature = "websocket")]
    fn websocket_request(key: &str) -> Request {
        Request::builder()
            .version(Version::HTTP_11)
            .header("host", "localhost")
            .header("connection", "keep-alive, Upgrade")
            .header("upgrade", "WebSocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", key)
            .body(boxed_body(h12tiny_util::empty_body()))
            .expect("the fixed WebSocket request is valid HTTP")
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn websocket_upgrade_validates_rfc_sample_and_builds_switching_response() {
        let mut request = websocket_request("dGhlIHNhbXBsZSBub25jZQ==");
        let upgrade = WebSocketUpgrade::try_from_request(&mut request)
            .expect("the RFC 6455 sample request must validate");
        let response = upgrade.response();
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        assert_eq!(response.headers()["connection"], "Upgrade");
        assert_eq!(response.headers()["upgrade"], "websocket");
        assert_eq!(
            response.headers()["sec-websocket-accept"],
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn websocket_upgrade_rejects_invalid_and_ambiguous_keys() {
        let mut invalid = websocket_request("aW52YWxpZA==");
        assert_eq!(
            WebSocketUpgrade::try_from_request(&mut invalid).err(),
            Some(WebSocketUpgradeError::InvalidKey)
        );

        let mut repeated = websocket_request("dGhlIHNhbXBsZSBub25jZQ==");
        repeated.headers_mut().append(
            "sec-websocket-key",
            HeaderValue::from_static("dGhlIHNhbXBsZSBub25jZQ=="),
        );
        assert_eq!(
            WebSocketUpgrade::try_from_request(&mut repeated).err(),
            Some(WebSocketUpgradeError::InvalidKey)
        );
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn websocket_upgrade_requires_get_host_and_one_version_header() {
        let mut wrong_method = websocket_request("dGhlIHNhbXBsZSBub25jZQ==");
        *wrong_method.method_mut() = Method::POST;
        assert_eq!(
            WebSocketUpgrade::try_from_request(&mut wrong_method).err(),
            Some(WebSocketUpgradeError::Method)
        );

        let mut missing_host = websocket_request("dGhlIHNhbXBsZSBub25jZQ==");
        missing_host.headers_mut().remove("host");
        assert_eq!(
            WebSocketUpgrade::try_from_request(&mut missing_host).err(),
            Some(WebSocketUpgradeError::Host)
        );

        let mut repeated_version = websocket_request("dGhlIHNhbXBsZSBub25jZQ==");
        repeated_version
            .headers_mut()
            .append("sec-websocket-version", HeaderValue::from_static("13"));
        assert_eq!(
            WebSocketUpgrade::try_from_request(&mut repeated_version).err(),
            Some(WebSocketUpgradeError::Version)
        );
    }

    #[cfg(feature = "json")]
    #[test]
    fn json_extractor_and_response_are_bounded() {
        #[derive(miniserde::Deserialize, miniserde::Serialize)]
        struct Payload {
            name: String,
        }

        let router = Router::<()>::new().route(
            "/json",
            post(|Json(payload): Json<Payload>| async move { Json(payload) }).body_limit(64),
        );
        let request = Request::post("/json")
            .header(CONTENT_TYPE, "application/json")
            .body(h12tiny_util::bytes_body(r#"{"name":"tiny"}"#))
            .unwrap();
        let response = block_on(router.call(request));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
    }

    #[cfg(feature = "json")]
    #[test]
    fn optional_json_distinguishes_an_empty_body_from_invalid_json() {
        #[derive(miniserde::Deserialize)]
        struct Payload {
            name: String,
        }

        let router = Router::<()>::new().route(
            "/optional",
            post(|payload: Option<Json<Payload>>| async move {
                payload
                    .map(|Json(payload)| payload.name)
                    .unwrap_or_else(|| "absent".to_owned())
            }),
        );
        let response = block_on(
            router.call(
                Request::post("/optional")
                    .body(h12tiny_util::empty_body())
                    .unwrap(),
            ),
        );
        let body = block_on(response.into_body().collect()).unwrap().to_bytes();
        assert_eq!(&body[..], b"absent");

        let response = block_on(
            router.call(
                Request::post("/optional")
                    .body(h12tiny_util::bytes_body("not json"))
                    .unwrap(),
            ),
        );
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[cfg(feature = "query")]
    #[test]
    fn query_and_raw_query_extract_without_protocol_state() {
        #[derive(miniserde::Deserialize)]
        struct Filters {
            page: u32,
        }

        let router = Router::<()>::new().route(
            "/search",
            get(
                |Query(filters): Query<Filters>, RawQuery(raw): RawQuery| async move {
                    format!("{}:{}", filters.page, raw.unwrap())
                },
            ),
        );
        let request = Request::get("/search?page=3")
            .body(h12tiny_util::empty_body())
            .unwrap();
        let response = block_on(router.call(request));
        let body = block_on(response.into_body().collect()).unwrap().to_bytes();
        assert_eq!(&body[..], b"3:page=3");
    }

    #[cfg(feature = "sse")]
    #[test]
    fn sse_frames_standard_fields() {
        let event = Event::new()
            .event("message")
            .unwrap()
            .id("1")
            .unwrap()
            .retry(5000)
            .data("one\r\ntwo\rlast\n");
        assert_eq!(
            &event.encode()[..],
            b"data: one\ndata: two\ndata: last\ndata: \nevent: message\nid: 1\nretry: 5000\n\n"
        );
        let response = Sse::new(futures_util::stream::iter(vec![event])).into_response();
        assert_eq!(response.headers()[CONTENT_TYPE], "text/event-stream");
    }

    #[cfg(feature = "sse")]
    #[test]
    fn sse_rejects_line_breaks_and_nul_in_protocol_fields() {
        assert_eq!(
            Event::new().event("message\r\nevent: injected"),
            Err(EventFieldError::EventContainsLineBreak)
        );
        assert_eq!(
            Event::new().id("1\nid: injected"),
            Err(EventFieldError::IdContainsLineBreak)
        );
        assert_eq!(
            Event::new().id("1\0other"),
            Err(EventFieldError::IdContainsNul)
        );
    }

    #[cfg(feature = "cors")]
    fn response_for<S: Clone + Send + Sync + 'static>(
        router: &Router<S>,
        method: Method,
        path: &str,
        origin: Option<&str>,
        preflight_method: Option<&str>,
    ) -> Response {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(origin) = origin {
            request = request.header(ORIGIN, origin);
        }
        if let Some(method) = preflight_method {
            request = request.header("access-control-request-method", method);
        }
        block_on(router.call(request.body(h12tiny_util::empty_body()).unwrap()))
    }

    #[cfg(feature = "cors")]
    #[test]
    fn cors_preserves_vary_values_and_varies_on_origin_for_every_origin() {
        let mut headers = HeaderMap::new();
        headers.append(http::header::VARY, HeaderValue::from_static("Accept-Encoding"));
        headers.append(http::header::VARY, HeaderValue::from_static("X-Client-Hint"));
        let router = Router::new()
            .route(
                "/vary",
                get(move || {
                    let headers = headers.clone();
                    async move { (headers, "ok") }
                }),
            )
            .cors(Cors::new().allow_origin("https://allowed.example"));

        let response = response_for(
            &router,
            Method::GET,
            "/vary",
            Some("https://allowed.example"),
            None,
        );
        assert_eq!(response.headers().get_all(http::header::VARY).iter().count(), 3);
        assert_eq!(
            response.headers()["access-control-allow-origin"],
            "https://allowed.example"
        );

        let response = response_for(
            &router,
            Method::GET,
            "/vary",
            Some("https://blocked.example"),
            None,
        );
        assert_eq!(response.headers().get_all(http::header::VARY).iter().count(), 3);
        assert!(response.headers().get("access-control-allow-origin").is_none());

        let response = response_for(&router, Method::GET, "/vary", None, None);
        assert_eq!(response.headers().get_all(http::header::VARY).iter().count(), 3);
    }

    #[cfg(feature = "cors")]
    #[test]
    fn cors_does_not_duplicate_origin_or_append_it_after_vary_wildcard() {
        let mut origin_headers = HeaderMap::new();
        origin_headers.insert(
            http::header::VARY,
            HeaderValue::from_static("Accept-Encoding, oRiGiN"),
        );
        let with_origin = Router::new()
            .route(
                "/vary",
                get(move || {
                    let headers = origin_headers.clone();
                    async move { (headers, "ok") }
                }),
            )
            .cors(Cors::new());
        let response = response_for(&with_origin, Method::GET, "/vary", None, None);
        assert_eq!(response.headers().get_all(http::header::VARY).iter().count(), 1);
        assert_eq!(response.headers()[http::header::VARY], "Accept-Encoding, oRiGiN");

        let mut wildcard_headers = HeaderMap::new();
        wildcard_headers.insert(http::header::VARY, HeaderValue::from_static("*"));
        let with_wildcard = Router::new()
            .route(
                "/vary",
                get(move || {
                    let headers = wildcard_headers.clone();
                    async move { (headers, "ok") }
                }),
            )
            .cors(Cors::new());
        let response = response_for(&with_wildcard, Method::GET, "/vary", None, None);
        assert_eq!(response.headers().get_all(http::header::VARY).iter().count(), 1);
        assert_eq!(response.headers()[http::header::VARY], "*");
    }

    #[cfg(feature = "cors")]
    #[test]
    fn cors_only_treats_options_with_requested_method_as_preflight_and_decorates_errors() {
        let router = Router::new()
            .route("/resource", get(|| async { "get" }))
            .route("/resource", options(|| async { "ordinary options" }))
            .cors(Cors::new().allow_origin("https://allowed.example"));

        let response = response_for(
            &router,
            Method::OPTIONS,
            "/resource",
            Some("https://allowed.example"),
            None,
        );
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get("access-control-allow-methods").is_none());
        assert_eq!(response.headers()["access-control-allow-origin"], "https://allowed.example");

        let response = response_for(
            &router,
            Method::OPTIONS,
            "/resource",
            Some("https://allowed.example"),
            Some("GET"),
        );
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(response.headers()[ALLOW], "GET, OPTIONS");
        assert_eq!(response.headers()["access-control-allow-origin"], "https://allowed.example");

        let missing = response_for(&router, Method::GET, "/missing", None, None);
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert_eq!(missing.headers()[http::header::VARY], "Origin");

        let wrong_method = response_for(
            &router,
            Method::POST,
            "/resource",
            Some("https://allowed.example"),
            None,
        );
        assert_eq!(wrong_method.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(wrong_method.headers()["access-control-allow-origin"], "https://allowed.example");
        assert_eq!(wrong_method.headers()[http::header::VARY], "Origin");
    }

    #[test]
    fn method_not_allowed_lists_only_configured_methods_in_stable_order() {
        let router = Router::new()
            .route("/resource", post(|| async { "post" }))
            .route("/resource", get(|| async { "get" }));
        let response = block_on(router.call(
            Request::delete("/resource")
                .body(h12tiny_util::empty_body())
                .unwrap(),
        ));
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[ALLOW], "GET, POST");
    }

    #[cfg(feature = "sse")]
    #[test]
    fn sse_keepalive_emits_comments_only_when_upstream_is_idle() {
        use futures_lite::future::poll_once;
        use futures_util::StreamExt;
        use std::convert::Infallible;

        let source =
            futures_util::stream::once(async { Ok::<_, Infallible>(Event::new().data("first")) })
                .chain(futures_util::stream::pending());
        let response = Sse::new(source)
            .keep_alive(
                KeepAlive::new()
                    .interval(Duration::from_millis(15))
                    .text("tick"),
            )
            .into_response();
        let mut data = response.into_body().into_data_stream();

        assert_eq!(
            block_on(data.next()).unwrap().unwrap(),
            Bytes::from_static(b"data: first\n\n")
        );
        assert!(block_on(poll_once(data.next())).is_none());
        assert_eq!(
            block_on(data.next()).unwrap().unwrap(),
            Bytes::from_static(b":tick\n\n")
        );
    }

    #[cfg(feature = "sse")]
    #[test]
    #[should_panic(expected = "non-zero")]
    fn sse_keepalive_rejects_zero_interval() {
        let _ = KeepAlive::default().interval(Duration::ZERO);
    }

    #[cfg(feature = "sse")]
    #[test]
    fn sse_keepalive_preserves_upstream_cancellation() {
        use futures_lite::future::poll_once;
        use futures_util::{Stream, StreamExt};
        use std::convert::Infallible;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };

        struct PendingStream(Arc<AtomicBool>);

        impl Stream for PendingStream {
            type Item = Result<Event, Infallible>;

            fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
                Poll::Pending
            }
        }

        impl Drop for PendingStream {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let response = Sse::new(PendingStream(dropped.clone()))
            .keep_alive(KeepAlive::new().interval(Duration::from_secs(60)))
            .into_response();
        let mut data = response.into_body().into_data_stream();
        assert!(block_on(poll_once(data.next())).is_none());
        drop(data);

        assert!(dropped.load(Ordering::SeqCst));
    }
}
