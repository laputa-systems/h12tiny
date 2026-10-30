# h12tiny-client

Direct-origin HTTP client for h12tiny. It accepts absolute `http` and `https`
URIs and owns origin routing, TLS and ALPN, per-origin pooling, and request
normalization. Hyper owns all HTTP/1 and HTTP/2 protocol machinery.

## Features

No feature is enabled by default.

- `http1`: HTTP/1.1 connections.
- `http2`: HTTP/2 connections.
- `tls`: HTTPS with Rustls and operating-system trust from `h12tiny-tls-system`.

Most applications depend on the `h12tiny` facade with its `client` feature
instead of this crate. See the [client guide](../../docs/client.md) for pooling
and request behavior.
