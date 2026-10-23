Prefix: H12T
Next: 9

### H12T-1 · P2 · Fragmented text silently drops an incomplete UTF-8 suffix

- **Area:** Data
- **Where:** `crates/laputa-fastwebsockets-futures-lite/src/fragment.rs:185`, `crates/laputa-fastwebsockets-futures-lite/src/fragment.rs:193`, `crates/h12tiny-web/src/lib.rs:2240`
- **Problem:** A final text continuation can leave incomplete UTF-8 in `pending`, then return only the validated buffer as a successful message. A public-API probe sent masked nonfinal Text(`abc`) followed by final Continuation(`[F0,9F]`), and received `Ok("abc")`. This silently changes malformed input rather than rejecting it. The default h12tiny WebSocket reader uses this collector.
- **Recommendation:** Reject FIN when a text message still has pending UTF-8 bytes. Keep a regression covering incomplete suffixes both in the initial fragment and in the final continuation, alongside valid split code points.
- **Fix with:** root-cause
- **Effort:** S (hours)

### H12T-2 · P2 · A new fragmented message replaces unfinished content

- **Area:** Data
- **Where:** `crates/laputa-fastwebsockets-futures-lite/src/fragment.rs:119`, `crates/laputa-fastwebsockets-futures-lite/src/fragment.rs:134`, `crates/h12tiny-web/src/lib.rs:2240`
- **Problem:** The nonfinal Text/Binary branch overwrites the active fragmented message. A public-API probe sent nonfinal Binary(`original`), nonfinal Binary(`replacement`), then final Continuation(`!`), and received `Ok("replacement!")`. Only the final-frame branch currently rejects starting a second message. The default h12tiny collector therefore accepts invalid interleaving and discards the original content.
- **Recommendation:** Reject a new Text/Binary frame whenever a fragmented message is active, regardless of FIN. Test both text and binary restarts while preserving valid interleaved control frames.
- **Fix with:** root-cause
- **Effort:** S (hours)

### H12T-3 · P2 · Blocking TLS setup can outlive its connection timeout

- **Area:** Errors
- **Where:** `crates/h12tiny-client-sync/src/lib.rs:166`, `crates/h12tiny-client-sync/src/lib.rs:226`, `tests/client_sync_tls.rs:94`
- **Problem:** `connect_timeout` promises to bound the TLS handshake, but `complete_io` receives a socket with a fresh timeout for each read and write. A real-client probe trickled a valid Rustls handshake flight in 40 ms fragments with a 100 ms timeout. The request remained pending after 180 ms and returned a response-read error only after peer closure at 879 ms. The existing slow-TLS test checks dispatch after setup, leaving this handshake deadline unchecked.
- **Recommendation:** Use one monotonic deadline for TLS negotiation and recalculate remaining time before each socket operation, reusing the dispatch socket approach where suitable. Add a trickled-handshake regression that expects the timeout error before peer closure.
- **Fix with:** root-cause
- **Effort:** S (hours)

### H12T-4 · P2 · Raw request handlers cannot classify body-limit failures

- **Area:** Errors
- **Where:** `crates/h12tiny-web/src/lib.rs:291`, `crates/h12tiny-web/src/lib.rs:318`, `crates/h12tiny-web/src/lib.rs:358`, `crates/h12tiny-web/src/lib.rs:554`
- **Problem:** The router erases limit failures into private `LimitedBodyError`, which exposes neither its public `BodyLimitExceeded` nor an error source. It also converts underlying body failures to strings. Built-in extractors can inspect the private wrapper, while raw request handlers cannot reliably distinguish a 413 case from a transport failure. A public-API probe exceeded a one-byte route limit with three bytes: downcasting the returned error to `BodyLimitExceeded` failed, and no source was available.
- **Recommendation:** Preserve a public structured limit error and the original body error through type erasure. Verify a raw handler can classify and inspect a limit breach and still inspect an underlying transport error, without parsing Display text.
- **Fix with:** error-model
- **Effort:** S (hours)

### H12T-5 · P2 · Cancelling a WebSocket read loses parser state

- **Area:** Concurrency
- **Where:** `crates/laputa-fastwebsockets-futures-lite/src/lib.rs:337`, `crates/laputa-fastwebsockets-futures-lite/src/lib.rs:339`, `crates/laputa-fastwebsockets-futures-lite/src/lib.rs:388`, `crates/h12tiny-web/src/lib.rs:2118`
- **Problem:** Frame headers leave the persistent buffer before subsequent reads await. The decoded header exists only in the future, so dropping a pending read loses it. A controlled public-API probe delivered the first two bytes of a valid masked binary frame, cancelled the pending read, then delivered the rest. The next read returned `UnmaskedFrame`. Applications racing the default h12tiny reader against timers or outbound work can corrupt an otherwise valid session. The API does not state a cancellation restriction.
- **Recommendation:** Keep the frame header buffered until the full frame can be committed, or store parsing progress in `ReadHalf`. Test cancellation while awaiting extended length, mask, and payload bytes, followed by successful resumption of the original frame.
- **Fix with:** lifecycle-concurrency
- **Effort:** M (days)

### H12T-6 · P3 · Vectored WebSocket writes spin when the writer returns zero

- **Area:** Errors
- **Where:** `crates/laputa-fastwebsockets-futures-lite/src/frame.rs:312`, `crates/laputa-fastwebsockets-futures-lite/src/frame.rs:318`, `crates/laputa-fastwebsockets-futures-lite/src/lib.rs:434`
- **Problem:** A zero-byte vectored write leaves progress unchanged and immediately retries. A public-API probe supplied a legal `AsyncWrite` returning `Ok(0)` and observed eleven calls within one poll before an injected error stopped it. An unbounded writer would spin and prevent cancellation. The default writer selects this path for payloads above 1024 bytes. No real TCP zero-write case was reproduced, so the demonstrated impact is on generic writers accepted by the fork.
- **Recommendation:** Return `io::ErrorKind::WriteZero` when a nonempty vectored write makes no progress. Test zero progress before and partway through the header, plus successful partial writes.
- **Fix with:** error-model
- **Effort:** S (hours)

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
