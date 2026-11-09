# Server and upgrades

The server serves HTTP/1.1 and HTTP/2 connections over caller-provided
futures-I/O streams. Listener lifecycle helpers support async-net TCP and,
where available, Unix listeners.

## Protocol selection

For cleartext auto serving, the server progressively matches the HTTP/2
prior-knowledge preface. A complete match selects H2. A mismatch selects H1
and replays every byte already consumed, including when the request line
diverges early. This is not HTTP/1.1 `Upgrade: h2c` support.

HTTP/1.1 requests with duplicate or conflicting `Content-Length` fields, or
with both `Content-Length` and `Transfer-Encoding`, are rejected with a parse
error before the service is invoked. This is stricter than upstream Hyper,
which treats `Transfer-Encoding` as authoritative and closes the connection;
the stricter rule prevents a peer from choosing a different framing.

For TLS, the caller supplies a Rustls acceptor and server configuration. When
both protocols are enabled, advertise `h2` and `http/1.1`. Negotiated `h2`
selects H2; `http/1.1` or no ALPN selects H1 when H1 is compiled. Unknown or
uncompiled protocol selections error. `serve_tls_connection` respects
`http1_only()` and `http2_only()` builder restrictions: matching selections
serve, and conflicts return an unsupported-protocol error. An H2-only builder
rejects absent ALPN.

## Listener lifecycle

`serve` and `serve_tls` own acceptance and executor-owned connection tasks,
not process signals. The caller supplies a shutdown future. Shutdown stops
accepting, cancels pending TLS handshakes and incomplete HTTP/2 setup,
asks established drivers to drain,
and resolves after those drivers finish. Connection limits and an optional
connection-error hook are available on the lifecycle builders.

`Serve::production` and `TlsServe::production` apply the Internet-facing
preset: 1024 active connections, a 10 s HTTP/1 header-read timeout, and a 30 s
drain deadline. The preset also installs the Hyper timer needed by the header
timeout. Add `on_connection_rejected` to observe streams closed at capacity.
Transient listener accept errors are retried with a short backoff; fatal
accept errors stop the loop.

Draining established streams has no deadline unless `drain_timeout` is set.
`Serve::production` and `TlsServe::production` use 30 s. When the deadline
expires, remaining connection futures are dropped and the lifecycle future
returns `io::ErrorKind::TimedOut`; `None` waits indefinitely.

## Upgrades

The `upgrade` feature exposes raw HTTP/1.1 upgrade plumbing for application
protocols. It remains separate from ordinary routing. The optional web
`websocket` feature validates an RFC 6455 handshake, constructs the 101
response, and adapts the upgraded stream to a futures-lite frame parser.
Message, authentication, session, and keepalive policy remain application
owned. The web `websocket-client` feature is the client role of the same
boundary: `connect_websocket` sends the opening handshake over a stream the
caller dialed, checks the `101` response and `Sec-WebSocket-Accept`, and
returns the same `WebSocketConnection` with masked client frames.
