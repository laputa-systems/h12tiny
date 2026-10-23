Prefix: H12T
Next: 9

### H12T-7 · P3 · Client WebSocket masks use a noncryptographic generator

- **Area:** Security
- **Where:** `crates/laputa-fastwebsockets-futures-lite/src/frame.rs:250`, `crates/laputa-fastwebsockets-futures-lite/src/lib.rs:419`
- **Problem:** Client-role writes generate masking keys with `fastrand::u32`, whose implementation documents that it is not cryptographically secure. This does not meet the unpredictable-key requirement in [RFC 6455 section 5.3](https://www.rfc-editor.org/rfc/rfc6455#section-5.3). The standalone fork exports `Role::Client`; h12tiny's wrapper uses only `Role::Server`. Concrete next-key prediction or exploitation was not demonstrated.
- **Recommendation:** Obtain client masking keys from an OS-backed cryptographic random source, propagate acquisition errors, and remove fastrand if it has no remaining production use. Verify client frames remain masked and server frames remain unmasked.
- **Fix with:** security
- **Effort:** S (hours)

### H12T-8 · P3 · H2 window-update integration coverage cannot run

- **Area:** Tests
- **Where:** `crates/laputa-h2-futures-lite/tests/h2-tests/tests/flow_control.rs:151`, `crates/laputa-h2-futures-lite/tests/h2-support/src/mock.rs:10`, `crates/laputa-h2-futures-lite/tests/h2-support/src/mock.rs:347`, `crates/laputa-h2-futures-lite/src/client.rs:1304`, `docs/verification.md:21`
- **Problem:** The retained `release_capacity_sends_window_update` test sends 16 KiB frames and asserts stream and connection WINDOW_UPDATE frames. Its mock implements only Tokio I/O, while the fork handshake requires futures-I/O traits. The documented H2 check runs library unit tests and excludes this separate integration crate. The h12tiny H2 tests use small bodies and do not check capacity release or window-update frames. Thus this named flow-control behavior has test source but no usable regression check in the current verification path. This conclusion follows from the incompatible trait implementations; the integration crate was not compiled during review.
- **Recommendation:** Adapt the test transport to futures-I/O and restore a runnable focused test for capacity release, stream updates, and connection updates. Document its local invocation alongside the H2 unit-test command.
- **Fix with:** reference-parity
- **Effort:** M (days)
