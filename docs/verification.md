# Build and verification

Start with the narrow check for the owning crate or changed boundary. The
workspace checks below are the broader integration gates.

## Commands

- `cargo check --locked --workspace --all-features` checks all crate feature surfaces.
- `cargo test --locked --workspace --all-features` runs unit, transport, lifecycle,
  web, and compatibility tests.
- `scripts/check-features.sh` compiles the component/facade matrix, checks
  required dependency absences, and exercises H1-only TLS and H2-only
  cleartext defaults.
- `sh scripts/check-normal-dependencies.sh [cargo feature args]` checks only
  the enabled normal dependency graph; with no arguments it checks facade
  `full`. Dev and optional packages in Cargo.lock are not a policy failure.
- `scripts/miri-io.sh` runs the focused unsafe I/O adapter tests under nightly
  Miri.
- `sh scripts/check-msrv.sh` reads every workspace member from `cargo metadata`
  and checks it with that member's declared `rust-version` and all features,
  then keeps the workspace-excluded fork checks.
- H2 unit tests: `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml --lib`.
- H2 flow control: `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml --test flow_control_futures`.
- Hyper: `cargo check --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full`.
- fastwebsockets: run `cargo test --all-features` from `crates/laputa-fastwebsockets-futures-lite`.
- Fork protocol gate: `sh scripts/verify-protocols.sh` runs every fork suite and
  the shared test support crate. Pass `--suite NAME` to run one suite and
  `--check-ledger` to validate `docs/fork-test-parity.md`. CI runs the full gate
  on Linux; the individual commands above are for iterating on one fork, not a
  substitute for the gate.

The concrete client, server, and web timeouts and size limits are listed in
[limits](limits.md).

The integration tests cover adversarial H1 framing, H2 lifecycle and
cancellation, cleartext and TLS protocol selection, system-trust defaults and
refresh, distrusted-intermediate rejection, client compatibility,
Unix listener lifecycle, raw upgrades, and WebSocket interop. Extend the
nearest fixture when changing one of those contracts.

CI and `make test` run `cargo test --locked --workspace --all-features`.
CI also checks workspace Clippy, feature isolation, and the full normal
dependency policy on macOS and Linux. A separate Linux job checks the
declared minimum compiler versions. The fork packages are excluded from the
h12tiny Cargo workspace, so run their commands above when changing fork code.

The manifests select published `laputa-*` runtime versions. The repository's
[Cargo configuration](../.cargo/config.toml) resolves crates.io through the
`stuff` archive. A local smolith checkout is not required for these commands.
The smolith repository separately tests an h12tiny checkout, with
runtime dependencies redirected to local smolith crates, using its `h12tiny.sh`
fixture script.

The [parity ledger](fork-test-parity.md) records the inherited fork tests and
local regressions. The flow-control regression uses futures-I/O and a
wake-driven executor to check `WINDOW_UPDATE` after capacity release and read
the next data frame. The shared support crate includes a missing-wake control.

The pinned nightly is the development toolchain; see the
[current compiler requirements](../README.md#add-it) for consumers.

`make lint` checks workspace Clippy with all targets and features.
`make publish` bumps workspace minor versions, commits and tags the release,
and publishes workspace crates to crates.io in dependency order. It then runs
`publish-mycrates` for the subset selected by `CRATES` in the [Makefile](../Makefile).
The fork packages are separate releases. Check their declared version
requirements in the [manifests](../crates/) before releasing dependents;
packaging drops `path`, so required versions must exist in the registry.
Publish missing runtime dependencies first, then H2, Hyper, and fastwebsockets.

The workspace release checks for a Cargo login and explicitly targets
crates.io outside the repository configuration. It resumes partial publication
while HEAD remains the matching release commit. Set `TMPDIR` to the
repository's `.work/tmp` for backup files. See the
[upstream rebase guide](../.agents/skills/rebase-upstreams/SKILL.md) and the
[fork publish guide](../.agents/skills/publish-forks/SKILL.md) for fork releases.

The [benchmark workspace](../benchmarks/twelve-dollar/Cargo.toml) is an
in-repository path consumer with a separate lockfile. Its current facade
requirement is stale; the resolution defect is in [the backlog](../TODO.md).

External interoperability and performance runs require separate tools and
endpoint setup; see the [scripts guide](../scripts/README.md). Performance
measurements are not correctness checks. The HTTP/1.1, HTTP/2, client-pool,
and router campaign record is in [performance](performance/README.md).
