//! Protocol-neutral HTTP body conveniences.
//!
//! This crate intentionally does not know about DNS, TCP, TLS, client pools,
//! server listeners, or routing. Its optional `json` feature is the only
//! serialization surface.

use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use async_io::Timer;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures_io::AsyncRead;
use futures_util::future::poll_fn;
use futures_util::stream::{Stream, StreamExt};
use http::{HeaderValue, Request, Response};
use http_body::{Body, Frame, SizeHint};
use http_body_util::combinators::{BoxBody as HttpBoxBody, UnsyncBoxBody};
use http_body_util::BodyExt as HttpBodyExt;
use pin_project_lite::pin_project;

/// The error type used by [`BoxBody`].
pub type BoxError = Box<dyn StdError + Send + Sync>;

/// An intentionally named erased body for applications that use several
/// concrete request body implementations.
///
/// Concrete body types remain available for applications that do not need
/// type erasure. `BoxBody` requires a `Send + Sync` body; use
/// [`UnsyncBoxedBody`] or [`boxed_unsync_body`] when synchronization is not
/// required.
pub type BoxBody = HttpBoxBody<Bytes, BoxError>;

/// An erased body that is `Send` but need not be `Sync`.
pub type UnsyncBoxedBody = UnsyncBoxBody<Bytes, BoxError>;

/// Re-export the standard `http-body-util` extension methods, including
/// `collect`, `boxed`, `into_stream`, and `into_data_stream`.
pub use http_body_util::BodyExt;
pub use http_body_util::{
    BodyDataStream, BodyStream, Collected, Empty, Full, LengthLimitError, Limited, StreamBody,
};

/// Construct an empty body using the standard `http-body-util` primitive.
pub fn empty_body() -> Empty<Bytes> {
    Empty::new()
}

/// Short alias for [`empty_body`].
pub fn empty() -> Empty<Bytes> {
    empty_body()
}

/// Construct a single-frame body from bytes.
pub fn bytes_body(bytes: impl Into<Bytes>) -> Full<Bytes> {
    Full::new(bytes.into())
}

/// Short alias for [`bytes_body`].
pub fn bytes(bytes: impl Into<Bytes>) -> Full<Bytes> {
    bytes_body(bytes)
}

/// Construct a single-frame body without copying a static byte slice.
pub fn static_bytes(bytes: &'static [u8]) -> Full<Bytes> {
    bytes_body(Bytes::from_static(bytes))
}

/// Construct a body from a stream of complete HTTP frames.
pub fn frame_stream_body<S, D, E>(stream: S) -> StreamBody<S>
where
    S: Stream<Item = Result<Frame<D>, E>>,
    D: Buf,
{
    StreamBody::new(stream)
}

/// Construct a request body from a stream of data buffers.
///
/// Data items are converted into data frames. Trailers can be supplied by
/// using [`frame_stream_body`] directly.
pub fn stream_body<S, D, E>(stream: S) -> StreamBody<impl Stream<Item = Result<Frame<D>, E>>>
where
    S: Stream<Item = Result<D, E>>,
    D: Buf,
{
    StreamBody::new(stream.map(|item| item.map(Frame::data)))
}

/// Alias for [`stream_body`] that makes the data-only nature explicit.
pub fn data_stream_body<S, D, E>(stream: S) -> StreamBody<impl Stream<Item = Result<Frame<D>, E>>>
where
    S: Stream<Item = Result<D, E>>,
    D: Buf,
{
    stream_body(stream)
}

/// Construct an erased, synchronized body from a concrete body.
pub fn boxed_body<B>(body: B) -> BoxBody
where
    B: Body<Data = Bytes> + Send + Sync + 'static,
    B::Error: Into<BoxError>,
{
    HttpBodyExt::boxed(HttpBodyExt::map_err(body, Into::into))
}

/// Short alias for [`boxed_body`].
pub fn boxed<B>(body: B) -> BoxBody
where
    B: Body<Data = Bytes> + Send + Sync + 'static,
    B::Error: Into<BoxError>,
{
    boxed_body(body)
}

/// Construct an erased body that is `Send` but need not be `Sync`.
pub fn boxed_unsync_body<B>(body: B) -> UnsyncBoxedBody
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<BoxError>,
{
    HttpBodyExt::boxed_unsync(HttpBodyExt::map_err(body, Into::into))
}

/// A stream of chunks read from a `futures-io` asynchronous reader.
///
/// This is intentionally a small adapter rather than a filesystem
/// abstraction. It works with async file types from any runtime that
/// implement [`AsyncRead`].
#[derive(Debug)]
pub struct ReaderStream<R> {
    reader: R,
    chunk_size: usize,
    done: bool,
}

impl<R> ReaderStream<R> {
    /// Create a reader stream using a 16 KiB chunk size.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            chunk_size: 16 * 1024,
            done: false,
        }
    }

    /// Set the maximum size of each emitted chunk.
    pub fn with_chunk_size(mut self, chunk_size: usize) -> Self {
        self.chunk_size = chunk_size.max(1);
        self
    }

    /// Return the wrapped reader.
    pub fn into_inner(self) -> R {
        self.reader
    }
}

impl<R> Stream for ReaderStream<R>
where
    R: AsyncRead + Unpin,
{
    type Item = io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }

        let mut buffer = vec![0; self.chunk_size];
        match Pin::new(&mut self.reader).poll_read(cx, &mut buffer) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(0)) => {
                self.done = true;
                Poll::Ready(None)
            }
            Poll::Ready(Ok(read)) => {
                buffer.truncate(read);
                Poll::Ready(Some(Ok(Bytes::from(buffer))))
            }
            Poll::Ready(Err(error)) => {
                self.done = true;
                Poll::Ready(Some(Err(error)))
            }
        }
    }
}

/// Construct a data-stream body from an asynchronous reader.
pub fn reader_body<R>(reader: R) -> StreamBody<impl Stream<Item = Result<Frame<Bytes>, io::Error>>>
where
    R: AsyncRead + Unpin,
{
    stream_body(ReaderStream::new(reader))
}

/// Alias for [`reader_body`].
pub fn body_from_reader<R>(
    reader: R,
) -> StreamBody<impl Stream<Item = Result<Frame<Bytes>, io::Error>>>
where
    R: AsyncRead + Unpin,
{
    reader_body(reader)
}

pin_project! {
    /// A body wrapper that fails after a period with no body frame.
    ///
    /// The timer belongs to this logical body, not to the connection carrying
    /// it. Every successful data or trailers frame resets the timer, so one
    /// active H2 stream cannot affect any other stream on the connection.
    #[derive(Debug)]
    pub struct IdleTimeoutBody<B> {
        #[pin]
        inner: B,
        timer: Timer,
        idle: Duration,
        done: bool,
    }
}

impl<B> IdleTimeoutBody<B> {
    /// Wrap a body with a maximum permitted idle period.
    pub fn new(inner: B, idle: Duration) -> Self {
        Self {
            inner,
            timer: Timer::after(idle),
            idle,
            done: false,
        }
    }

    /// Return the configured idle period.
    pub fn idle_timeout(&self) -> Duration {
        self.idle
    }

    /// Return the wrapped body.
    pub fn into_inner(self) -> B {
        self.inner
    }
}

/// An error returned by [`IdleTimeoutBody`].
#[derive(Debug)]
pub enum IdleTimeoutError<E> {
    /// The wrapped body returned an error.
    Body(E),
    /// No body frame arrived during the configured idle period.
    TimedOut { idle: Duration },
}

impl<E> IdleTimeoutError<E> {
    /// Returns `true` if this is the idle-timeout case.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::TimedOut { .. })
    }

    /// Borrow the wrapped body error, if present.
    pub fn body_error(&self) -> Option<&E> {
        match self {
            Self::Body(error) => Some(error),
            Self::TimedOut { .. } => None,
        }
    }
}

impl<E: fmt::Display> fmt::Display for IdleTimeoutError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Body(error) => write!(formatter, "body error: {error}"),
            Self::TimedOut { idle } => {
                write!(formatter, "body idle timeout after {} ms", idle.as_millis())
            }
        }
    }
}

impl<E: StdError + 'static> StdError for IdleTimeoutError<E> {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Body(error) => Some(error),
            Self::TimedOut { .. } => None,
        }
    }
}

impl<B> Body for IdleTimeoutBody<B>
where
    B: Body,
{
    type Data = B::Data;
    type Error = IdleTimeoutError<B::Error>;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();
        if *this.done {
            return Poll::Ready(None);
        }

        // Give an already-ready body frame precedence over a timer that fires
        // in this same poll. Receiving a frame is activity and resets idle.
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                this.timer.set_after(*this.idle);
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                *this.done = true;
                Poll::Ready(Some(Err(IdleTimeoutError::Body(error))))
            }
            Poll::Ready(None) => {
                *this.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => match Pin::new(&mut *this.timer).poll(cx) {
                Poll::Ready(_) => {
                    *this.done = true;
                    Poll::Ready(Some(Err(IdleTimeoutError::TimedOut { idle: *this.idle })))
                }
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        self.done || self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// Add a logical-body idle timeout to any `http-body` body.
pub trait IdleTimeoutExt: Body + Sized {
    /// Wrap this body so no gap between successful frames exceeds `idle`.
    fn with_idle_timeout(self, idle: Duration) -> IdleTimeoutBody<Self> {
        IdleTimeoutBody::new(self, idle)
    }
}

impl<B> IdleTimeoutExt for B where B: Body + Sized {}

/// The reason a bounded body collection failed.
#[derive(Debug)]
pub enum BodyCollectionError<E> {
    /// The body itself failed while being read.
    Body(E),
    /// The body exceeded the configured limit.
    LimitExceeded(BodyLimitError),
    /// The collected bytes were not valid UTF-8.
    InvalidUtf8(std::string::FromUtf8Error),
    #[cfg(feature = "json")]
    /// JSON deserialization failed after the body was collected.
    Json(miniserde::Error),
}

/// The clear, structured error returned when a collection exceeds its cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyLimitError {
    limit: usize,
    received: usize,
}

impl BodyLimitError {
    /// The configured maximum body size.
    pub fn limit(self) -> usize {
        self.limit
    }

    /// The number of bytes observed when the limit was crossed.
    pub fn received(self) -> usize {
        self.received
    }
}

impl fmt::Display for BodyLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "body exceeded limit of {} bytes (received at least {} bytes)",
            self.limit, self.received
        )
    }
}

impl StdError for BodyLimitError {}

impl<E> BodyCollectionError<E> {
    /// Returns `true` if the collection stopped because of its configured cap.
    pub fn is_limit_exceeded(&self) -> bool {
        matches!(self, Self::LimitExceeded(_))
    }

    /// Alias for [`is_limit_exceeded`](Self::is_limit_exceeded).
    pub fn is_body_limit(&self) -> bool {
        self.is_limit_exceeded()
    }

    /// Borrow the structured limit error, if present.
    pub fn limit_error(&self) -> Option<&BodyLimitError> {
        match self {
            Self::LimitExceeded(error) => Some(error),
            _ => None,
        }
    }
}

impl<E: fmt::Display> fmt::Display for BodyCollectionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Body(error) => write!(formatter, "body error: {error}"),
            Self::LimitExceeded(error) => error.fmt(formatter),
            Self::InvalidUtf8(error) => write!(formatter, "body was not valid UTF-8: {error}"),
            #[cfg(feature = "json")]
            Self::Json(error) => write!(formatter, "invalid JSON body: {error}"),
        }
    }
}

impl<E: StdError + 'static> StdError for BodyCollectionError<E> {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Body(error) => Some(error),
            Self::LimitExceeded(error) => Some(error),
            Self::InvalidUtf8(error) => Some(error),
            #[cfg(feature = "json")]
            Self::Json(error) => Some(error),
        }
    }
}

/// Collect a body incrementally while enforcing `limit` before allocating
/// beyond it. Trailers are consumed and discarded, as a bytes-only result has
/// no place to expose them.
pub async fn collect_bytes_limited<B>(
    body: B,
    limit: usize,
) -> Result<Bytes, BodyCollectionError<B::Error>>
where
    B: Body,
{
    let initial_capacity = body
        .size_hint()
        .upper()
        .and_then(|upper| usize::try_from(upper).ok())
        .map_or(0, |upper| upper.min(limit));
    let mut output = BytesMut::with_capacity(initial_capacity);
    let mut received = 0usize;

    let mut body = Box::pin(body);
    while let Some(frame) = poll_fn(|cx| body.as_mut().poll_frame(cx)).await {
        let frame = frame.map_err(BodyCollectionError::Body)?;
        if let Ok(mut data) = frame.into_data() {
            let frame_len = data.remaining();
            let next = received.saturating_add(frame_len);
            if next > limit {
                return Err(BodyCollectionError::LimitExceeded(BodyLimitError {
                    limit,
                    received: next,
                }));
            }
            output.put(&mut data);
            received = next;
        }
    }

    Ok(output.freeze())
}

/// A response extension for bounded bytes, text, and (with `json`) JSON
/// collection. The entire response head remains available to the caller.
pub trait ResponseBodyExt: Sized {
    type BodyError;

    /// Collect response data while enforcing `limit`.
    fn bytes_limited(
        self,
        limit: usize,
    ) -> impl Future<Output = Result<Bytes, BodyCollectionError<Self::BodyError>>>;

    /// Collect response data and decode it as UTF-8 while enforcing `limit`.
    fn text_limited(
        self,
        limit: usize,
    ) -> impl Future<Output = Result<String, BodyCollectionError<Self::BodyError>>>;

    #[cfg(feature = "json")]
    /// Collect response data and deserialize JSON while enforcing `limit`.
    fn json_limited<T: miniserde::Deserialize>(
        self,
        limit: usize,
    ) -> impl Future<Output = Result<T, BodyCollectionError<Self::BodyError>>>;
}

impl<B> ResponseBodyExt for Response<B>
where
    B: Body,
{
    type BodyError = B::Error;

    async fn bytes_limited(
        self,
        limit: usize,
    ) -> Result<Bytes, BodyCollectionError<Self::BodyError>> {
        collect_bytes_limited(self.into_body(), limit).await
    }

    async fn text_limited(
        self,
        limit: usize,
    ) -> Result<String, BodyCollectionError<Self::BodyError>> {
        let bytes = collect_bytes_limited(self.into_body(), limit).await?;
        String::from_utf8(bytes.to_vec()).map_err(BodyCollectionError::InvalidUtf8)
    }

    #[cfg(feature = "json")]
    async fn json_limited<T: miniserde::Deserialize>(
        self,
        limit: usize,
    ) -> Result<T, BodyCollectionError<Self::BodyError>> {
        let bytes = collect_bytes_limited(self.into_body(), limit).await?;
        let text =
            std::str::from_utf8(&bytes).map_err(|_| BodyCollectionError::Json(miniserde::Error))?;
        miniserde::json::from_str(text).map_err(BodyCollectionError::Json)
    }
}

/// Alias for [`ResponseBodyExt`] for callers who prefer a shorter import.
pub use ResponseBodyExt as ResponseExt;

/// Construct an `Authorization: Bearer ...` header value.
pub fn bearer(token: impl AsRef<str>) -> Result<HeaderValue, http::header::InvalidHeaderValue> {
    HeaderValue::try_from(format!("Bearer {}", token.as_ref()))
}

/// A small callable factory that creates a fresh body (or a fallible body
/// result) on every invocation. It carries no retry policy.
#[derive(Debug)]
pub struct BodyFactory<F> {
    factory: F,
}

impl<F> BodyFactory<F> {
    /// Wrap a closure or other `Fn` that can produce a fresh body.
    pub fn new(factory: F) -> Self {
        Self { factory }
    }

    /// Borrow the underlying factory.
    pub fn as_inner(&self) -> &F {
        &self.factory
    }
}

impl<F: Clone> Clone for BodyFactory<F> {
    fn clone(&self) -> Self {
        Self::new(self.factory.clone())
    }
}

impl<F, O> BodyFactory<F>
where
    F: Fn() -> O,
{
    /// Create a fresh body by invoking the factory.
    pub fn make(&self) -> O {
        (self.factory)()
    }
}

/// A request head paired with an explicit fresh-body factory.
///
/// Calling [`ReplayableRequest::build`] clones only the request head and
/// invokes the factory for a new body. This makes application-controlled
/// retries possible without silently adding retry policy or buffering a
/// one-shot stream.
#[derive(Debug)]
pub struct ReplayableRequest<F> {
    template: Request<()>,
    body: BodyFactory<F>,
}

impl<F> ReplayableRequest<F> {
    /// Create a replayable request from a body-free template and factory.
    pub fn new(template: Request<()>, factory: F) -> Self {
        Self {
            template,
            body: BodyFactory::new(factory),
        }
    }

    /// Retain the head of an existing request and replace its body with a
    /// factory. The existing body is dropped immediately.
    pub fn from_request<B>(request: Request<B>, factory: F) -> Self {
        let (parts, _) = request.into_parts();
        Self::new(Request::from_parts(parts, ()), factory)
    }

    /// Borrow the body-free request template.
    pub fn template(&self) -> &Request<()> {
        &self.template
    }

    /// Borrow the explicit body factory.
    pub fn body_factory(&self) -> &BodyFactory<F> {
        &self.body
    }
}

impl<F> Clone for ReplayableRequest<F>
where
    F: Clone,
{
    fn clone(&self) -> Self {
        Self {
            template: self.template.clone(),
            body: self.body.clone(),
        }
    }
}

impl<F, O> ReplayableRequest<F>
where
    F: Fn() -> O,
{
    /// Build a request with a newly generated body.
    pub fn build(&self) -> Request<O> {
        Request::from_parts(self.template.clone().into_parts().0, self.body.make())
    }

    /// Alias for [`build`](Self::build).
    pub fn request(&self) -> Request<O> {
        self.build()
    }
}

/// A descriptive alias for a replayable request factory.
pub type RequestFactory<F> = ReplayableRequest<F>;

/// Descriptive alias for a bounded collection error.
pub type CollectError<E> = BodyCollectionError<E>;

/// Descriptive alias for the structured body-size limit error.
pub type BodyLimitExceeded = BodyLimitError;

/// Make a factory from a cloneable value, useful for bytes and static JSON
/// bodies that are naturally replayable.
pub fn replayable_value<T: Clone>(value: T) -> BodyFactory<impl Fn() -> T + Clone> {
    BodyFactory::new(move || value.clone())
}

#[cfg(feature = "json")]
mod json {
    use super::{bytes_body, Bytes, Full, HeaderValue, Response};
    use miniserde::Serialize;

    /// Serialize a value as a JSON request body.
    pub fn json_body<T: Serialize>(value: &T) -> Result<Full<Bytes>, miniserde::Error> {
        Ok(bytes_body(miniserde::json::to_string(value)))
    }

    /// Serialize a value as a JSON response and set its media type.
    pub fn json_response<T: Serialize>(
        value: &T,
    ) -> Result<Response<Full<Bytes>>, miniserde::Error> {
        let mut response = Response::new(json_body(value)?);
        response.headers_mut().insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        Ok(response)
    }
}

#[cfg(feature = "json")]
pub use json::{json_body, json_response};

#[cfg(test)]
mod tests {
    use super::*;
    use futures_lite::future::block_on;
    use futures_util::stream;
    use std::convert::Infallible;

    #[test]
    fn constructors_and_data_stream_preserve_chunks() {
        let body = stream_body(stream::iter(vec![
            Ok::<_, Infallible>(Bytes::from_static(b"one")),
            Ok(Bytes::from_static(b"two")),
        ]));
        let chunks = block_on(body.into_data_stream().collect::<Vec<_>>());
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].as_ref().unwrap().as_ref(), b"one" as &[u8]);
        assert_eq!(chunks[1].as_ref().unwrap().as_ref(), b"two" as &[u8]);
    }

    #[test]
    fn bounded_collection_consumes_trailers_and_rejects_over_limit() {
        let mut trailers = http::HeaderMap::new();
        trailers.insert("x-checksum", HeaderValue::from_static("ok"));
        let body = frame_stream_body(stream::iter(vec![
            Ok::<_, Infallible>(Frame::data(Bytes::from_static(b"hello"))),
            Ok(Frame::trailers(trailers)),
        ]));
        let response = Response::new(body);
        let bytes = block_on(response.bytes_limited(5)).unwrap();
        assert_eq!(bytes, Bytes::from_static(b"hello"));

        let response = Response::new(bytes_body("too long"));
        let error = block_on(response.bytes_limited(3)).unwrap_err();
        assert_eq!(error.limit_error().unwrap().limit(), 3);
        assert!(error.is_limit_exceeded());
    }

    #[test]
    fn bearer_and_replayable_request_are_explicit() {
        assert_eq!(bearer("token").unwrap(), "Bearer token");
        let template = Request::get("https://example.test/data").body(()).unwrap();
        let replayable = ReplayableRequest::new(template, || bytes_body("body"));
        assert_eq!(replayable.build().body().size_hint().exact(), Some(4));
        assert_eq!(replayable.build().body().size_hint().exact(), Some(4));
    }

    #[test]
    fn idle_timeout_resets_after_each_frame() {
        let body = frame_stream_body(stream::iter(vec![Ok::<_, Infallible>(Frame::data(
            Bytes::from_static(b"frame"),
        ))]))
        .with_idle_timeout(Duration::from_millis(100));
        let mut data = body.into_data_stream();
        assert_eq!(
            block_on(data.next()).unwrap().unwrap(),
            Bytes::from_static(b"frame")
        );
        assert!(block_on(data.next()).is_none());
    }

    #[test]
    fn idle_timeout_reports_a_stalled_body() {
        let body = frame_stream_body(stream::pending::<Result<Frame<Bytes>, Infallible>>())
            .with_idle_timeout(Duration::from_millis(2));
        let mut data = body.into_data_stream();
        let error = block_on(data.next()).unwrap().unwrap_err();
        assert!(error.is_timeout());
    }

    #[cfg(feature = "json")]
    #[test]
    fn json_body_and_bounded_decode_set_media_type() {
        #[derive(miniserde::Deserialize, miniserde::Serialize)]
        struct Payload {
            ok: bool,
        }

        let response = json_response(&Payload { ok: true }).unwrap();
        assert_eq!(
            response.headers()[http::header::CONTENT_TYPE],
            "application/json"
        );
        let decoded: Payload = block_on(response.json_limited(64)).unwrap();
        assert!(decoded.ok);
    }
}
