# H2 test support

This crate contains the inherited integration test helpers and enables H2's
`unstable` feature. The mock codec, frame builders, and future extensions run
on futures-I/O. `laputa-protocol-test-support` supplies the wake-driven
executor, timer, and scripted mock I/O, so the helpers need no Tokio runtime.
See the [integration test notes](../h2-tests/README.md) and
[verification commands](../../../../docs/verification.md).
