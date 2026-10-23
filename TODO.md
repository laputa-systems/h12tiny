Prefix: H12T
Next: 10

### H12T-9 · P3 · Cancelling an automatic reply discards the reply obligation

- **Area:** Concurrency
- **Where:** `crates/laputa-fastwebsockets-futures-lite/src/lib.rs:153`, `crates/laputa-fastwebsockets-futures-lite/src/fragment.rs:80`
- **Problem:** Both readers consume a Ping or Close before awaiting the automatic-reply callback. Cancelling that await drops the local reply frame. A public-API probe supplied a masked Ping followed by data, stalled the pong callback before writing, cancelled the read, then resumed with a ready callback. The next data frame arrived with zero resumed reply callbacks. A peer can therefore miss its pong or close acknowledgement when an application races reads against other work. The documented input-byte cancellation guarantee does not cover this separate callback await.
- **Recommendation:** Keep pending reply ownership in the reader until acknowledgement and define the cancellation requirements on callbacks, including partially written replies. Retain a stalled-callback regression proving that cancellation before any output cannot silently discard a reply.
- **Fix with:** lifecycle-concurrency
- **Effort:** M (days)
