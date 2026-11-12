# H12T-25: Connection setup deadlines

Status: draft

## Summary

Give the production listener a deadline for TLS acceptance and protocol
detection, before an established HTTP driver takes over its own deadlines.

## Motivation

The current header timeout starts inside the selected H1 driver. Before that,
an accepted stream may remain in protocol detection or TLS acceptance while
occupying an admission slot. Source inspection establishes that these states
have shutdown cancellation but no elapsed-time bound. No expiry or saturation
experiment was run during review.

## Design

Recommend one setup budget measured from acceptance through completion of
transport negotiation and protocol selection. On expiry, drop the setup
future, release its admission slot, notify the existing error hook with a
timeout, and finish the tracked task exactly once.

Apply a nonzero setup deadline in `production()`. Keep explicit builder
configuration available without turning the setup budget into a lifetime limit
on response streams. Preserve shutdown cancellation and drain deadlines.

Adding a field to the public `ProductionBounds` struct can break struct-literal
callers. Choose an additive builder method or an explicitly versioned public
change; do not accidentally alter the compatibility contract while adding the
limit.

Acceptance uses one pending-I/O connection, controlled timer completion, and
an observed released slot. A positive setup case must retain the connection
past that setup deadline while streaming normally. No flood test is required.

## Alternatives considered

- Reuse only Hyper's header timeout. Insufficient because it starts later.
- Apply a deadline to the entire connection. Rejected because valid long-lived
  response streams would expire.
- Separate TLS and protocol-detection budgets. Possible, but adds settings
  without an established need over a single acceptance-to-selection budget.

## Open questions

- What should the production setup default be? Recommend 10 s initially,
  matching the current header preset while keeping the scopes distinct.
- Preserve the current `ProductionBounds` struct shape or change it in the
  next minor release? Recommend an additive builder method for compatibility.

## Decision

Pending. The exact API and default are proposals, not current behavior.
