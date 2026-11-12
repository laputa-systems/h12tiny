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
| Linux and other Unix | System PEM store via `rustls-native-certs`, verified with rustls-webpki and the explicit Graviola provider | Certificate verification reloads changed stores; empty or unreadable stores fail closed |
| macOS | Native Security.framework evaluation via `rustls-platform-verifier` with the explicit Graviola provider, unless `SSL_CERT_FILE` or `SSL_CERT_DIR` selects PEM roots, which take precedence | Platform distrust and constraint decisions are honored as the OS reports them; an explicit file selection reloads on change and fails closed like Unix |

`openssl-probe` only locates the system CA paths. It is not a TLS or
cryptographic backend. Cryptography remains Rustls plus Graviola; the normal
dependency check still rejects `ring`, AWS-LC, OpenSSL, and native-TLS
backends.

A missing, unreadable, empty, or invalid system store fails the handshake
with a TLS error. The clients never silently fall back to embedded roots.

`ClientTlsConfigBuilder::build_with_trust_source()` reports whether explicit
roots won or which system source was used: native platform evaluation, PEM
files, or the platform certificate store. Direct users of
`h12tiny-tls-system` can classify failures with `SystemTrustError::kind()` and
distinguish a refresh from a first load with `is_reload()`. The async client
also records terminal request failures as `DebugEvent::ConnectionFailed` with
the error kind; like other debug events, it is a best-effort inspection hook,
not a metrics framework.

Trust sources have explicit authority. The designated certificate file
(`SSL_CERT_FILE` when set, otherwise the probed default) must parse to at
least one usable root: every `CERTIFICATE` block must base64-decode and
admission-test as a CA certificate, and the first failure rejects the whole
load. Discovered certificate directories are supplementary: entry failures
there are tolerated so one bad file cannot veto an otherwise valid store.
An empty aggregate store still fails closed.

Change detection compares path, modification time, and length plus, on
Unix, device, inode, and change time. The inode and change time close two
gaps in mtime/length comparison: atomic replacement (`rename` over the
bundle path) and symlink retargeting that preserve both, including
modification times forged with `utimensat` (change time cannot be forged).
For symlinks the link itself is fingerprinted, so retargeting is detected
even when the new target matches in size and mtime. Non-Unix platforms keep
the portable mtime/length comparison; that limitation is documented here
rather than papered over.

## Trust freshness

On a full handshake that invokes certificate verification, the PEM verifier
stats the selected store and reloads changed files. A failed reload fails
verification. There is no background polling thread. macOS native evaluation
consults the OS when verification runs instead of maintaining a PEM snapshot.

Managed configurations currently retain Rustls's default session-resumption
cache. Resumption can reuse earlier certificate approval without invoking the
verifier, so a new transport connection does not guarantee refreshed trust.
Established pooled connections also keep serving. Full certificate-verifying
handshakes use current trust; resumption freshness is deferred work in
[the backlog](../TODO.md).

`Client::new()` and `Connector::new()` load lazily so their constructors
stay infallible: the first handshake reports an unusable store as a TLS
error. `ClientTlsConfigBuilder::build()` loads eagerly and returns the
failure instead, which is the fail-fast path for callers that need it.

## Custom trust

The async and blocking client configuration builders start with system
trust. Callers can:

- Replace the store with `root_certificates(...)`. Replacement never merges
  with system roots.
- Snapshot system trust plus one anchor with `add_root_certificate(...)`.
  The snapshot is taken by that call while system trust is selected, before
  `build()`; later system changes do not affect it.
- Replace the whole Rustls configuration with
  `Builder::tls_config(...)` or `ConnectorBuilder::tls_config(...)`,
  including a private verifier. Custom configurations remain the caller's
  responsibility.
- Select an explicit provider with `with_provider(...)` or
  `Connector::with_tls_provider(...)` while keeping system trust. Neither
  installs nor reads Rustls' process-global provider.

`Protocol::Http1Only` restricts the client builder's ALPN to HTTP/1.1,
including a caller-built TLS configuration. `Protocol::Http2Only` requires h2
from the configured offerings rather than replacing them. Selecting
`Protocol::Auto` removes the HTTP/1.1 restriction; other custom ALPN
offerings remain explicit caller policy.

## Distrusted intermediates

Removing a root does not reject an intermediate that chains to another trusted
anchor. The workspace requires `rustls-webpki 0.103.16` or later, whose verifier
rejects the distrusted intermediate public keys covered by the
[system-trust fixtures](../tests/support/tls_system.rs). Host-store updates
and verifier updates remain separate requirements.

## Server side

Server applications provide their own Rustls server configuration and
`TlsAcceptor`; advertise `h2` and `http/1.1` when serving both protocols.
Select a crypto provider per Rustls configuration. h12tiny does not install or
consult a process-global provider.

Rustls certificate validation remains part of the TLS stack.
See [dependency policy](architecture.md#dependency-constraints).
