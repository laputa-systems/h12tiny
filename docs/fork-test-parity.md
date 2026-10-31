# Fork test parity ledger

Every inherited test or test target in the h2 and hyper forks is classified
here. Counts are `cargo test -- --list` counts from the reference release, not
annotation counts. A closed suite has `Ported + Excluded` equal to its
reference count, and the gate refuses a row still marked `not ported`.

Local regressions (`h2_975`, `wake_loss_is_detected`, and the wake-driven
flow-control exchange) are separate rows and do not inflate inherited counts.

| Fork | Suite | Source file | Reference count | Ported | Excluded | Status | Command | Covering test |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| h2 | library | `src/**/*.rs` | 446 | 446 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml --lib` | self |
| h2 | flow-control futures regression | `tests/flow_control_futures.rs` | 1 | 1 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml --test flow_control_futures` | self |
| h2 | h2-tests ping_pong | `tests/h2-tests/tests/ping_pong.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test ping_pong` | - |
| h2 | h2-tests codec_write | `tests/h2-tests/tests/codec_write.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test codec_write` | - |
| h2 | h2-tests flow_control | `tests/h2-tests/tests/flow_control.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test flow_control` | - |
| h2 | h2-tests stream_states | `tests/h2-tests/tests/stream_states.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test stream_states` | - |
| h2 | h2-tests server | `tests/h2-tests/tests/server.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test server` | - |
| h2 | h2-tests client_request | `tests/h2-tests/tests/client_request.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test client_request` | - |
| h2 | h2-tests codec_read | `tests/h2-tests/tests/codec_read.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test codec_read` | - |
| h2 | h2-tests informational_responses | `tests/h2-tests/tests/informational_responses.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test informational_responses` | - |
| h2 | h2-tests prioritization | `tests/h2-tests/tests/prioritization.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test prioritization` | - |
| h2 | h2-tests push_promise | `tests/h2-tests/tests/push_promise.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test push_promise` | - |
| h2 | h2-tests trailers | `tests/h2-tests/tests/trailers.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test trailers` | - |
| h2 | h2-tests hammer | `tests/h2-tests/tests/hammer.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test hammer` | - |
| hyper | library | `src/**/*.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --lib` | - |
| hyper | h1_flush_before_yield | `tests/h1_flush_before_yield.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --test h1_flush_before_yield` | - |
| hyper | h1_shutdown_while_buffered | `tests/h1_shutdown_while_buffered.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --test h1_shutdown_while_buffered` | - |
| hyper | ready_on_poll_stream | `tests/ready_on_poll_stream.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --test ready_on_poll_stream` | - |
| hyper | unbuffered_stream | `tests/unbuffered_stream.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --test unbuffered_stream` | - |
| hyper | h1_ready_in_flight | `tests/h1_ready_in_flight.rs` | TBD | 0 | 1 | excluded | not in gate | `ready_on_poll_stream`, `h1_flush_before_yield` |
| hyper | client | `tests/client.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full --test client` | - |
| hyper | server | `tests/server.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full --test server` | - |
| hyper | integration | `tests/integration.rs` | TBD | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full --test integration` | - |
| fastwebsockets | fastwebsockets | `crates/laputa-fastwebsockets-futures-lite` | 37 | 37 | 0 | ported | `cargo test --manifest-path crates/laputa-fastwebsockets-futures-lite/Cargo.toml --locked --all-features` | self |

Exclusion reasons:

- `h1_ready_in_flight` asserts Tokio's cooperative-budget behavior, not fork
  behavior. `ready_on_poll_stream` and `h1_flush_before_yield` cover the
  readiness path.

## Local regressions

| Fork | Suite | Source file | Reference count | Ported | Excluded | Status | Command | Covering test |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| h2 | h2#975 bidirectional deadlock | `tests/h2-tests/tests/h2_975.rs` | 1 | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test h2_975` | self |
| shared | wake loss detection | `crates/laputa-protocol-test-support` | 1 | 0 | 0 | not ported | `cargo test --manifest-path crates/laputa-protocol-test-support/Cargo.toml wake_loss_is_detected` | self |

## Gate runs

| Date | Commit | Host | Command | Result | Wall |
| --- | --- | --- | --- | --- | --- |
| 2026-10-31 | `FVP-01` | Linux x86_64, 32 cores, load 38.1 | `sh scripts/verify-protocols.sh --suite h2-lib --suite h2-flow --suite hyper-check --suite fastwebsockets` | pass (446 + 1 + 37 tests, Hyper check) | 16 s |
