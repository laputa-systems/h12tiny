# Clients

Both clients require absolute URIs and share request normalization.
The default async connector and blocking client accept `http` and `https`,
verifying HTTPS against the host system trust store (see
[TLS configuration](tls.md)). A custom async `Dialer` receives the origin
URI unchanged and owns scheme and transport policy. The transports have
separate lifecycle contracts.

## Futures-I/O client

`h12tiny-client` uses the configured executor for connection drivers. Its
default connector performs DNS, TCP, Rustls, and ALPN; a custom `Dialer`
owns transport selection. The client normalizes Host and HTTP/1 request
targets and uses the `http` URI types without a URL stack.

Cleartext defaults to HTTP/1.1 when H1 is compiled, and to prior-knowledge H2
in an H2-only build. A dual-protocol client can explicitly select plaintext
H2 with `Protocol::Http2Only`.

`Protocol` is one policy, not a stack of independent flags. Selecting
`Http1Only` restricts TLS ALPN to HTTP/1.1, including a caller-built TLS
configuration. `Http2Only` requires h2 from ALPN but leaves the configured
offerings in place, so a peer that selects anything else fails cleanly as an
ALPN error. Selecting `Auto` later removes the HTTP/1.1 restriction. A custom
`Dialer` owns its own transport negotiation and may ignore the ALPN portion.

Pool identity uses the parsed scheme and authority; an explicit default port
and an omitted port remain distinct keys. HTTP/2 connections are not
coalesced across origins. H1 reservations are exclusive to one request
exchange; reuse also depends on response completion, EOF, upgrade state,
cancellation, and driver state. H2 connections are shared, and concurrent
requests for a new H2 origin share one establishment attempt. If its owner is
dropped or fails, waiters are released so a later attempt can proceed.

`pool_max_connections_per_host` and `pool_max_idle_per_host` take a nonzero
limit or `None` for unlimited; zero is not an unlimited sentinel. Call
`disable_pool` when connections must not be reused.

A failed reused connection retries at most twice by default. Only safe or
idempotent methods with an exactly empty body are retried; a non-empty or
streaming body is never replayed. Call `max_stale_retries(0)` to disable
retries. Terminal failures and driver failures are reported through
`DebugEvent` when a log is configured.

`Builder::production` applies the Internet-facing preset: 32 open and 16 idle
connections per origin, two stale retries, 10 s DNS/TCP/TLS deadlines, and
30 s response-header deadline. The 90 s pool idle timeout is unchanged. Later
builder calls override individual values.

Request deadlines have separate scopes: DNS, TCP establishment, TLS/ALPN, and
dispatch through response headers. They are request-scoped and do not include
streaming response-body time. Pool idle eviction is a separate client setting.
Every configured timeout must be greater than zero: a zero duration expires
before any work can start and is rejected by the builder that accepts it. Do
not replace these scopes with a whole-request deadline that expires active
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
