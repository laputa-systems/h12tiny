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

Router body limits and timeouts apply to routes present when configured,
including only methods present in each method router; later additions and the
fallback keep their own settings. Built-in byte and JSON extractors return 413
when a body limit is exceeded; raw request handlers receive a body error and
own its response mapping. Handler deadlines produce 408 while extracting and
constructing the response, and do not cover streaming response bodies.
The fallback handles unmatched paths; a matched path with an unsupported
method returns 405 with an `Allow` header listing the route's registered
methods. Router clones share an immutable route table and matcher; builder
changes to a clone leave the original router's routes intact.

Optional features add JSON and query extraction, SSE event
framing and idle-only comment keepalives, structural CORS, raw upgrade access,
and the narrow WebSocket handshake/frame boundary.

SSE data frames CR, LF, and CRLF as line breaks. `Event::event` and `Event::id`
return `Result<Event, EventFieldError>`; event names reject CR/LF, and identifiers
also reject NUL. Use `?` after each of these setters.

CORS decorates handler and router error responses while preserving existing
`Vary` fields. It adds Origin variation unless already present or covered by
`Vary: *`, including requests without an allowed origin. Preflight handling
requires OPTIONS, Origin, and Access-Control-Request-Method; ordinary OPTIONS
requests reach the route.

The web layer does not add middleware composition, handler reflection,
dependency injection, OpenAPI generation, or application session policy.
Applications own redirects, cookies, authentication, upload policy, and
WebSocket message behavior.
