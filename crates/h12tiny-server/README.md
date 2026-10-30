# h12tiny-server

Runtime-neutral HTTP server primitives for h12tiny. Connection policy selects
HTTP/1.1 or HTTP/2 from the plaintext preface or TLS ALPN, and runs the listener
lifecycle. It has no client pool or router.

## Features

No feature is enabled by default.

- `http1`: HTTP/1.1 connections.
- `http2`: HTTP/2 connections.
- `tls`: HTTPS with Rustls.
- `upgrade`: raw HTTP/1 upgrades; implies `http1`.

See the [server guide](../../docs/server.md).
