# Build and verification

Start with the narrow check for the owning crate or changed boundary. The
workspace checks below are the broader integration gates.

## Commands

- `cargo check --workspace --all-features` checks all crate feature surfaces.
- `cargo test --workspace --all-features` runs unit, transport, lifecycle,
  web, and compatibility tests.
- `scripts/check-features.sh` compiles the component/facade matrix, checks
  required dependency absences, and exercises H1-only TLS and H2-only
  cleartext defaults.
- `sh scripts/check-normal-dependencies.sh [cargo feature args]` checks only
  the enabled normal dependency graph; with no arguments it checks facade
  `full`. Dev and optional packages in Cargo.lock are not a policy failure.
- `scripts/miri-io.sh` runs the focused unsafe I/O adapter tests under nightly
  Miri.
- `sh scripts/check-msrv.sh` installs and checks each manifest's declared
  minimum Rust version across its supported feature selections.

The integration tests cover adversarial H1 framing, H2 lifecycle and
cancellation, cleartext and TLS protocol selection, client compatibility,
Unix listener lifecycle, raw upgrades, and WebSocket interop. Extend the
nearest fixture when changing one of those contracts.

CI and `make test` run `cargo test --locked --workspace --all-features`.
CI also checks workspace Clippy, feature isolation, and the full normal
dependency policy on macOS and Linux. A separate Linux job checks the
declared minimum compiler versions.

The pinned nightly is the development toolchain; see the
[current compiler requirements](../README.md#add-it) for consumers.

`make lint` checks workspace Clippy with all targets and features.
`make publish` bumps workspace minor versions, commits and tags the release,
and publishes crates in dependency order. It can resume a partial publication
while HEAD remains the matching release commit. Run it only when releasing,
with `TMPDIR` set to the repository's `.work/tmp` for its backup files.

External interoperability and performance runs require separate tools and
endpoint setup; see the [scripts guide](../scripts/README.md). Performance
measurements are not correctness checks.
