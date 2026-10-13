# Build and verification

Start with the narrow check for the owning crate or changed boundary. The
workspace checks below are the broader integration gates.

## Commands

- `cargo check --workspace --all-features` checks all crate feature surfaces.
- `cargo test --workspace --all-features` runs unit, transport, lifecycle,
  web, and compatibility tests.
- `scripts/check-features.sh` compiles the component/facade matrix and checks
  required dependency absences.
- `sh scripts/check-normal-dependencies.sh [cargo feature args]` checks only
  the enabled normal dependency graph; with no arguments it checks facade
  `full`. Dev and optional packages in Cargo.lock are not a policy failure.
- `scripts/miri-io.sh` runs the focused unsafe I/O adapter tests under nightly
  Miri.

The integration tests cover adversarial H1 framing, H2 lifecycle and
cancellation, cleartext and TLS protocol selection, client compatibility,
Unix listener lifecycle, raw upgrades, and WebSocket interop. Extend the
nearest fixture when changing one of those contracts.

External interoperability and performance runs require separate tools and
endpoint setup; see the [scripts guide](../scripts/README.md). Performance
measurements are not correctness checks.
