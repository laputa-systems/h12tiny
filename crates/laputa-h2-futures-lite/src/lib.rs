#![allow(unused)]
//! An asynchronous, HTTP/2 server and client implementation.
//!
//! This library implements the [HTTP/2] specification. The implementation is
//! asynchronous, using [futures] as the basis for the API. The implementation
//! is also decoupled from TCP or TLS details. The user must handle ALPN and
//! HTTP/1.1 upgrades themselves.
//!
//! # Getting started
//!
//! Add the following to your `Cargo.toml` file:
//!
//! ```toml
//! [dependencies]
//! h2 = "0.4"
//! ```
//!
//! # Layout
//!
//! The crate is split into [`client`] and [`server`] modules. Types that are
//! common to both clients and servers are located at the root of the crate.
//!
//! See module level documentation for more details on how to use `h2`.
//!
//! # Handshake
//!
//! Both the client and the server require a connection to already be in a state
//! ready to start the HTTP/2 handshake. This library does not provide
//! facilities to do this.
//!
//! There are three ways to reach an appropriate state to start the HTTP/2
//! handshake.
//!
//! * Opening an HTTP/1.1 connection and performing an [upgrade].
//! * Opening a connection with TLS and use ALPN to negotiate the protocol.
//! * Open a connection with prior knowledge, i.e. both the client and the
//!   server assume that the connection is immediately ready to start the
//!   HTTP/2 handshake once opened.
//!
//! Once the connection is ready to start the HTTP/2 handshake, it can be
//! passed to [`server::handshake`] or [`client::handshake`]. At this point, the
//! library will start the handshake process, which consists of:
//!
//! * The client sends the connection preface (a predefined sequence of 24
//!   octets).
//! * Both the client and the server sending a SETTINGS frame.
//!
//! See the [Starting HTTP/2] in the specification for more details.
//!
//! # Flow control
//!
//! [Flow control] is a fundamental feature of HTTP/2. The `h2` library
//! exposes flow control to the user.
//!
//! An HTTP/2 client or server may not send unlimited data to the peer. When a
//! stream is initiated, both the client and the server are provided with an
//! initial window size for that stream.  A window size is the number of bytes
//! the endpoint can send to the peer. At any point in time, the peer may
//! increase this window size by sending a `WINDOW_UPDATE` frame. Once a client
//! or server has sent data filling the window for a stream, no further data may
//! be sent on that stream until the peer increases the window.
//!
//! There is also a **connection level** window governing data sent across all
//! streams.
//!
//! Managing flow control for inbound data is done through [`FlowControl`].
//! Managing flow control for outbound data is done through [`SendStream`]. See
//! the struct level documentation for those two types for more details.
//!
//! [HTTP/2]: https://http2.github.io/
//! [futures]: https://docs.rs/futures/
//! [`client`]: client/index.html
//! [`server`]: server/index.html
//! [Flow control]: http://httpwg.org/specs/rfc7540.html#FlowControl
//! [`FlowControl`]: struct.FlowControl.html
//! [`SendStream`]: struct.SendStream.html
//! [Starting HTTP/2]: http://httpwg.org/specs/rfc7540.html#starting
//! [upgrade]: https://developer.mozilla.org/en-US/docs/Web/HTTP/Protocol_upgrade_mechanism
//! [`server::handshake`]: server/fn.handshake.html
//! [`client::handshake`]: client/fn.handshake.html

#![deny(
    missing_debug_implementations,
    missing_docs,
    clippy::missing_safety_doc,
    clippy::undocumented_unsafe_blocks
)]
#![allow(clippy::type_complexity, clippy::manual_range_contains)]
// uncomment me to run benchmarks
//#![cfg_attr(test, feature(test))]
#![cfg_attr(test, deny(warnings))]

// Keep the existing `tracing::...` call sites working when instrumentation is
// disabled. With the feature enabled, these paths resolve directly to the
// optional dependency; otherwise, this crate acts as a small no-op substitute.
#[cfg(not(feature = "tracing"))]
extern crate self as tracing;

#[cfg(not(feature = "tracing"))]
#[macro_export]
#[doc(hidden)]
macro_rules! trace {
    ($($arg:tt)*) => {{}};
}

#[cfg(not(feature = "tracing"))]
#[macro_export]
#[doc(hidden)]
macro_rules! debug {
    ($($arg:tt)*) => {{}};
}

#[cfg(not(feature = "tracing"))]
#[macro_export]
#[doc(hidden)]
macro_rules! warn {
    ($($arg:tt)*) => {{}};
}

#[cfg(not(feature = "tracing"))]
#[macro_export]
#[doc(hidden)]
macro_rules! trace_span {
    ($($arg:tt)*) => {
        $crate::Span::none()
    };
}

#[cfg(not(feature = "tracing"))]
#[macro_export]
#[doc(hidden)]
macro_rules! debug_span {
    ($($arg:tt)*) => {
        $crate::Span::none()
    };
}

#[cfg(not(feature = "tracing"))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Span;

#[cfg(not(feature = "tracing"))]
impl Span {
    pub(crate) fn current() -> Self {
        Self
    }

    pub(crate) fn none() -> Self {
        Self
    }

    pub(crate) fn enter(&self) -> Entered {
        Entered
    }

    pub(crate) fn follows_from(&self, _span: Self) {}

    pub(crate) fn in_scope<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        f()
    }
}

#[cfg(not(feature = "tracing"))]
pub(crate) struct Entered;

#[cfg(not(feature = "tracing"))]
pub(crate) mod instrument {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    #[repr(transparent)]
    pub(crate) struct Instrumented<T>(pub(crate) T);

    pub(crate) trait Instrument: Sized {
        fn instrument(self, _span: super::Span) -> Instrumented<Self> {
            Instrumented(self)
        }
    }

    impl<T> Instrument for T {}

    impl<T: Future> Future for Instrumented<T> {
        type Output = T::Output;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            // SAFETY: `Instrumented<T>` is a transparent wrapper around `T`,
            // so pinning the wrapper also pins its inner future.
            unsafe { self.map_unchecked_mut(|instrumented| &mut instrumented.0) }.poll(cx)
        }
    }
}

#[cfg(not(feature = "tracing"))]
pub(crate) use instrument::{Instrument, Instrumented};

macro_rules! proto_err {
    (conn: $($msg:tt)+) => {
        tracing::debug!("connection error PROTOCOL_ERROR -- {};", format_args!($($msg)+))
    };
    (stream: $($msg:tt)+) => {
        tracing::debug!("stream error PROTOCOL_ERROR -- {};", format_args!($($msg)+))
    };
}

macro_rules! ready {
    ($e:expr) => {
        match $e {
            ::std::task::Poll::Ready(r) => r,
            ::std::task::Poll::Pending => return ::std::task::Poll::Pending,
        }
    };
}

#[cfg_attr(feature = "unstable", allow(missing_docs))]
mod codec;
mod error;
mod futures_codec;
mod hpack;

#[cfg(not(feature = "unstable"))]
mod proto;

#[cfg(feature = "unstable")]
#[allow(missing_docs)]
pub mod proto;

#[cfg(not(feature = "unstable"))]
mod frame;

#[cfg(feature = "unstable")]
#[allow(missing_docs)]
pub mod frame;

pub mod client;
pub mod ext;
pub mod server;
mod share;

#[cfg(fuzzing)]
#[cfg_attr(feature = "unstable", allow(missing_docs))]
pub mod fuzz_bridge;

pub use crate::error::{Error, Reason};
pub use crate::share::{FlowControl, Ping, PingPong, Pong, RecvStream, SendStream, StreamId};

#[cfg(feature = "unstable")]
pub use codec::{Codec, SendError, UserError};

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Creates a future from a function that returns `Poll`.
fn poll_fn<T, F: FnMut(&mut Context<'_>) -> T>(f: F) -> PollFn<F> {
    PollFn(f)
}

/// The future created by `poll_fn`.
struct PollFn<F>(F);

impl<F> Unpin for PollFn<F> {}

impl<T, F: FnMut(&mut Context<'_>) -> Poll<T>> Future for PollFn<F> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        (self.0)(cx)
    }
}
