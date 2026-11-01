# H12T-12: Production bounds and lifecycle policy

Status: accepted

## Summary

The client pool, server accept loop, body collection, and connection drivers currently fail open: unbounded retries, unbounded queues and bodies, silent at-capacity close, and shutdown drain with no deadline. A single misconfigured application gets slow-loris and OOM exposure with no observable signal.

## Motivation

Evidence: `crates/h12tiny-client/src/lib.rs:637` (unbounded `Retry` continue), `:850-877` (detached drivers discard `Result`), `:1081-1085` (pool `usize::MAX` defaults), `crates/h12tiny-server/src/lifecycle.rs:357` (silent at-capacity drop), `:372` (graceful drain with no timeout), `crates/h12tiny-web/src/lib.rs:502` (`unwrap_or(usize::MAX)` body fallback).

The server accept loop also never installs a Hyper timer, so Hyper's default HTTP/1 header-read timeout is inert: a client that never finishes its headers holds a connection indefinitely.

## Design

Keep the existing permissive constructors and add explicit role presets:

- `h12tiny-client`: a `production()` builder preset sets pool limits, retry bounds, and default request deadlines.
- `h12tiny-server`: `Serve::production()` and `TlsServe::production()` install `AsyncIoTimer`, set a header-read timeout, cap connections, report at-capacity rejection, classify transient accept errors, and apply a drain deadline.
- `h12tiny-web`: `Router::production()` applies a default body limit and handler deadline to configured routes, future routes, nested routes, and the fallback.

The presets never replace explicit per-request, per-route, or per-listener settings. Existing constructors retain their current behavior.

Bound retry with a cap plus backoff and safe/idempotent gating, report driver errors through `DebugEvent`, and force-abort a drain that exceeds its deadline. Body timeouts remain application-owned on the raw server; the web preset bounds handler time instead.

## Alternatives considered

- Per-crate ad-hoc limits with no shared preset: keeps autonomy but preserves the current discovery cost and inconsistent tuning.
- Hard-coded low defaults: protects out of the box but breaks high-throughput consumers who currently rely on unlimited pooling.
- Failing builders that omit a preset: guesses deployment context and breaks existing users; an explicit `validate_bounds` remains available for applications that want enforcement.

## Open questions

None. The decisions below settle the former questions.

## Decision

| Question | Decision |
| --- | --- |
| Defaults | Existing constructors stay permissive. The presets are opt-in. |
| Missing preset | Do not warn or fail the ordinary builders. Presets supply valid values; applications that require enforcement call an explicit validation method. |
| Retry | Retry only safe or idempotent methods with an exactly empty body over a reused connection. Default two retries with 10 ms backoff; zero disables. Non-empty and streaming bodies are never replayed. |
| Client preset | `max_connections_per_host = 32`, `max_idle_per_host = 16`, DNS/TCP/TLS 10 s, headers 30 s. Pool idle stays 90 s. |
| Server preset | `max_connections = 1024`, header read timeout 10 s, drain deadline 30 s, at-capacity rejection hook. |
| Web preset | Request body limit 1 MiB, handler deadline 30 s, applied to fallback and future routes. |
| At-capacity signal | A dedicated no-argument `on_connection_rejected` hook, not a synthetic `ConnectionError`. |
| Accept errors | Retry `Interrupted`, `WouldBlock`, `ConnectionAborted`, `ConnectionReset`, and `TimedOut` with a short backoff; abort on all other errors. |
| Drain deadline | On expiry, force-drop active connection futures and return `io::ErrorKind::TimedOut`. |

### Delivery slices

1. Bound client stale-connection retries and report driver failures.
2. Add the client production preset.
3. Add the server production preset without the drain deadline.
4. Bound graceful drain and force-abort on expiry.
5. Add the web router production defaults.
