# Fork test parity ledger

Every inherited test or test target in the h2 and hyper forks is classified
here. Counts are `cargo test -- --list` counts from the reference release, not
annotation counts. A closed suite has `Ported + Excluded` equal to its
reference count, and the gate refuses a row still marked `not ported`.

Local regressions (`h2_975`, `wake_loss_is_detected`, and the wake-driven
flow-control exchange) are separate rows and do not inflate inherited counts.

| Fork | Suite | Source file | Reference count | Ported | Excluded | Status | Command | Covering test |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| h2 | library | `src/**/*.rs` | 447 | 446 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml --lib` | self |
| h2 | flow-control futures regression | `tests/flow_control_futures.rs` | 1 | 1 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml --test flow_control_futures` | self |
| h2 | h2-tests ping_pong | `tests/h2-tests/tests/ping_pong.rs` | 5 | 5 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test ping_pong` | - |
| h2 | h2-tests codec_write | `tests/h2-tests/tests/codec_write.rs` | 4 | 4 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test codec_write` | - |
| h2 | h2-tests flow_control | `tests/h2-tests/tests/flow_control.rs` | 52 | 52 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test flow_control` | - |
| h2 | h2-tests stream_states | `tests/h2-tests/tests/stream_states.rs` | 36 | 36 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test stream_states` | - |
| h2 | h2-tests server | `tests/h2-tests/tests/server.rs` | 46 | 46 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test server` | - |
| h2 | h2-tests client_request | `tests/h2-tests/tests/client_request.rs` | 47 | 47 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test client_request` | - |
| h2 | h2-tests codec_read | `tests/h2-tests/tests/codec_read.rs` | 15 | 15 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test codec_read` | self |
| h2 | h2-tests informational_responses | `tests/h2-tests/tests/informational_responses.rs` | 7 | 7 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test informational_responses` | self |
| h2 | h2-tests prioritization | `tests/h2-tests/tests/prioritization.rs` | 7 | 7 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test prioritization` | self |
| h2 | h2-tests push_promise | `tests/h2-tests/tests/push_promise.rs` | 13 | 13 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test push_promise` | self |
| h2 | h2-tests trailers | `tests/h2-tests/tests/trailers.rs` | 5 | 5 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test trailers` | self |
| h2 | h2-tests hammer | `tests/h2-tests/tests/hammer.rs` | 1 | 1 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test hammer` | self |
| hyper | library | `src/**/*.rs` | 135 | 131 | 4 | ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full --lib` | `client_flushing_is_not_ready_for_next_request` |
| hyper | h1_flush_before_yield | `tests/h1_flush_before_yield.rs` | 1 | 1 | 0 | ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full --test h1_flush_before_yield` | - |
| hyper | h1_shutdown_while_buffered | `tests/h1_shutdown_while_buffered.rs` | 1 | 1 | 0 | ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full --test h1_shutdown_while_buffered` | - |
| hyper | ready_on_poll_stream | `tests/ready_on_poll_stream.rs` | 1 | 1 | 0 | ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full --test ready_on_poll_stream` | - |
| hyper | unbuffered_stream | `tests/unbuffered_stream.rs` | 1 | 1 | 0 | ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full --test unbuffered_stream` | - |
| hyper | h1_ready_in_flight | deleted (Tokio cooperative budget) | 1 | 0 | 1 | excluded | not in gate | `ready_on_poll_stream`, `h1_flush_before_yield` |
| hyper | client | `tests/client.rs` | 70 | 70 | 0 | ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full --test client` | - |
| hyper | server | `tests/server.rs` | 99 | 99 | 0 | ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full --test server` | - |
| hyper | integration | `tests/integration.rs` | 14 | 14 | 0 | ported | `cargo test --manifest-path crates/laputa-hyper-futures-lite/Cargo.toml --features full --test integration` | - |
| fastwebsockets | fastwebsockets | `crates/laputa-fastwebsockets-futures-lite` | 37 | 37 | 0 | ported | `cargo test --manifest-path crates/laputa-fastwebsockets-futures-lite/Cargo.toml --locked --all-features` | self |

Exclusion reasons:

- `h1_ready_in_flight` asserts Tokio's cooperative-budget behavior, not fork
  behavior. `ready_on_poll_stream` and `h1_flush_before_yield` cover the
  readiness path. The test file was deleted rather than left compiling against
  Tokio.
- `client_close_drains_pending_requests_on_reset`,
  `client_close_drains_pending_requests_on_eof`, and
  `client_close_drains_pending_requests_without_shutdown` exhaust Tokio's
  cooperative budget to force a closed channel to return `Pending`. Futures
  channels have no cooperative budget, so the tests are ignored, not simulated.
  `client_flushing_is_not_ready_for_next_request` covers dispatcher shutdown
  and drain ordering.
- `client_read_bytes_before_writing_request` depends on the Tokio mock's
  write-blocking order for a response that arrives before its request.
  `client_flushing_is_not_ready_for_next_request` covers the dispatcher write
  gate; the excluded case is recorded rather than weakened.

### Upstream ignored tests

These remain `#[ignore]`d exactly as upstream, so they are counted as ported
tests with an ignored outcome rather than as exclusions. None of them gate a
port change.

- H2 library (1): `hpack::encoder::test::test_evicted_overflow`.
- H2 integration (12): `request_with_h1_version`, `read_frame_too_big`,
  `read_headers_empty_payload`, `read_headers_with_pseudo`,
  `read_headers_without_pseudo`, `expand_window_calls_are_coalesced`,
  `expand_window_sends_window_update`, `recv_window_update_causes_overflow`,
  `stream_close_by_recv_reset_frame_releases_capacity`,
  `recv_push_promise_with_wrong_authority_is_stream_error`,
  `accept_with_pending_connections_after_socket_close`,
  `recv_trailers_without_eos`.
- Hyper library (6 upstream): the five HTTP/2 executor compile checks
  (`send_sync_executor_of_send_futures`,
  `send_sync_executor_of_non_send_futures`,
  `send_not_sync_executor_of_send_futures`,
  `send_not_sync_executor_of_not_send_futures`,
  `not_send_not_sync_executor_of_not_send_futures`) and
  `iobuf_write_empty_slice`.
- Hyper server (1): `response_body_lengths::chunked_response_trumps_length`.

## Local regressions

| Fork | Suite | Source file | Reference count | Ported | Excluded | Status | Command | Covering test |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| h2 | h2#975 bidirectional deadlock | `tests/h2-tests/tests/h2_975.rs` | 1 | 1 | 0 | ported | `cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml -p h2-tests --test h2_975` | self |
| shared | wake loss detection | `crates/laputa-protocol-test-support` | 1 | 1 | 0 | ported | `cargo test --manifest-path crates/laputa-protocol-test-support/Cargo.toml wake_loss_is_detected` | self |

## Reference inventory

Measured 2026-10-31 on Linux x86_64 with nightly-2026-09-15 from the upstream
git tags, because the crates.io packages exclude `tests/`. Cargo target and
temporary directories were under `.work/`.

| Reference | Tag and commit | Command | Outcome |
| --- | --- | --- | --- |
| h2 0.4.20 | `v0.4.20` `b0e890842ac16d3c76836fd4fcc06421acb15fa7` | `cargo test --lib` | 444 passed, 0 failed, 1 ignored |
| h2 0.4.20 | same | `cargo test -p h2-tests` | all 12 targets green: client_request 46+1 ignored, codec_read 11+4 ignored, codec_write 4, flow_control 48+4 ignored, hammer 1, informational_responses 7, ping_pong 5, prioritization 7, push_promise 12+1 ignored, server 44+1 ignored, stream_states 34, trailers 4+1 ignored |
| hyper 1.12.0 | `v1.12.0` `7f67699ce32d717081deb7018b4da7d679277ac0` | `cargo test --features full --tests` | all targets green: lib 124+6 ignored, client 70, server 98+1 ignored, integration 14, h1_flush_before_yield 1, h1_ready_in_flight 1, h1_shutdown_while_buffered 1, ready_on_poll_stream 1, unbuffered_stream 1 |

The h2 reference `--list` totals are 47, 15, 4, 52, 1, 7, 5, 7, 13, 45,
34, and 5 across the twelve integration files, and 445 for the library.

The fork corpus is the ledger denominator when it differs from upstream:

- `server.rs` adds `early_response_no_error_resets_do_not_affect_local_limit`
  (46 versus 45).
- `stream_states.rs` adds `recv_end_stream_survives_go_away` and
  `recv_end_stream_survives_eof` (36 versus 34).
- The fork library has 447 tests versus 445 upstream.
- The Hyper library has 135 tests versus 133 upstream; `common/io/compat`
  adds shutdown normalization regressions.
- Every other inherited h2 test file and every inherited Hyper test file is
  byte-identical to the reference release.

### Wake negative control

On 2026-10-31, removing the reader wake from `BudgetPipe::poll_write` made
`flow_control_futures` fail with
`Stall { stats: Stats { polls: 2, wakes: 1 }, limit: 2s }` in 2.00 s instead
of spinning. The patch was reverted. `wake_loss_is_detected` in the shared
crate gates the same executor behavior without a patched dependency.

### h2#975 regression

The gated regression is `pending_control_writes_do_not_stop_reading` in
`tests/h2-tests/tests/h2_975.rs`. It runs two client connections over the
scripted mock codec with a 1 KiB write budget. Each connection queues 64 KiB of
request DATA, fills the peer write budget, then receives a response HEADERS,
a peer PING, and 8 KiB of DATA. One connection also queues a user PING. The app
reads the response DATA, the peer restores write capacity, and the test waits
for PONG (and the user PING) before closing the peer. It asserts completion,
not a wall-clock threshold.

On the unfixed `poll2` ordering the test fails with
`Stall { polls: 4, wakes: 3, limit: 2s }`: the connection reads the PING,
queues PONG, and stops reading the DATA behind it. After the fix (read while a
control-frame write is pending) the test passed 20 consecutive runs and the
12-run scripted soak passed 12/12. The non-gating TLS corroboration
(`sh scripts/h2-975-soak.sh --tls`) passed 12/12 against the local fixture.

## Gate runs

| Date | Commit | Host | Command | Result | Wall |
| --- | --- | --- | --- | --- | --- |
| 2026-10-31 | `FVP-01` | Linux x86_64, 32 cores, load 38.1 | `sh scripts/verify-protocols.sh --suite h2-lib --suite h2-flow --suite hyper-check --suite fastwebsockets` | pass (446 + 1 + 37 tests, Hyper check) | 16 s |
| 2026-10-31 | `FVP-13` | Linux x86_64, 32 cores | `sh scripts/verify-protocols.sh` | pass: H2 lib 446 passed/1 ignored, H2 flow 1, H2 inherited 227 passed/12 ignored, Hyper tests 309 passed/11 ignored, fastwebsockets 37, support 6 | 34 s |
| 2026-10-31 | `FVP-13` | Linux x86_64, 32 cores | `sh scripts/verify-protocols.sh --check-ledger` | pass: statuses closed, 5 exclusions, every `--list` count matches | 2 s |
| 2026-10-31 | `FVP-13` | Linux x86_64, 32 cores | `cargo test --locked --workspace --all-features` | pass | 16 s |
