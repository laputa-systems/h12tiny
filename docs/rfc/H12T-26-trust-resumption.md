# H12T-26: System trust and session resumption

Status: draft

## Summary

Choose whether a new transport connection using managed system trust must
observe current trust even when the TLS peer supports session resumption.
Recommend disabling resumption for this managed configuration first.

## Motivation

The managed verifier refreshes its internal trust state during certificate
verification. Rustls resumption can reuse an earlier approval without invoking
that verifier. Its cache compatibility checks verifier identity, which remains
unchanged when the managed verifier replaces its internal roots.

The relevant configuration and Rustls 0.23.46 paths were inspected locally.
An end-to-end exchange after a trust change was not run. The TLS guide now
describes the current limit instead of promising fresh verification on every
new handshake.

## Design

Recommend disabling resumption when h12tiny constructs a configuration using
managed system trust, for both unauthenticated and mutual-TLS client builders.
Keep explicit-root and caller-supplied configuration policy with the caller.
Established pooled connections may continue under the existing contract.

If resumption is retained, session lookup must check trust freshness before
offering cached state. A generation change or reload failure must invalidate
the previous approval. Native platform trust also needs a supported freshness
mechanism; changing only the PEM verifier is not a complete policy.

Acceptance should distinguish a full handshake from a resumed one, reuse a
configuration across an authorized trust-store change, and verify the selected
policy for both TLS 1.2 and TLS 1.3. Include an unreadable or invalid store after
the first connection. Use private local fixtures. Keep custom-root behavior
and typed reload failures covered. Do not claim a performance cost without
measurement.

## Alternatives considered

- Disable managed resumption. Recommended for the smallest complete freshness
  contract, with a potential reconnect cost to measure separately.
- Invalidate resumable state before use when trust changes. Preserves more
  reuse but adds cache ownership and platform-specific freshness work.
- Accept old approval until sessions expire. Matches continuation semantics,
  but weakens the previous new-handshake freshness promise and needs an
  explicit owner decision.

## Open questions

- Is strict freshness required on new connections, or is a resumption grace
  period acceptable? Recommend strict freshness.
- Is retaining resumption worth implementing trust-aware cache invalidation?
  Recommend disabling it first and measuring a representative reconnect
  workload before adding that mechanism.

## Decision

Pending. No TLS configuration behavior changes as part of this review.
