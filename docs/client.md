# Clients

Both clients make direct-origin requests from absolute `http` or `https`
URIs. They share request normalization, but have separate transport contracts.

## Futures-I/O client

`h12tiny-client` uses the configured executor for connection drivers. Its
default connector performs DNS, TCP, Rustls, and ALPN; a custom `Dialer`
owns transport selection. The client normalizes Host and HTTP/1 request
targets and uses the `http` URI types without a URL stack.

Pool identity is canonical scheme plus authority. HTTP/2 connections are not
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

Cancellation must leave pool waiters and later requests viable. Dropping a
request future closes an affected H1 session or resets the affected H2 stream;
pool drop guards release abandoned establishment state.

## Blocking client

`h12tiny-client-sync` opens one HTTP/1.1 connection per request, sends
`Connection: close`, and returns a `ResponseBody` that owns that connection
and implements `std::io::Read`. Dropping the body closes the transport.
There is no pool, proxy or redirect policy, protocol upgrade, or async runtime.

`request_with_timeout` limits request writing and response headers;
`ResponseBody::set_read_timeout` limits individual body reads. Streaming
uploads use `request_streaming` with a declared content length and a
`std::io::Read` body.

## Normalization

Requests require an absolute URI for origin selection. HTTP/1 uses origin-form
targets, except CONNECT which uses authority form. The shared normalizer
preserves explicit Host values or synthesizes Host when enabled, handles root
paths and default ports, and supports IPv4 and bracketed IPv6 authorities.
Neither client adds proxy or URI-to-Unix-socket policy.
