# TLS configuration

TLS is selected with the `tls` feature on the client, server, or blocking
client role. The blocking client uses only synchronous Rustls and remains
HTTP/1.1-only.

## Client trust

Both clients verify HTTPS against the host operating-system trust store by
default. There is no embedded Mozilla root list. The default verifier is the
only layer that may change what a hostname validates against; private roots
and caller-supplied verifiers stay under caller control.

| Platform | Default backend | Distrust handling |
| --- | --- | --- |
| Linux and other Unix | System PEM store via `rustls-native-certs`, verified with rustls-webpki and the explicit Graviola provider | Administrator removals take effect on later handshakes; empty or unreadable stores fail closed |
| macOS | Native Security.framework evaluation via `rustls-platform-verifier` with the explicit Graviola provider | Platform distrust and constraint decisions are honored as the OS reports them |

`openssl-probe` only locates the system CA paths. It is not a TLS or
cryptographic backend. Cryptography remains Rustls plus Graviola; the normal
dependency check still rejects `ring`, AWS-LC, OpenSSL, and native-TLS
backends.

A missing, unreadable, empty, or invalid system store fails the handshake
with a TLS error. The clients never silently fall back to embedded roots.

## Trust freshness

The default verifier reloads the system store when its files change and
re-verifies on later handshakes. A failed reload fails the handshake rather
than reusing stale trust. Requests already admitted may complete; new
handshakes observe the refreshed store. There is no background polling
thread; each handshake stats the store paths and only reparses certificates
when they changed.

`Client::new()` and `Connector::new()` load lazily so their constructors
stay infallible: the first handshake reports an unusable store as a TLS
error. `ClientTlsConfigBuilder::build()` loads eagerly and returns the
failure instead, which is the fail-fast path for callers that need it.

The async client's pooled connections are unaffected: established sessions
keep serving, while new handshakes verify against current trust.

## Custom trust

The async and blocking client configuration builders start with system
trust. Callers can:

- Replace the store with `root_certificates(...)`. Replacement never merges
  with system roots.
- Snapshot system trust plus one anchor with `add_root_certificate(...)`.
  The snapshot boundary is the build; later system changes do not affect it.
- Replace the whole Rustls configuration with
  `Builder::tls_config(...)` or `ConnectorBuilder::tls_config(...)`,
  including a private verifier. Custom configurations remain the caller's
  responsibility.
- Select an explicit provider with `with_provider(...)` or
  `Connector::with_tls_provider(...)` while keeping system trust. Neither
  installs nor reads Rustls' process-global provider.

`http1_only()` restricts the client builder's ALPN to HTTP/1.1, including a
caller-built TLS configuration. Other custom ALPN offerings remain explicit
caller policy.

## Why verifier-level mitigation matters

The October 2026 eMudhra incident exposed recoverable intermediate private
keys, some cross-signed under other trusted roots. Removing a root is not
sufficient when a compromised intermediate can still chain to an alternative
trusted anchor. The minimum patched verifier is `rustls-webpki 0.103.16`,
which rejects paths containing the distrusted intermediate public keys even
when another trusted root would otherwise validate them. h12tiny requires at
least that version, so updating the host store and updating the verifier are
independent layers and both are enforced.

## Server side

Server applications provide their own Rustls server configuration and
`TlsAcceptor`; advertise `h2` and `http/1.1` when serving both protocols.
Select a crypto provider per Rustls configuration. h12tiny does not install or
consult a process-global provider.

Rustls certificate validation remains part of the TLS stack.
See [dependency policy](architecture.md#dependency-constraints).
