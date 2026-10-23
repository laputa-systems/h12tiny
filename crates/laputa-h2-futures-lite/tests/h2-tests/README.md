# H2 integration test sources

This separate crate uses `h2-support` and the fork's `unstable` feature.
Its inherited Tokio transports require adaptation to the fork's futures-I/O
traits before these tests can run. The focused futures-I/O flow-control
regression is in [`../flow_control_futures.rs`](../flow_control_futures.rs).
Run it with the H2 flow-control command in
[verification](../../../../docs/verification.md).
