# H12T-18: Bounded H2 control responses

Status: draft

## Summary

Make inbound control handling and outbound acknowledgment capacity one explicit
protocol contract. Preserve bidirectional progress when output is blocked,
without reentering a single occupied reply slot or accumulating unbounded work.

## Motivation

`Connection::poll2` reads more frames when control output is pending. The PING
and SETTINGS handlers still assume the previous reply has been buffered before
another frame arrives. The resulting assertion path was independently traced
through the codec, but no wire-level reproduction was run during review.

The existing bidirectional-progress regression protects a real failure mode.
Restoring the previous stop-reading rule would exchange one defect for another.

## Design

Use one bounded owner for control obligations. Its states must distinguish
received work, acknowledgment awaiting encoder capacity, and completed work.
Receiving another control frame must either enter valid bounded state or
produce a protocol-level outcome; it must not invoke an internal assertion.

Keep SETTINGS application and acknowledgments in their required order. Account
for obligations separately from DATA buffering, and retain the read progress
needed for the peer to release flow-control pressure. Do not introduce an
unbounded queue as an acknowledgment backlog.

Acceptance must combine multiple control obligations with exhausted encoder
capacity, then restore output capacity and verify ordered completion. Retain
the existing bidirectional-progress case and verify bounded behavior when the
chosen control-work budget is exhausted. Use deterministic in-memory I/O.

## Alternatives considered

- Stop all reads while control output is pending. Rejected because it restores
  the previously fixed bidirectional deadlock.
- Remove the assertions or overwrite pending state. Rejected because it loses
  control obligations and can apply SETTINGS incorrectly.
- Reserve control capacity or maintain a bounded ordered backlog. These are
  viable mechanisms if their scheduling preserves progress and ordering.

## Open questions

- Should exhausted control capacity close the connection with a defined
  protocol error, or suspend a subset of inbound work? Recommend a bounded,
  explicit error policy unless selective suspension can preserve progress.
- Does the limit need a public setting? Recommend an internal limit first;
  expose configuration only for a demonstrated application requirement.

## Decision

Pending. Implement only after the scheduling and boundedness policy is chosen.
