# Architecture and boundaries

h12tiny owns direct-origin endpoint policy and small application conveniences.
The futures-I/O protocol crates own HTTP framing and protocol state.

## Crates

| Crate | Responsibility | Boundary |
| --- | --- | --- |
| `h12tiny-core` | Futures-I/O/Hyper adapters and runtime-neutral executor/timer adapters | No connector, pool, listener, TLS, or application policy |
| `h12tiny-client` | Async DNS/TCP, TLS/ALPN, request normalization, H1/H2 handshakes, and per-origin pool | No server or routing code |
| `h12tiny-client-normalize` | Direct-origin URI, Host, and HTTP/1 target normalization | No transport or protocol framing |
| `h12tiny-client-sync` | Blocking direct-origin HTTP/1.1 serialization and response framing | No futures, runtime, H2, pool, proxy, or upgrade |
| `h12tiny-tls-system` | Operating-system TLS trust and refresh for both clients | No HTTP framing, pooling, or application policy |
| `h12tiny-server` | H1/H2 connection selection, TLS ALPN dispatch, upgrades, and listener lifecycle | No client pool or router |
| `h12tiny-util` | Protocol-neutral bodies, bounded collection, streaming helpers, and optional JSON | No DNS, TCP, TLS, listeners, or routing |
| `h12tiny-web` | Small router, extractors, response conversion, and optional web conveniences | No protocol implementation or client policy |
| `h12tiny` | Feature-gated component reexports | No endpoint implementation |

The dependency direction is one-way: client and server use core; both client
transports use normalization and the shared system-trust crate; the blocking
client does not use core; web uses util and reaches server only for the
optional upgrade surface. Client crates do not depend on server or web. The
system-trust crate owns OS trust loading and refresh; both clients use it for
their default verifier and never depend on each other's TLS code.

## Protocol and runtime ownership

The async client and server use `laputa-hyper-futures-lite` for HTTP framing.
`laputa-h2-futures-lite` implements parsing, HPACK, flow control, streams,
SETTINGS, GOAWAY, and request dispatch. h12tiny supplies direct-origin
connection policy, normalization, pooling, runtime adaptation, lifecycle, and
server protocol selection.

The repository includes the Laputa Hyper, H2, and fastwebsockets crates under
[`crates/`](../crates/). h12tiny and Hyper use local path dependencies for
those packages. Cargo still uses each package's declared version when it
publishes a crate. The fork packages keep their own manifests and verification
commands.

Production async code uses Hyper's executor boundary and the core timer
adapter; it has no Tokio runtime dependency. The blocking client is independent
of the async stack and uses `std::net` plus optional synchronous Rustls.

## Dependency constraints

The normal dependency graph must stay free of transport stacks and general
framework dependencies such as Tokio, native TLS, Hyper-util, Reqwest, Tower,
Axum, and URL-processing stacks. The full forbidden package set is enforced on
the enabled normal graph by [the dependency check](../scripts/check-normal-dependencies.sh);
Cargo.lock also contains optional and development packages and is not that
check.

Optional serialization remains at the util/web layer. `miniserde` is enabled
only by the JSON or query features; plain body helpers and the plain router do
not select it.

## Upstream-derived code

The async client pool and server auto-selection adapt `hyper-util` 0.1.20 source.
The protocol dependency is `laputa-hyper-futures-lite`. The source headers
identify the upstream sections;
connector and core I/O/runtime boundaries are rewritten for this workspace.
The workspace split, blocking client, listener lifecycle, and application
crates are local extensions. Keep applicable MIT attribution with substantial
upstream-derived code.
