# H2 integration test sources

This separate crate uses `h2-support` and the fork's `unstable` feature.
Its inherited Tokio transports require adaptation to the fork's futures-I/O
traits before these tests can run. The supported H2 library unit-test command
is listed in [verification](../../../../docs/verification.md).
