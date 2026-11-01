#![cfg(any(feature = "http1", feature = "http2"))]

//! Server endpoint surface.
//!
//! The connection policy selects HTTP/1.1 or HTTP/2 from plaintext preface
//! detection or TLS ALPN. Raw HTTP/1 upgrades are exposed only with the
//! optional `upgrade` feature.

pub mod conn;

/// Raw HTTP upgrade types and the [`on`](upgrade::on) extraction function.
#[cfg(feature = "upgrade")]
pub mod upgrade;

/// Runtime-neutral listener and graceful-drain helpers.
pub mod lifecycle;

/// TLS acceptor used by [`serve_tls`].
///
/// Reexporting this boundary keeps applications on h12tiny's selected
/// futures-rustls version instead of requiring a second direct dependency just
/// to adapt an already-configured `rustls::ServerConfig`. Applications should
/// build that config with `rustls_graviola::default_provider()`.
#[cfg(feature = "tls")]
pub use futures_rustls::TlsAcceptor;
pub use lifecycle::{serve, ConnectionError, Listener, ProductionBounds};
#[cfg(feature = "tls")]
pub use lifecycle::{serve_tls, TlsServe};
