# Review: h12tiny

Scope: the facade, seven component crates, examples, tests, manifests, CI,
release scripts, and documentation. Dependency sources were inspected where
needed to understand protocol setup and cancellation; sibling repositories
were not changed.

## Checks run

- `cargo test --locked --workspace --all-features` passed, including retained
  regressions for pool capacity, cancellation, TLS setup, blocking deadlines
  and body state, SSE framing, CORS, routing, and pending body reads.
- `cargo clippy --locked --workspace --all-targets --all-features -- --deny warnings`
  passed. Cargo still reports the existing server manifest warning about the
  explicit `rustls-graviola` dependency.
- The feature compile matrix and normal dependency policy passed. Separate
  H1-only TLS and H2-only cleartext runtime tests passed.
- `scripts/check-msrv.sh` passed using Rust 1.85 and 1.89 for the corresponding
  component contracts.
- Controlled release probes compared wrapped routing and pending reads with
  the previous implementations; host readiness checks passed before and after
  each measurement. Allocation checks confirmed that router clones share
  routing state and pending reads retain their buffer.
- The README example compiled, Markdown links and documentation routing were
  checked, and `git diff --check` passed.

Not covered: Linux execution, Miri, live external interoperability, actual
publication, exhaustive historical secret scanning, or independent dependency
security audits. The allocation probes do not establish production throughput.

## Summary

The eighteen reported findings are resolved. Socket ownership now preserves
pool capacity through eviction and handoff, incomplete HTTP setup is
cancellable, and protocol defaults follow compiled capabilities. Blocking
dispatch uses one deadline and reports body state accurately. Web framing,
CORS, and method discovery respect their wire contracts. CI executes the
behavioral suite and enforces feature, dependency, and compiler requirements.

Public contract changes are documented: SSE event/id setters return a result,
invalid explicit ports return `InvalidPort`, successful blocking CONNECT
returns `UnsupportedUpgrade`, and client/server/web crates require Rust 1.89.

## Findings

No outstanding findings from this review.

## Strengths to keep

- Separate async and blocking transports share origin normalization without
  pulling an async runtime into the blocking client.
- H1 reservations are exclusive and H2 sessions are shared; retries require
  a returned unsent request rather than replaying an ambiguous exchange.
- Empty facade defaults and weak protocol forwarding keep role selection
  explicit, with dependency isolation enforced in CI.
- Raw-wire, cancellation, lifecycle, and interoperability fixtures exercise
  real protocol boundaries; the new regressions retain those boundaries.
