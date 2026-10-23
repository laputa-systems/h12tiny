# fastwebsockets for futures-I/O

`laputa-fastwebsockets-futures-lite` adapts fastwebsockets to
`futures-lite::io`. It provides split frame readers and writers and a
fragment collector. The crate uses the Apache-2.0 license. See [LICENSE](LICENSE).

```toml
[dependencies]
fastwebsockets = { package = "laputa-fastwebsockets-futures-lite", version = "0.10" }
```

After completing an HTTP WebSocket handshake, pass your stream halves to
`after_handshake_split(read, write, Role::Server)` or `Role::Client`.
`WebSocketRead::read_frame` returns individual frames.
`FragmentCollectorRead::new(reader)` collects fragmented messages.
Both readers take a callback that sends automatic pong and close replies
through the writer. Use `WebSocketWrite::write_frame` and `flush` to send frames.
Canceling a read while it waits for input bytes leaves the partial frame buffered
for the next read.
Client writers use OS entropy for masking and return an I/O error if entropy
acquisition fails. Direct `Frame::mask` calls return `std::io::Result<()>`.

This fork has no `upgrade`, `handshake`, or Axum integration features.
For an HTTP server handshake and upgraded transport, use h12tiny's
[`websocket` feature](../../docs/application.md#router) and
[interop test](../../tests/websocket_interop.rs).
Applications own subprotocols, authentication, task ownership, and message policy.
Compression is unsupported.

See the [API documentation](https://docs.rs/laputa-fastwebsockets-futures-lite)
for the frame interfaces. The files under `examples/`, `autobahn/`, and
`benches/` are inherited upstream tools. They are not adapted runnable examples
for this fork, and their historical results do not verify this adaptation.
