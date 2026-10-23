# Hyper for futures-I/O

`laputa-hyper-futures-lite` is a low-level HTTP/1 and HTTP/2 client and server
fork. HTTP/2 uses the local `laputa-h2-futures-lite` fork. Production code
does not require Tokio.

```toml
[dependencies]
hyper = { package = "laputa-hyper-futures-lite", version = "=1.12.0-beta.0", features = ["client", "http1"] }
```

Select `client`, `server`, `http1`, and `http2` as needed, or use `full`
for all four. Defaults are empty. Applications supply transports implementing
Hyper's runtime I/O traits and any required executor and timer adapters.
See the [API documentation](https://docs.rs/laputa-hyper-futures-lite).

For futures-I/O adapters, connection establishment, pooling, or listener
lifecycle, use [h12tiny](../../README.md). Its [examples](../../examples/)
exercise this fork with futures-I/O transports.
The inherited Hyper [examples](examples/README.md) and Tokio test harness
require adaptation before use with this fork.

## License

MIT. See [LICENSE](LICENSE).
