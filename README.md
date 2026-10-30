# h12tiny

Tokio-free HTTP components for Rust applications: a futures-I/O HTTP/1.1 and
HTTP/2 client, a blocking HTTP/1.1 client, an HTTP/1.1 and HTTP/2 server, body
helpers, and a small optional router.

## Add it

The facade is version 0.6 and has no default features. Select the roles and
protocols your application needs:

```toml
[dependencies]
h12tiny = { version = "0.6", default-features = false, features = [
  "client", "http1", "tls", "util",
] }
bytes = "1"
http-body-util = "0.1"
smol = "2"
```

The facade, client, server, and web crates require Rust 1.89. Core,
normalization, and body utilities require Rust 1.85. Component
crates can be used directly when you want a smaller dependency graph; see
[feature selection](docs/features.md).

## Make an async request

The client uses an application-supplied executor for its connection tasks. This
example uses `smol`; other executors can be adapted with `FnExecutor`.

```rust,no_run
use bytes::Bytes;
use h12tiny::{client::Client, runtime::FnExecutor};
use http_body_util::Empty;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let executor = FnExecutor::new(|future: h12tiny::runtime::BoxSendFuture| {
        smol::spawn(future).detach();
    });
    let client = Client::builder(executor).build::<Empty<Bytes>>();
    let uri = "https://example.com/".parse()?;
    let response = smol::block_on(client.get(uri))?;
    let body = smol::block_on(h12tiny::util::collect_bytes_limited(
        response.into_body(),
        1_048_576,
    ))?;
    println!("read {} bytes", body.len());
    Ok(())
}
```

For the blocking client, enable `client-sync`; its `ResponseBody` implements
`std::io::Read`. For a server, enable `server` and `http1` and/or `http2`;
add `web` for the optional router. TLS client defaults and custom trust
settings are described in
[TLS configuration](docs/tls.md).

## Choose features

| Use | Facade features |
| --- | --- |
| Async client | `client`, `http1` and/or `http2`; add `tls` for HTTPS |
| Blocking client | `client-sync`; add `tls` for HTTPS |
| Server | `server`, `http1` and/or `http2`; add `tls` for HTTPS |
| Router and extractors | `web`, `http1` and/or `http2` |
| JSON, query, SSE, CORS, raw upgrade, WebSocket | `json`, `query`, `sse`, `cors`, `upgrade`, `websocket` as needed |

Protocol features do not select client or server roles by themselves.
`websocket` enables the HTTP/1.1 upgrade surface. See
[feature selection](docs/features.md) for component-crate and isolation details.

## License

MIT. See [LICENSE](LICENSE).

For source, examples, and support, see the
[h12tiny repository](https://github.com/laputa-systems/h12tiny).
