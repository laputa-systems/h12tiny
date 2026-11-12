Prefix: H12T
Next: 27

### H12T-18 · P1 · H2 read-ahead can reenter occupied control-reply state

- **Area:** Concurrency
- **Where:** `crates/laputa-h2-futures-lite/src/proto/connection.rs:223`, `:386`, `:419`, `:637`; `crates/laputa-h2-futures-lite/src/proto/ping_pong.rs:97`, `:147`; `crates/laputa-h2-futures-lite/src/proto/settings.rs:81`, `:123`; `crates/laputa-h2-futures-lite/src/codec/framed_write.rs:236`, `:305`, `:352`.
- **Problem:** The control-output backpressure path continues reading frames, but the PING and SETTINGS handlers still assume their previous acknowledgment has been buffered. When the codec has no send capacity, a pending PONG or remote SETTINGS remains occupied. Another control frame reaches an unconditional assertion in the corresponding handler. A blocked socket alone is insufficient: the codec must also lack buffering capacity, as it does with a pending chained DATA frame. The read side does not wait for that write capacity. This can panic a connection driver and terminate an application using abort-on-panic. Two independent source reviews traced the dispatch and codec predicates. **Runtime reachability is unproven by execution; no wire trigger was run.** Existing multiple-PING and bidirectional-progress tests cover the two conditions separately.
- **Recommendation:** Give pending control responses one bounded owner that preserves acknowledgment and SETTINGS ordering while allowing the progress required by the bidirectional-deadlock regression. Do not restore the old unconditional stop-reading behavior or merely remove the assertions. Verify multiple control obligations under constrained output together with the existing progress regression.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** M (days)
- **Design:** [Bounded H2 control responses](docs/rfc/H12T-18-h2-control-responses.md).

### H12T-19 · P1 · The benchmark workspace no longer resolves the local facade

- **Area:** Build
- **Where:** `benchmarks/twelve-dollar/Cargo.toml:15`, `benchmarks/twelve-dollar/Cargo.lock:365`, `benchmarks/twelve-dollar/build.sh:6`, `Cargo.toml:3`, `scripts/bump_minor_release.sh:450`.
- **Problem:** The benchmark workspace requires the local `h12tiny` path at `^0.8.0`, while the facade is `0.9.0`. Cargo enforces both the path and the version requirement. An isolated manifest copying that dependency declaration and pointing to the unchanged facade fails with `failed to select a version for the requirement h12tiny = "^0.8.0"`, reporting `0.9.0` as the nonmatching candidate. The full benchmark check was separately blocked by uncached `hmac` in offline mode; the isolated resolution check establishes the version defect without that unrelated dependency. The release script discovers only root workspace members, so excluded in-repository consumers do not move with a minor release.
- **Recommendation:** Update the benchmark dependency and its lockfile. Give this non-published local consumer a dependency policy that survives facade releases, or include its requirement and lockfile in release planning. Verify the benchmark's locked build after dependency resolution is available.
- **Fix with:** `single-source-of-truth`
- **Effort:** S (hours)

### H12T-20 · P2 · Router composition silently replaces configured child state

- **Area:** Architecture
- **Where:** `crates/h12tiny-web/src/lib.rs:1147`, `:1224`, `:1253`, `:1335`.
- **Problem:** `nest` and `merge` flatten endpoints into the parent but copy child state only when the parent has none. Subsequent configured children lose their state, and every endpoint receives the one retained parent value. A public-API, in-memory check nested routers configured with `state-a` and `state-b` under `/a` and `/b`; both returned HTTP 200 with `state-a`. The API describes shared state but does not reject or document these conflicting bindings. The same composition code also drops child CORS and promotes a child fallback globally; those policy behaviors were source-inspected, not separately reproduced, and belong in the same composition contract decision.
- **Recommendation:** Preserve state bound to child endpoints, or make state binding a single explicit construction step that rejects conflicting configured routers. Define nested fallback and CORS ownership alongside state so composition cannot silently discard a supplied policy. Verify differently configured children through both `nest` and `merge`.
- **Fix with:** `api-surface-and-module-boundaries`
- **Effort:** M (days)
- **Design:** [Router composition ownership](docs/rfc/H12T-20-router-composition.md).

### H12T-21 · P2 · Blocking chunk decoding loses framing progress after a read timeout

- **Area:** Data
- **Where:** `crates/h12tiny-client-sync/src/lib.rs:515`, `:555`, `:578`.
- **Problem:** `read_chunked` consumes the separator after a chunk while leaving `separator_due` set. If reading the next size line returns a recoverable timeout, the next `Read::read` consumes the separator again and rejects valid input. A bounded loopback check delivered one valid chunk, paused using a channel while a 30 ms body-read timeout elapsed, and then delivered the next valid chunk and terminator. The retry returned `InvalidData: HTTP chunk data was not followed by CRLF`. Partial separators and the zero-chunk/trailer path also keep progress only in stack locals. The check used the real public client and joined its fixture thread.
- **Recommendation:** Persist separator, size-line, data, and trailer phases, including partial framing progress and trailer byte accounting. Advance state as bytes are consumed. Verify retry after a recoverable read error between chunks, after the first separator byte, and during trailers through `ResponseBody`.
- **Fix with:** `make-invalid-states-unrepresentable`
- **Effort:** M (days)

### H12T-22 · P2 · A canceled WebSocket Close can report success without sending bytes

- **Area:** Concurrency
- **Where:** `crates/laputa-fastwebsockets-futures-lite/src/lib.rs:161`, `:282`, `:503`; `crates/h12tiny-web/src/lib.rs:2243`.
- **Problem:** The writer sets `closed` before awaiting the Close frame's first write. Canceling while the transport is pending before any bytes are accepted leaves that flag set. Retrying Close then returns `Ok(())` without writing anything. An in-memory check against the actual fork observed `retry_result=Ok(()) writer_closed=true emitted_bytes=0`. This also defeats the reader's retained automatic-close reply: its callback can now falsely acknowledge delivery and clear the obligation. No partial frame was emitted in the reproduced case, so it does not violate the documented callback requirement against unacknowledged partial writes.
- **Recommendation:** Distinguish a pending Close from a fully transmitted Close and retain write progress across cancellation, or return an explicit terminal failure when transmission cannot safely resume. Do not acknowledge an unsent frame. Cover both zero-byte and partial-write cancellation; moving the boolean below the await alone does not settle the latter.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** M (days)

### H12T-23 · P2 · Query extraction guesses types before consulting the destination type

- **Area:** Data
- **Where:** `crates/h12tiny-web/src/lib.rs:1536`, `:1560`, `:1592`.
- **Problem:** URL-decoded strings are guessed to be JSON booleans, nulls, integers, or floats before deserialization. An in-memory public-router check accepts `text=hello` for a `String`, but returns HTTP 400 for `text=123`, `text=00123`, `text=true`, and `text=null`. For `Option<String>`, explicitly supplied `text=null` silently becomes `None`. Search text and identifiers therefore change meaning or fail according to their spelling. Existing query tests exercise numeric fields and alphabetic strings, not this distinction.
- **Recommendation:** Deserialize each decoded value according to the requested field type. Preserve exact string spellings and missing-versus-supplied optional values while retaining numeric and boolean fields. Exercise leading zeroes and scalar-looking text through the public extractor.
- **Fix with:** `typing-audit`
- **Effort:** M (days)

### H12T-24 · P2 · Fallible system-trust TLS builders panic on unsupported configuration

- **Area:** Errors
- **Where:** `crates/h12tiny-tls-system/src/lib.rs:438`, `:631`; `crates/h12tiny-client/src/connect.rs:370`, `:412`.
- **Problem:** The default system-trust/no-client-auth branch calls `expect` on Rustls protocol/provider validation even though `build()` returns `Result` and the client wrapper promises a configuration error for an empty protocol list. A local construction-only check of `.protocol_versions([]).build()` caught the panic at line 448. The same selection with explicit roots returned the expected `Err`. No handshake or remote peer was involved. Equivalent client-auth and explicit-root branches already map unsupported configurations to a typed error.
- **Recommendation:** Propagate configuration errors from the `Result`-returning system-trust builders as `SystemTrustErrorKind::Unsupported`, consistently with the other trust/auth branches. Keep the public lazy-constructor signature separate unless an API change is explicitly chosen. Verify empty and provider-incompatible version selections across trust/auth variants.
- **Fix with:** `error-model`
- **Effort:** S (hours)

### H12T-25 · P2 · Production listener deadlines exclude connection setup

- **Area:** Concurrency
- **Where:** `crates/h12tiny-server/src/lifecycle.rs:94`, `:332`, `:603`, `:724`; `crates/h12tiny-server/src/conn/auto.rs:447`, `:705`; `tests/server_bounds.rs:30`.
- **Problem:** The production preset's 10 s header deadline is installed on the eventual Hyper H1 driver. Cleartext protocol detection runs before that driver exists, and TLS acceptance races only against server shutdown. These pending setup states consume admission slots with no elapsed-time bound. The existing header-timeout test supplies a recognizable request prefix, so it starts after the uncovered boundary. **Source-traced; runtime expiry behavior is unproven by execution.** No connection flood or external-service probe was run. The earlier production-bounds work remains implemented; this is a narrower setup-stage gap.
- **Recommendation:** Bound TLS acceptance and protocol detection in the production lifecycle, release the admission slot on expiry, and report a typed timeout. Keep setup separate from established-stream lifetime and shutdown draining. Verify with a controlled pending-I/O fixture and a positive completed-setup case.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** M (days)
- **Design:** [Connection setup deadlines](docs/rfc/H12T-25-connection-setup-deadline.md).

### H12T-26 · P2 · TLS session resumption can retain approval across system-trust changes

- **Area:** Security
- **Where:** `crates/h12tiny-tls-system/src/lib.rs:202`, `:351`, `:438`, `:487`; pinned `rustls 0.23.46` sources `src/client/builder.rs:172`, `src/client/tls13.rs:573`, `src/msgs/persist.rs:273`.
- **Problem:** Managed system-trust configurations retain Rustls's default resumption cache. Store refresh runs when the certificate verifier is called, but a resumed TLS 1.3 handshake reuses the session's previous verification result. Rustls compares verifier identity, and the outer `SystemVerifier` identity stays the same when its internal roots change. A client reusing its configuration with a resumable peer can therefore establish another connection without checking a changed or unusable store. This contradicts the earlier promise that every new handshake observes current trust; the TLS guide now states the implementation's limit. **Source-traced through the pinned dependency; the end-to-end trust-change case is unproven and was not exercised.** The affected asset is the client's current trust policy after an administrator changes it, not established connections already allowed to finish.
- **Recommendation:** Preserve strict trust freshness by disabling resumption for managed system trust, or invalidate cached sessions before any resume attempt when that policy changes. Keep explicit-root and caller-supplied configuration policy separate. Settle the performance/freshness choice in the RFC before implementing it.
- **Fix with:** `security`
- **Effort:** M (days)
- **Design:** [System trust and session resumption](docs/rfc/H12T-26-trust-resumption.md).
