# H2 test support

This crate contains the inherited integration test helpers and enables H2's
`unstable` feature. Its Tokio transport helpers require adaptation before the
inherited tests can use this fork. The focused futures-I/O flow-control
regression bypasses these helpers. See the [integration test notes](../h2-tests/README.md)
and [verification commands](../../../../docs/verification.md).
