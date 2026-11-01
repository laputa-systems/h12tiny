Prefix: H12T
Next: 17

### H12T-12 · P1 · Unbounded retry, fail-open defaults, and deadline-free shutdown

- **Area:** Concurrency
- **Where:** `crates/h12tiny-client/src/lib.rs:637`, `crates/h12tiny-client/src/lib.rs:850-877`, `crates/h12tiny-client/src/lib.rs:1081-1085`, `crates/h12tiny-server/src/lifecycle.rs:357`, `crates/h12tiny-server/src/lifecycle.rs:372`, `crates/h12tiny-web/src/lib.rs:502`
- **Problem:** A repeatedly reset pooled connection retries with no cap, delay, or idempotency check; H1/H2 drivers are detached and their `Result` discarded, so driver death is invisible until the next `sender.ready()` failure. Defaults stack fail-open: `max_connections: None`, pool bounds `usize::MAX`, `RequestOptions` timeouts `None`, web body fallback `usize::MAX`. At cap the server drops the socket with no signal; any single `accept()` error drains all connections and ends the loop; post-shutdown each connection drains with no per-connection deadline, so one stalled stream blocks shutdown.
- **Recommendation:** Bound retry with cap plus backoff and safe-retry gating, report driver errors via the connection-error hook, add a production-minimums preset with a drain deadline and at-capacity signal. See the draft RFC for the owner decisions.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** M
- **Design:** `docs/rfc/H12T-12-production-bounds.md`

### H12T-15 · P2 · TLS trust selection and failure signals are easy to misread

- **Area:** Security
- **Where:** `crates/h12tiny-tls-system/src/lib.rs:384-406`, `crates/h12tiny-tls-system/src/lib.rs:527-597`, `crates/h12tiny-client/src/lib.rs:425-485`
- **Problem:** `SSL_CERT_FILE`/`SSL_CERT_DIR` silently switch macOS from platform evaluation to PEM trust; env is read on every `capture()` and the builder does not surface which source won. `DebugEvent` is explicitly not a metrics framework and trust-reload failures surface as generic strings, so operators cannot alert on store vs network vs pool causes. Non-Unix `file_identity()` returns `None`, leaving only mtime/length; a preserved-mtime replacement there is undetected until restart. The same Graviola plus system-trust plus ALPN defaults are copy-pasted across client, sync client, and tls-system.
- **Recommendation:** Expose which trust source won, add a minimal error taxonomy plus caller hooks for reload/pool/handshake causes, tighten or document the non-Unix fingerprint guarantee, and unify the TLS default constructors into one shared policy.
- **Fix with:** `security`
- **Effort:** M


