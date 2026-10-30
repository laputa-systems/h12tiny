# h12tiny

Tokio-free Rust HTTP components: a futures-I/O client and server, a separate blocking HTTP/1.1 client, body utilities, and an optional router.

## Commands

- Build: `cargo build --workspace --all-features`
- Test: `cargo test --workspace --all-features`
- Fork checks: see [verification](docs/verification.md)
- Feature and dependency isolation: `scripts/check-features.sh`
- Normal dependency policy: `sh scripts/check-normal-dependencies.sh`
- Minimum compiler versions: `sh scripts/check-msrv.sh`
- Unsafe I/O adapter check (nightly Miri): `scripts/miri-io.sh`

## Invariants

- Facade features default to empty; protocol features only forward to selected roles. See [features](docs/features.md).
- Hyper owns async HTTP framing; the blocking client owns direct-origin HTTP/1.1 only. See [architecture](docs/architecture.md) and [clients](docs/client.md).
- The Hyper, H2, and fastwebsockets fork crates live under `crates/` and resolve through local path dependencies. Their runtime dependencies resolve from crates.io. See [verification](docs/verification.md).
- Pool keys use parsed scheme and authority; H1 is reserved exclusively, H2 is shared, and cross-origin coalescing is absent. See [clients](docs/client.md).
- Production async code is runtime-neutral and does not depend on Tokio. See [architecture](docs/architecture.md).
- Plaintext H2 selection uses the prior-knowledge preface, TLS uses ALPN, and h2c upgrade is unsupported. See [server](docs/server.md).
- Router and body helpers stay protocol-neutral; application policy remains with the application. See [application layer](docs/application.md).

## Topic map

- Crate ownership, dependency direction, upstream boundaries, and feature behavior: [architecture](docs/architecture.md), [features](docs/features.md)
- Async and blocking client behavior: [clients](docs/client.md)
- Server selection, listeners, and upgrades: [server](docs/server.md)
- Bodies, routing, and application conveniences: [application layer](docs/application.md)
- Rustls defaults and custom configuration: [TLS configuration](docs/tls.md)
- Build, tests, feature gates, and coverage: [verification](docs/verification.md)
- External interoperability and performance tools: [scripts guide](scripts/README.md)
