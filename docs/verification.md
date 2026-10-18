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
- `sh scripts/check-msrv.sh` checks the h12tiny component MSRVs listed in that
  script across their supported feature selections.
- H2: `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml --lib`.
- Hyper: `cargo check --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full`.
- fastwebsockets: run `cargo test --all-features` from `crates/laputa-fastwebsockets-futures-lite`.

The integration tests cover adversarial H1 framing, H2 lifecycle and
cancellation, cleartext and TLS protocol selection, client compatibility,
Unix listener lifecycle, raw upgrades, and WebSocket interop. Extend the
nearest fixture when changing one of those contracts.

CI and `make test` run `cargo test --locked --workspace --all-features`.
CI also checks workspace Clippy, feature isolation, and the full normal
dependency policy on macOS and Linux. A separate Linux job checks the
declared minimum compiler versions. The fork packages are excluded from the
h12tiny Cargo workspace, so run their commands above when changing fork code.

The pinned nightly is the development toolchain; see the
[current compiler requirements](../README.md#add-it) for consumers.

`make lint` checks workspace Clippy with all targets and features.
`make publish` bumps h12tiny workspace minor versions, commits and tags the
release, and publishes h12tiny crates in dependency order. It does not publish
the fork packages. It can resume a partial publication while HEAD remains the
matching release commit. Run it only when releasing, with `TMPDIR` set to the
repository's `.work/tmp` for its backup files. See the
[upstream rebase guide](../.agents/skills/rebase-upstreams/SKILL.md) and the
[fork publish guide](../.agents/skills/publish-forks/SKILL.md) for fork releases.

External interoperability and performance runs require separate tools and
endpoint setup; see the [scripts guide](../scripts/README.md). Performance
measurements are not correctness checks.
