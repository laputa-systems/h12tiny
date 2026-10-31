Prefix: H12T
Next: 16

### H12T-12 · P1 · Unbounded retry, fail-open defaults, and deadline-free shutdown

- **Area:** Concurrency
- **Where:** `crates/h12tiny-client/src/lib.rs:637`, `crates/h12tiny-client/src/lib.rs:850-877`, `crates/h12tiny-client/src/lib.rs:1081-1085`, `crates/h12tiny-server/src/lifecycle.rs:357`, `crates/h12tiny-server/src/lifecycle.rs:372`, `crates/h12tiny-web/src/lib.rs:502`
- **Problem:** A repeatedly reset pooled connection retries with no cap, delay, or idempotency check; H1/H2 drivers are detached and their `Result` discarded, so driver death is invisible until the next `sender.ready()` failure. Defaults stack fail-open: `max_connections: None`, pool bounds `usize::MAX`, `RequestOptions` timeouts `None`, web body fallback `usize::MAX`. At cap the server drops the socket with no signal; any single `accept()` error drains all connections and ends the loop; post-shutdown each connection drains with no per-connection deadline, so one stalled stream blocks shutdown.
- **Recommendation:** Bound retry with cap plus backoff and safe-retry gating, report driver errors via the connection-error hook, add a production-minimums preset with a drain deadline and at-capacity signal. See the draft RFC for the owner decisions.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** M
- **Design:** `docs/rfc/H12T-12-production-bounds.md`

### H12T-14 · P2 · Client and body builders admit invalid states that fail at runtime

- **Area:** Types
- **Where:** `crates/h12tiny-client/src/lib.rs:1116-1123`, `crates/h12tiny-client/src/lib.rs:1159-1173`, `crates/h12tiny-client/src/connect.rs:593-597`, `crates/h12tiny-client/src/lib.rs:305-317`, `crates/h12tiny-util/src/lib.rs:156-159`, `crates/h12tiny-client-sync/src/lib.rs:789-800`
- **Problem:** `pool_max_connections_per_host(0)` means unlimited while adjacent `pool_max_idle_per_host` treats `0` as zero; `http1_only()` plus `http2_only(true)` combine into an H2-required pool over H1-only ALPN and fail at handshake; `Http2Settings` accepts any `u32`/`Duration` and silently drops inert settings; zero `Duration` timeouts are accepted and instantly expire (the test suite uses `Duration::ZERO` to force timeout); `ReaderStream::with_chunk_size` clamps only the bottom so `usize::MAX` is a one-call OOM; `BodyMode::Chunked` permits `complete: true` with `remaining > 0`.
- **Recommendation:** Split limited/unlimited into `Option` or `NonZero` types, make the protocol selector a single enum with reversible transitions, validate H2 settings at construction, reject or normalize zero timeouts consistently, bound chunk size, and separate the chunked-complete state.
- **Fix with:** `make-invalid-states-unrepresentable`
- **Effort:** M

### H12T-15 · P2 · TLS trust selection and failure signals are easy to misread

- **Area:** Security
- **Where:** `crates/h12tiny-tls-system/src/lib.rs:384-406`, `crates/h12tiny-tls-system/src/lib.rs:527-597`, `crates/h12tiny-client/src/lib.rs:425-485`
- **Problem:** `SSL_CERT_FILE`/`SSL_CERT_DIR` silently switch macOS from platform evaluation to PEM trust; env is read on every `capture()` and the builder does not surface which source won. `DebugEvent` is explicitly not a metrics framework and trust-reload failures surface as generic strings, so operators cannot alert on store vs network vs pool causes. Non-Unix `file_identity()` returns `None`, leaving only mtime/length; a preserved-mtime replacement there is undetected until restart. The same Graviola plus system-trust plus ALPN defaults are copy-pasted across client, sync client, and tls-system.
- **Recommendation:** Expose which trust source won, add a minimal error taxonomy plus caller hooks for reload/pool/handshake causes, tighten or document the non-Unix fingerprint guarantee, and unify the TLS default constructors into one shared policy.
- **Fix with:** `security`
- **Effort:** M

### H12T-16 · P3 · Topology, versions, and limits are stated in several places

- **Area:** Config
- **Where:** `Cargo.toml:53-75`, `docs/features.md:8-36`, `README.md:61-67`, `scripts/check-features.sh:47-170`, `scripts/check-msrv.sh:50-70`, `crates/h12tiny-web/Cargo.toml:24`
- **Problem:** Facade flags live in the manifest, two prose tables, and a shell-script oracle that is the only executable truth; the README table already hides the `upgrade`-implies-`server` auto-selection. MSRV is split 1.85/1.89 across manifests plus a hand-maintained script list plus prose. Timeouts and limits are magic numbers in five crates with no limits table. `base64` resolves to two majors (`0.22` workspace vs `0.23.1` in web). README facade version was `0.3` against manifest `0.6.0` (fixed in this pass).
- **Recommendation:** Generate the feature/MSRV tables from manifests or collapse to one table plus the script; add one documented limits table; unify on one `base64` major with a duplicate-version lint.
- **Fix with:** `single-source-of-truth`
- **Effort:** S
