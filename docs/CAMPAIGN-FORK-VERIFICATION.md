# Campaign: fork protocol verification

## Status

**Goal:** the Hyper and H2 forks have a futures-I/O test suite that runs locally and in CI, a ledger that classifies every inherited test, a wake-driven executor that fails on lost wakeups, and a deterministic regression for the upstream H2 bidirectional deadlock ([h2#975](https://github.com/hyperium/h2/issues/975)). No protocol change may land while its fork tests are skipped or unported.

**Baseline:** commit `57b545c`, 2026-10-31. Root workspace green; inherited H2 and Hyper suites red before they compile. This campaign starts at `FVP-01`.

**Accepted:** none.

**Red:** `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml --workspace --tests` fails in `h2-support` with 8 errors; `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full --tests` fails across 9 targets with 428 error lines, including 188 in `tests/client.rs`.

**Next:** `FVP-01` (gate skeleton, ledger, CI job).

**Resume:** `sh scripts/verify-protocols.sh`; then continue at the first red or unported row in `docs/fork-test-parity.md`.

## Charter

- **This campaign is H12T-13 only.** It builds the verification foundation and the h2#975 regression. It does not change h12tiny's production API, resource defaults, retry policy, TLS trust, or lifecycle. Those are campaign 2.
- **The reference is upstream h2 0.4.20 and hyper 1.12.0**, the releases the fork changelogs identify, plus the inherited test corpus. Port behavior, not implementation shape. A deliberate difference is recorded in the ledger with its reason and covering test.
- **Parity before optimization.** No performance, size, or release work. The host is busy (load 23.59 on 32 cores), so timings are hang detection and loop budget, not a performance gate. A test that makes no progress within 10 s fails; the gate target is warm <= 2 min and cold <= 10 min.
- **One writer by default.** Parallel lanes are allowed only for the disjoint pairs named in **Sequence**, in worktrees at `.work/wt/<lane>`. The parent owns integration, the ledger, and the gate script. Before adding a lane, run `perf-gate-ready -s`; if it reports the host busy, stay serial.
- **Attempt limit: three per slice.** If the acceptance command is still red, park the branch with the exact failure, the cause, and the retry condition. Red work never lands on the integration branch.
- **Fix egregious problems met on the way**, in their own commit: a test that hangs past 10 s, a gate that runs zero tests, a build break caused by the port, or a test that passes without exercising the behavior. Record anything else and leave it.
- **Commits:** Conventional Commit, scope is the item workstream, item ID in the body. Code comments never cite this document.
- **No `/tmp`.** Worktrees, scratch, target overrides, and `TMPDIR` live under `.work/`; `/.work/` is already excluded through `.git/info/exclude`.

## Decisions

**Owner-decided (2026-10-31):**

- H12T-13 runs before H12T-12. Retry, pool, shutdown, and flow-control behavior change only after the protocol harness is trustworthy.
- Forks stay upstream-aligned. Port the harness and keep the futures-I/O semantic delta small; do not rewrite protocol internals.
- The program is four campaigns. This document authorizes campaign 1 only.

**Prescribed:**

- **H2 first, Hyper second; shared support third.** The H2 flow-control and deadlock work is the highest-risk area, so its harness and regression are proven before the larger Hyper port.
- **h2#975 is a gated finding, not an assumed fix.** Build the deterministic regression first. If it fails on the current fork, port the upstream fix if one exists; otherwise apply the minimal upstream-aligned fix (keep reading while a control-frame write is pending; do not restructure the state machine), bounded to 200 lines and no public API change. If it does not fail after 20 deterministic runs, record `not reproduced` with the exact topology and do not add an ignored gating test. A non-gating 12-run TLS soak under the smolith runtime is corroboration, not the gate.
- **CI runs the full fork gate on Linux and a smoke subset on macOS.** The smoke subset is H2 library tests, the H2 flow-control test, Hyper `--features full` check, and fastwebsockets. A full macOS fork run is a follow-on; it is not required to close this campaign.
- **Exclusions are capped.** At most 10 tests per fork may be excluded, each with a reason and a named covering test in the ledger. No broad `#[ignore]`, no disabling a test to make the gate green. `tests/h1_ready_in_flight.rs` is excluded because it tests Tokio's cooperative budget, not fork behavior; `ready_on_poll_stream` and `h1_flush_before_yield` cover the readiness path. The upstream `#[ignore]` tests stay ignored and are listed with their upstream reason; unignoring one is a separate finding.
- **The Tokio oracle is temporary.** `FVP-02` may run the upstream Tokio harness in a disposable worktree to record reference counts and outcomes. It is reverted before `FVP-03`; the final fork test graph contains no Tokio. Fork examples are out of scope: the gate uses `--tests` and `--lib`, so the Tokio examples are not ported in this campaign.
- **Dependencies come from crates.io `laputa-*` 0.3.0**, not `../smolith` paths. CI has no smolith checkout for fork commands. [`SMOLITH_REV`](../SMOLITH_REV) is not changed.
- **`crates/laputa-protocol-test-support` is dev-only.** `publish = false`; it is not a product crate and must not enter any normal dependency graph.

**Delegated to the executor:**

- Port mechanics per test file (test macro, `block_on`, task spawning), bounded by no production changes and no Tokio in the final test graph.
- Batch order inside an item, provided the item's named acceptance command still runs.
- Port bug versus fork bug: run the reference case first. If the reference passes and the port fails, the port is wrong. If both fail, it is a fork finding; fix it within the 200-line bound or park the test with a finding.
- Exclusion of a specific Tokio-only test within the 10-test cap and the covering-test rule.
- The exact shape of the h2#975 fix inside the stated direction and bound.

**What would reverse a decision:** the reference inventory shows a test's assertion depends on Tokio's cooperative-budget or timer semantics rather than fork behavior; then that test is excluded with a reason, not simulated. If h2#975 cannot be made deterministic after 20 runs, the fix is not attempted. If the gate exceeds 10 minutes warm, split it by fork and optimize before adding more suites; do not drop the per-PR gate.

## Baseline

Measured on 2026-10-31 at commit `57b545c`, Linux x86_64, 32 cores, load 23.59, nightly-2026-09-15. All commands used `TMPDIR=.work/tmp`.

| Command | Result | Wall |
| --- | --- | --- |
| `cargo test --locked --workspace --all-features` | 206 passed, 0 failed, 1 ignored, 54 suites | 29.74 s |
| H2 library: `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml --lib` | 446 passed, 0 failed, 1 ignored | 29.67 s |
| H2 flow: `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml --test flow_control_futures` | 1 passed | 2.73 s |
| H2 inherited: `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml --workspace --tests` | **red**, 8 errors in `h2-support` | 15.71 s |
| H2 full: `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml` | **red**, 3 examples fail on missing Tokio | 0.21 s |
| Hyper check: `cargo check --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full` | green | 8.29 s |
| Hyper tests: `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full --tests` | **red**, 9 targets, 428 error lines; largest is 188 in `tests/client.rs` | 6.88 s |
| fastwebsockets: `cargo test --manifest-path crates/laputa-fastwebsockets-futures-lite/Cargo.toml --locked --all-features` | 37 passed | 9.99 s |
| H2 dogfood under smol: `cargo test --locked --features client,client-sync,server,http1,http2,tls --test dogfood_h2_prior -- --exact h2_client_and_auto_server_interoperate_over_cleartext_prior_knowledge` | 1 passed | 16.39 s |
| H2 cancellation under smol: `cargo test --locked --features client,client-sync,server,http1,http2,tls --test h2_lifecycle -- --exact cancelling_one_h2_response_future_does_not_poison_the_shared_session` | 1 passed | 14.50 s |

**Source inventory.** The H2 integration crate has 238 active test annotations (225 `#[tokio::test]`, 13 `#[test]`) across 12 files. Hyper has 273 active annotations (109 `#[tokio::test]`, 164 `#[test]`) across `tests/` and `src/`; macro-generated tests make `cargo test -- --list` the authoritative count. fastwebsockets has 37. The exact reference counts are measured in `FVP-02` and become the ledger denominator.

**Known green:** root workspace, H2 library and flow-control tests, fastwebsockets, Hyper `cargo check`, and the H2 dogfood/cancellation tests under the smol runtime.

**Known red:** the inherited H2/Hyper test builds. `docs/verification.md` says the Tokio harnesses require adaptation and that the fork commands are manual; `.github/workflows/ci.yml` has no fork step. `scripts/check-smolith-pin.sh` checks versions only.

**Loop timing:** the runnable fork components sum to about 52 s warm. The gate does not exist yet; its first measured run is recorded in `FVP-01`. The target is warm <= 2 min, cold <= 10 min on this host.

## Sequence

Items share the H2 test crate, the Hyper test harness, and the ledger, so the default is serial. The named parallel pairs are the only exception. Every item ends with its acceptance command and the narrowest affected suite on the integrated tree. The full gate runs at `FVP-13`.

In the acceptance column: `H2` = `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml`; `HYPER` = `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full`; `SUPPORT` = `cargo test --manifest-path crates/laputa-protocol-test-support/Cargo.toml`; `WS` = `cargo test --manifest-path crates/laputa-fastwebsockets-futures-lite/Cargo.toml --locked --all-features`.

| ID | Slice | Owned files | Prerequisite | Acceptance |
| --- | --- | --- | --- | --- |
| `FVP-01` | Gate skeleton, ledger, CI job | `scripts/verify-protocols.sh` (new), `docs/fork-test-parity.md` (new), `.github/workflows/ci.yml`, `docs/verification.md` (gate section) | none | `WS`, `cargo check --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full`, `H2 --lib`, and `H2 --test flow_control_futures` all pass through `sh scripts/verify-protocols.sh`; it prints a count per suite, records the first wall time in the ledger, and fails on a zero-test or red suite; `sh -n scripts/verify-protocols.sh`; YAML parses with `jaq --from yaml` |
| `FVP-02` | Reference inventory | disposable `.work/wt/fvp-02`; ledger rows only | `FVP-01` | upstream h2 0.4.20 and hyper 1.12.0 `cargo test -- --list` counts and suite outcomes recorded; if the upstream build cannot run after three attempts, record source counts and the exact build failure instead; the fork's inherited files remain the corpus if the upstream test set differs; `git diff --exit-code` on the fork manifests; no oracle dependency committed |
| `FVP-03` | Shared futures-I/O test support | `crates/laputa-protocol-test-support/**` (new), root `Cargo.toml` `exclude` entry | `FVP-02` | `SUPPORT` passes at least 4 tests, including `wake_loss_is_detected` |
| `FVP-04` | H2 support port and pilot | `crates/laputa-h2-futures-lite/tests/h2-support/**`, `tests/h2-tests/Cargo.toml`, `tests/h2-tests/tests/ping_pong.rs`, `codec_write.rs` | `FVP-03` | `H2 -p h2-tests --test ping_pong --test codec_write` passes 9 tests |
| `FVP-05` | Wake-driven flow-control test | `crates/laputa-h2-futures-lite/tests/flow_control_futures.rs` | `FVP-03`, `FVP-04` | `H2 --test flow_control_futures` passes in <= 5 s; `rg` finds no `NoopWake` or `POLL_BUDGET`; the negative control fails when a wake is suppressed |
| `FVP-06` | h2#975 regression | `crates/laputa-h2-futures-lite/tests/h2-tests/tests/h2_975.rs` (new); `scripts/h2-975-soak.sh` (new, non-gating); the minimal H2 `src/proto/**` fix path only if the regression fails | `FVP-05` | the test fails on the current code and passes after the minimal fix, or the ledger records `not reproduced` after 20 deterministic runs with the exact topology; `H2 --lib` and `H2 --test flow_control_futures` stay green; if a fix is made, the 12-run TLS soak records 0 stalls; no ignored gating test |
| `FVP-07` | H2 flow and stream states | `tests/h2-tests/tests/flow_control.rs`, `stream_states.rs` | `FVP-04`, `FVP-06` | `H2 -p h2-tests --test flow_control --test stream_states` passes 88 tests |
| `FVP-08` | H2 server and client requests | `tests/h2-tests/tests/server.rs`, `client_request.rs` | `FVP-04` | `H2 -p h2-tests --test server --test client_request` passes 93 tests |
| `FVP-09` | H2 remainder | `tests/h2-tests/tests/{codec_read,informational_responses,prioritization,push_promise,trailers,hammer}.rs` | `FVP-04` | `H2 -p h2-tests --test codec_read --test informational_responses --test prioritization --test push_promise --test trailers --test hammer` passes 48 tests, or every exclusion is in the ledger with a covering test |
| `FVP-10` | Hyper support port and pilot | `crates/laputa-hyper-futures-lite/tests/support/**`, `tests/h1_flush_before_yield.rs`, `h1_shutdown_while_buffered.rs`, `ready_on_poll_stream.rs`, `unbuffered_stream.rs` | `FVP-03` | `HYPER --test h1_flush_before_yield --test h1_shutdown_while_buffered --test ready_on_poll_stream --test unbuffered_stream` passes 4 tests |
| `FVP-11` | Hyper integration tests | `tests/server.rs`, `tests/client.rs`, `tests/integration.rs` | `FVP-10` | `HYPER --test server --test client --test integration` passes; exclusions are in the ledger |
| `FVP-12` | Hyper unit tests and dead harness removal | Hyper `src/**` test modules; delete `src/mock.rs` and `src/client/tests.rs` | `FVP-10` | `HYPER --lib` passes; the two dead files are gone; `rg` finds no references |
| `FVP-13` | Close | `docs/fork-test-parity.md`, `docs/verification.md`, `.agents/skills/rebase-upstreams/SKILL.md`, `.github/workflows/ci.yml`, `scripts/verify-protocols.sh` | all | full gate, root suite, feature/dependency checks, ledger exact, no Tokio in fork normal or test graphs, `mdtool` clean |

**Parallel pairs:** after `FVP-04`, `FVP-07` (H2) and `FVP-10` (Hyper) may run in parallel because they own disjoint crates and neither edits the shared support crate, the ledger, or the gate script. After `FVP-10`, `FVP-09` (H2 remainder) and `FVP-11` (Hyper integration) may run in parallel for the same reason. The parent integrates and updates the shared files. Any needed change to `crates/laputa-protocol-test-support` is a serial foundation change, not a lane edit.

**Foundation designs:**

- **Wake executor.** `SUPPORT` provides a single-future executor that polls only after a wake, counts polls and wakes, and fails a 2 s no-wake stall instead of polling again. The flow-control and h2#975 tests use it. `wake_loss_is_detected` drives a future that returns `Pending` without waking and asserts the executor reports the stall. Multi-task tests use `laputa-smol` with the shared timer and pipe; the wake oracle is required only for the futures that assert wake correctness.
- **h2#975 topology.** Two H2 connections run over a duplex `BudgetPipe` with bounded write capacity. Each side fills the peer's write budget with DATA, then both send PING. The unfixed connection returns `Pending` from `poll_ready` while its PONG is queued and stops reading; the executor reports a 2 s stall. The fixed connection reads the peer's PING, writes the PONG, and drains the remaining DATA. The test asserts completion, not a wall-clock threshold.
- **Hyper adapter.** Hyper's test support keeps a test-local `FuturesIo<T>` implementing `hyper::rt::Read`/`Write` for `futures_io` streams. It cannot use `h12tiny-core::io::FuturesIo` because the fork is already a dependency of `h12tiny-core`; the shared crate owns the executor, timer, and pipe, not the Hyper adapter.
- **Workspace membership.** `crates/laputa-protocol-test-support` is added to the root `Cargo.toml` `exclude` list, beside the three forks, so the root workspace suite does not select it. The fork test workspaces path-depend on it.
- **Ledger columns.** `Fork | Suite | Source file | Reference count | Ported | Excluded | Status | Command | Covering test`. `Status` is `ported`, `replaced`, or `excluded`; `not ported` is allowed only while a slice is open.

## Acceptance

**D1. Gate.** `sh scripts/verify-protocols.sh` exits 0; every suite prints its passed/failed/ignored counts and at least one test; no suite is silently skipped. The gate covers H2 library, H2 flow-control, H2 inherited tests, Hyper check, Hyper tests, fastwebsockets, and the shared support crate. Warm <= 2 min, cold <= 10 min. For each fork, `cargo tree -e all` contains no `tokio` after the temporary oracle is removed.

**D2. Ledger.** `scripts/verify-protocols.sh --check-ledger` exits 0. Every row in `docs/fork-test-parity.md` is `ported`, `replaced`, or `excluded`; no row says `not ported`. The `--list` count for a closed suite equals the ledger count. Local regressions (`h2_975`, the wake negative control) are separate rows and do not inflate inherited-test counts. At most 10 exclusions per fork, each with a reason and a named covering test.

**D3. Wake correctness.** `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml --test flow_control_futures` passes; `cargo test --manifest-path crates/laputa-protocol-test-support/Cargo.toml wake_loss_is_detected` passes; a deliberate no-wake patch makes the flow test fail within 2 s rather than spin.

**D4. h2#975.** `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test h2_975` fails against the unmodified fork source (a stall, not a compile error) and passes after the fix, or the ledger records `not reproduced` with 20 deterministic runs, the exact scripted topology, and the host. An ignored test is not acceptance.

**D5. CI.** `.github/workflows/ci.yml` has a `protocol-forks` job that runs `sh scripts/verify-protocols.sh` on Linux and the named macOS smoke subset. `sh -n scripts/verify-protocols.sh` and a local YAML parse pass; the job command passes locally. The first GitHub run is owner-verified under **Needs environment**.

**D6. Documentation.** Run `mdtool check --untracked` once per changed doc (`docs/CAMPAIGN-FORK-VERIFICATION.md`, `docs/fork-test-parity.md`, `docs/verification.md`); each exits 0 except for planned-path warnings that disappear when the named files exist. `docs/verification.md` says the fork gate runs in CI, not manually. `.agents/skills/rebase-upstreams/SKILL.md` says `scripts/verify-protocols.sh` is the fork gate instead of "Hyper check only", and its conflict rules preserve the futures-I/O test support path.

**D7. No regression.** `cargo test --locked --workspace --all-features` still passes (206 or more, zero failures); `sh scripts/check-features.sh` and `sh scripts/check-normal-dependencies.sh --features full` pass; no root production API or behavior changes.

## Needs environment

- **GitHub Actions execution.** The executor cannot push. The owner runs the new `protocol-forks` job on a PR and confirms it is green. Command: `gh run watch` after the PR is opened. This is the only proof that a protocol change is blocked at merge time.
- **macOS fork smoke.** The local host is Linux. The macOS job runs H2 library, H2 flow-control, Hyper check, and fastwebsockets; a full macOS fork run is a follow-on.
- **Conformance and external interop.** `h2spec`, `nghttp`, `nghttpd`, `oha`, `h2load`, Autobahn, and Docker are not installed here. They belong to campaign 3 and are not proof for this campaign.
- **h2#975 TLS soak.** The deterministic scripted regression is the gate. A 12-run TLS soak with the local fixture can corroborate it; if the owner wants an external TLS peer, that run needs a host with the peer and is not required locally.

## Execution boundary and non-goals

**Authorized effects:** edit the fork test harnesses, the shared dev-only test support crate, the root `Cargo.toml` `exclude` entry for that crate, the gate script, the ledger, the named docs, and the CI workflow; run cargo tests and `git worktree` operations under `.work/`; commit locally. **Forbidden:** push, publish, `make publish`, `cargo publish`, change `rust-toolchain.toml` or `SMOLITH_REV`, change h12tiny production API/behavior, add Tokio to a normal dependency graph, delete root tests, add broad `#[ignore]`, use `/tmp` for scratch, run formatters, or force-remove a worktree.

**Contract changes:**

- Hyper's fork gate becomes `scripts/verify-protocols.sh` plus tests, not `cargo check --features full` alone. This supersedes the rule in `.agents/skills/rebase-upstreams/SKILL.md`.
- The H2 integration crate becomes a gate; the H2 library-test-only command is no longer the whole H2 proof.
- A new dev-only crate, `crates/laputa-protocol-test-support`, is added. It is not published and has no product API.
- CI gains a Linux `protocol-forks` job and a macOS smoke job; per-PR time increases by roughly the gate target.

**Follow-on campaigns (not authorized by this document):**

| Campaign | Scope | Completion criterion |
| --- | --- | --- |
| 2. Production invariants | H12T-12, H12T-14, H12T-15: bounded retry, explicit driver errors, production preset, drain deadline, validated protocol selection and limits, TLS trust source | every request and connection is accounted for through success, error, timeout, and cancellation; no fail-open default remains without an explicit opt-in |
| 3. Adversarial qualification | h2spec, RFC 9113 targeted cases, curl/nghttp2 interop, property-based state machines, fault injection, fuzzing with retained corpus, Autobahn, Miri/Loom where invariants exist | a tracked matrix of supported features, passed requirements, known exceptions, and reproducible failures; every deterministic layer is gated |
| 4. Release and performance | fork upstream SHA tracking, fork-delta audit, staged registry consumption, advisory checks, controlled latency and memory baselines | an independent consumer builds and tests from a staging registry; upstream SHAs and deltas are recorded; performance claims follow `benchmark-checklist` |

This campaign does not modify the H12T-12 RFC, the root production crates, fork package versions, or the release scripts. At `FVP-13` it may update `TODO.md` only to record H12T-13's completion; it does not change H12T-12/14/15/16. It does not run h2spec, Autobahn, fuzzing, or performance measurements.

## Reference

**Measured code and documentation:**

- H2 ordering gate: `crates/laputa-h2-futures-lite/src/proto/connection.rs:218-240` (`poll_ready`), `:356-390` (`poll2` calls `poll_ready` before `poll_next`).
- Manual-poll regression: `crates/laputa-h2-futures-lite/tests/flow_control_futures.rs:16-20,266-276` (`NoopWake`, `POLL_BUDGET`).
- H2 Tokio harness: `crates/laputa-h2-futures-lite/tests/h2-support/src/mock.rs:10`, `tests/h2-support/Cargo.toml:18-19`, `tests/h2-tests/Cargo.toml:14`.
- H2 adaptation note: `crates/laputa-h2-futures-lite/tests/h2-tests/README.md`.
- Hyper dead harness: `crates/laputa-hyper-futures-lite/src/mock.rs:1`, `src/client/tests.rs:1`.
- Hyper Tokio harness: `crates/laputa-hyper-futures-lite/tests/support/mod.rs:12,23-25`, `tests/support/tokiort.rs`.
- Fork exclusion: `Cargo.toml:22-26`.
- Manual-only fork commands: `docs/verification.md:21-24,32-48`.
- No fork CI step: `.github/workflows/ci.yml:50-54`.
- Version-only pin check: `scripts/check-smolith-pin.sh`.
- Existing futures-I/O adapter for reference: `crates/h12tiny-core/src/io.rs:19-168` (`FuturesIo`, `HyperIo`).
- H2 dogfood and cancellation under smol: `tests/dogfood_h2_prior.rs`, `tests/h2_lifecycle.rs`.

**Upstream and program references:**

- [h2#975, bidirectional bulk-data deadlock](https://github.com/hyperium/h2/issues/975); the fork is `0.4.20-beta.1` and the issue reports 0.4.19 and 0.4.20.
- H2 0.4.20 release notes: `crates/laputa-h2-futures-lite/CHANGELOG.md:1`.
- Hyper 1.12.0 release notes: `crates/laputa-hyper-futures-lite/CHANGELOG.md:1`.
- Smolith practice: `../smolith/UPSTREAM.toml`, `../smolith/docs/LEDGER.md`, `../smolith/docs/BASELINES.md`, `../smolith/docs/release.md`.
- Smolith's h12tiny fixture records the same backpressure gap: `../smolith/fixtures/h12tiny.py` (`BACKPRESSURE_GAP`).
- Fork maintenance: `.agents/skills/rebase-upstreams/SKILL.md`, `.agents/skills/publish-forks/SKILL.md`.

**Reuse / change / new:**

- Reuse: H2 library and flow-control tests, fastwebsockets 37 tests, root H2 dogfood/cancellation tests, the `FuturesIo` adapter design.
- Change: `h2-support` and the H2 test crate from Tokio I/O to futures-I/O; Hyper test support from `TokioIo`/`TokioExecutor`/`TokioTimer` to futures-I/O and smol; `docs/verification.md`; `rebase-upstreams` gate rule; CI.
- New: `scripts/verify-protocols.sh`, `docs/fork-test-parity.md`, `crates/laputa-protocol-test-support`, the H2 deadlock regression, the CI `protocol-forks` job.
