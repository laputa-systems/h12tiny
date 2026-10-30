# H12T-12: Production bounds and lifecycle policy

Status: draft

## Summary

The client pool, server accept loop, body collection, and connection drivers currently fail open: unbounded retries, unbounded queues and bodies, silent at-capacity close, and shutdown drain with no deadline. A single misconfigured application gets slow-loris and OOM exposure with no observable signal.

## Motivation

Evidence: `crates/h12tiny-client/src/lib.rs:637` (unbounded `Retry` continue), `:850-877` (detached drivers discard `Result`), `:1081-1085` (pool `usize::MAX` defaults), `crates/h12tiny-server/src/lifecycle.rs:357` (silent at-capacity drop), `:372` (graceful drain with no timeout), `crates/h12tiny-web/src/lib.rs:502` (`unwrap_or(usize::MAX)` body fallback).

## Design

Ship one documented production-minimums preset (connection cap, headers/body timeout, body cap, drain deadline) and fail loudly when a server/router is built with none of them. Bound retry with a cap plus backoff and only for safe/idempotent cases, join or report driver errors via the existing `DebugEvent`/connection-error hook, emit a counter or hook on at-capacity reject, classify accept errors (retry transient, abort only on fatal), and add a configurable drain timeout that escalates to abort.

## Alternatives considered

- Per-crate ad-hoc limits with no shared preset: keeps autonomy but preserves the current discovery cost and inconsistent tuning.
- Hard-coded low defaults: protects out of the box but breaks high-throughput consumers who currently rely on unlimited pooling.

## Open questions

- What should the default caps be (max connections per host, idle timeout, body limit, drain deadline), and which stay unlimited by default?
- Should a missing production preset warn or fail the builder?
- Should retry bounds distinguish idempotent methods and replayable bodies?

## Decision

Undecided. Owner to choose defaults and fail-loud behavior.
