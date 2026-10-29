# H2 for futures-I/O

`laputa-h2-futures-lite` is an asynchronous HTTP/2 client and server fork
using `futures_io::AsyncRead` and `AsyncWrite`. Applications supply an
established transport, TLS and ALPN policy, and an executor to drive the
connection future.

```toml
[dependencies]
h2 = { package = "laputa-h2-futures-lite", version = "=0.4.20-beta.1" }
```

Pass a futures-I/O transport to `client::handshake` or `server::handshake`.
The client handshake returns a request sender and a connection future that
must be polled while requests are active. The crate exposes stream and
connection flow control. See the
[API documentation](https://docs.rs/laputa-h2-futures-lite).

The crate owns HTTP/2 framing, HPACK, and stream state. Applications own
TCP establishment and protocol selection. For a client or server with
those policies included, use [h12tiny](../../README.md).

The optional `stream` feature adds futures Stream implementations, and
`tracing` enables instrumentation. `unstable` exposes APIs without compatibility
guarantees.

The examples and integration harness retain upstream Tokio code and require
adaptation to this fork's I/O traits. The supported local checks are listed in
[verification](../../docs/verification.md). Upstream h2spec claims do not
establish conformance for this adaptation.

## License

MIT. See [LICENSE](LICENSE).
