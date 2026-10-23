Prefix: H12T
Next: 9

### H12T-7 · P3 · Client WebSocket masks use a noncryptographic generator

- **Area:** Security
- **Where:** `crates/laputa-fastwebsockets-futures-lite/src/frame.rs:250`, `crates/laputa-fastwebsockets-futures-lite/src/lib.rs:419`
- **Problem:** Client-role writes generate masking keys with `fastrand::u32`, whose implementation documents that it is not cryptographically secure. This does not meet the unpredictable-key requirement in [RFC 6455 section 5.3](https://www.rfc-editor.org/rfc/rfc6455#section-5.3). The standalone fork exports `Role::Client`; h12tiny's wrapper uses only `Role::Server`. Concrete next-key prediction or exploitation was not demonstrated.
- **Recommendation:** Obtain client masking keys from an OS-backed cryptographic random source, propagate acquisition errors, and remove fastrand if it has no remaining production use. Verify client frames remain masked and server frames remain unmasked.
- **Fix with:** security
- **Effort:** S (hours)
