# H12T-20: Router composition ownership

Status: draft

## Summary

Define what survives `Router::nest` and `Router::merge`. A configured child
must retain its state and policy, or composition must reject the conflicting
configuration explicitly.

## Motivation

The current implementation flattens routes and retains a single state value.
Nesting children configured with different values causes both handlers to read
the first value. This was reproduced through `Router::call` without networking.

The same methods omit child CORS and can promote a child fallback to the whole
parent. Those behaviors need one composition contract, not separate patches
that preserve only whichever field a new caller happens to notice.

## Design

Recommend preserving state already bound to endpoints. Unbound endpoints may
inherit the assembled router's state; a later parent binding must not silently
replace a child's explicit binding. Keep route matching and immutable sharing
independent of this binding rule.

Define nested fallbacks within the prefix that owns them. Merged fallback
conflicts must be explicit. Decide how child and parent CORS compose, including
preflight and error responses, before adopting an implementation representation.

Acceptance uses distinct state values under both nesting and merging, parent
state before and after composition, unmatched paths inside and outside a nested
prefix, and CORS behavior on successful and rejected requests. The existing
route-bound body limits, timeouts, and copy-on-write behavior must remain intact.

## Alternatives considered

- Bind state once after composition and reject configured children. This is
  simpler, but changes valid-looking existing builder usage and needs a clear
  public construction/error contract.
- Preserve configured subtree state and policy. Recommended for independently
  assembled applications, at the cost of explicit binding ownership.
- Keep first-state-wins and document it. Rejected because it silently changes
  a configured handler's input when composition order changes.

## Open questions

- Preserve independently configured children, or require one final state
  binding? Recommend preservation; choose the single-binding alternative only
  if the smaller API is preferred enough to justify migration.
- How should parent and child CORS interact? Recommend that composition reject
  an ambiguous combination until an explicit precedence rule is selected.

## Decision

Pending. The current silent substitution is not an accepted ownership rule.
