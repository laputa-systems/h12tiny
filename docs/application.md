# Bodies and web applications

## Body utilities

`h12tiny-util` reexports `http-body-util` primitives and adds protocol-neutral
helpers for byte and streaming bodies, bounded collection, idle-body timeouts,
Bearer headers, and explicit replay factories. Bounded collection enforces
its limit while streaming. Replay factories make request-body replay explicit.
The optional `json` feature adds JSON helpers.

## Router

`h12tiny-web` provides a protocol-neutral router with static, parameter, and
catch-all paths; method dispatch; `State`, `Path`, `Extension`, `Bytes`,
raw request and query extraction; and a small `IntoResponse` conversion.
The same service can be used with H1, H2, cleartext auto selection, or TLS
server connections.

Route body limits stop oversized streaming bodies with 413. Handler deadlines
produce 408. Optional features add JSON and query extraction, SSE event
framing and idle-only comment keepalives, structural CORS, raw upgrade access,
and the narrow WebSocket handshake/frame boundary.

The web layer does not add middleware composition, handler reflection,
dependency injection, OpenAPI generation, or application session policy.
Applications own redirects, cookies, authentication, upload policy, and
WebSocket message behavior.
