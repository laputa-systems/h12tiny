# Clients

Both clients make direct-origin requests from absolute `http` or `https`
URIs. They share request normalization, but have separate transport contracts.

## Futures-I/O client

`h12tiny-client` uses the configured executor for connection drivers. Its
default connector performs DNS, TCP, Rustls, and ALPN; a custom `Dialer`
owns transport selection. The client normalizes Host and HTTP/1 request
targets and uses the `http` URI types without a URL stack.

Cleartext defaults to HTTP/1.1 when H1 is compiled, and to prior-knowledge H2
in an H2-only build. A dual-protocol client can explicitly select plaintext
H2 with `http2_only(true)`.

Pool identity uses the parsed scheme and authority; an explicit default port
and an omitted port remain distinct keys. HTTP/2 connections are not
coalesced across origins. H1 reservations are exclusive to one request
exchange; reuse also depends on response completion, EOF, upgrade state,
cancellation, and driver state. H2 connections are shared, and concurrent
requests for a new H2 origin share one establishment attempt. If its owner is
dropped or fails, waiters are released so a later attempt can proceed.

Request deadlines have separate scopes: DNS, TCP establishment, TLS/ALPN, and
dispatch through response headers. They are request-scoped and do not include
streaming response-body time. Pool idle eviction is a separate client setting.
Do not replace these scopes with a whole-request deadline that expires active
body streams.

Dropping a request future closes an affected H1 session or resets the affected
H2 stream; establishment drop guards release abandoned connection attempts.
An H1 slot guard follows each counted socket through establishment, idle
storage, and waiter handoff. Closure, eviction, and cancellation release the
slot once and let requests waiting for capacity retry their dial.

## Blocking client

`h12tiny-client-sync` opens one HTTP/1.1 connection per request, sends
`Connection: close` unless the caller supplies `Connection`, and returns a
`ResponseBody` that owns that connection and implements `std::io::Read`.
Dropping the body closes the transport.
There is no pool, proxy or redirect policy, protocol upgrade, or async runtime.

`request_with_timeout` shares one deadline across socket writes and interim
and final response headers, including socket operations inside Rustls.
Connection setup has its separate `connect_timeout`; the dispatch deadline
is cleared before returning the response body. `ResponseBody::set_read_timeout`
changes the timeout for subsequent network reads even when data is buffered.
`is_complete()` records fixed-length and chunked completion, and real EOF for
close-delimited bodies. Streaming uploads use `request_streaming` with a
declared content length and a `std::io::Read` body; callers own the behavior
of that reader.

Successful CONNECT responses return `UnsupportedUpgrade` because this client
does not expose a tunnel. Non-success CONNECT responses use ordinary HTTP
response framing.

## Normalization

Requests require an absolute URI for origin selection. HTTP/1 uses origin-form
targets, except CONNECT which uses authority form. The shared normalizer
preserves explicit Host values or synthesizes Host when enabled, handles root
paths and default ports, and supports IPv4 and bracketed IPv6 authorities.
Neither client adds proxy or URI-to-Unix-socket policy.

Origin extraction rejects empty, malformed, and out-of-range explicit ports
with `InvalidPort` before either client invokes its connector. Default ports
apply only when no port is supplied.
