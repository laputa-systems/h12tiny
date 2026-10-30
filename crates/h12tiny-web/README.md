# h12tiny-web

Small application routing adapter for h12tiny. `Router` owns application state
and dispatches ordinary HTTP requests to async functions. It stops at the `http`
and `http-body` layer; connection drivers and raw upgrade mechanics belong to
`h12tiny-server`.

## Features

No feature is enabled by default.

- `json`: JSON request and response bodies.
- `query`: query-string extraction.
- `sse`: server-sent events.
- `cors`: CORS helpers.
- `upgrade`: raw HTTP/1 upgrades through `h12tiny-server`.
- `websocket`: the RFC 6455 HTTP/1.1 handshake and frame adaptation, without
  application message policy.

See the [application layer guide](../../docs/application.md).
