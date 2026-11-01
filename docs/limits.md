# Limits and timeouts

Concrete bounds owned by the h12tiny crates. The value is a default unless the
row says it is fixed. A default of "none" means the bound is off or absent
until the application configures one. Source paths name the owning module;
this page is not a second source for their values.

## Async client (`h12tiny-client`)

| Bound | Default | Source | Configuration |
| --- | --- | --- | --- |
| Pool idle timeout | 90 s | `crates/h12tiny-client/src/lib.rs` | `Builder::pool_idle_timeout`; `None` disables idle eviction, zero is rejected |
| Stale-connection retries | 2 retries, 10 ms backoff | `crates/h12tiny-client/src/lib.rs` | `Builder::max_stale_retries`; only safe/idempotent methods with an empty body over a reused connection |
| Idle-pool sweep interval | matches the idle timeout, with a 90 ms minimum | `crates/h12tiny-client/src/pool.rs` | Internal; with the 90 s default, eviction runs every 90 s |
| Happy-eyeballs delay | 250 ms | `crates/h12tiny-client/src/connect.rs` | `ConnectorBuilder::happy_eyeballs_timeout`; `None` disables staggered dialing |
| Connector connect timeout | none | `crates/h12tiny-client/src/connect.rs` | `ConnectorBuilder::connect_timeout`; zero is rejected |
| Per-request DNS, TCP, TLS, and headers timeouts | none | `crates/h12tiny-client/src/lib.rs` | `RequestOptions`; zero is rejected, and there is no whole-request deadline |
| Idle connections per origin | unlimited | `crates/h12tiny-client/src/lib.rs` | `Builder::pool_max_idle_per_host` takes a nonzero limit or `None`; `disable_pool` disables reuse |
| Open HTTP/1.1 connections per origin | unlimited | `crates/h12tiny-client/src/lib.rs` | `Builder::pool_max_connections_per_host` takes a nonzero limit or `None`; the bound applies only while pooling is enabled |
| HTTP/2 stream and connection windows | Hyper defaults | `crates/h12tiny-client/src/lib.rs` | `Http2Settings`; values above 2^31 - 1 are rejected |
| HTTP/2 maximum frame size | Hyper default | `crates/h12tiny-client/src/lib.rs` | `Http2Settings::max_frame_size`; permitted range is 16384..=16777215 |
| HTTP/2 keep-alive | disabled | `crates/h12tiny-client/src/lib.rs` | `Http2Settings`; interval and timeout must be nonzero, and timeout or idle policy requires an interval |

`Builder::production` sets 32 open and 16 idle connections per origin, two
stale retries, and 10 s DNS/TCP/TLS plus 30 s header deadlines.

## Blocking client (`h12tiny-client-sync`)

| Bound | Default | Source | Configuration |
| --- | --- | --- | --- |
| Response head bytes | 64 KiB | `crates/h12tiny-client-sync/src/lib.rs` | Fixed; not configurable |
| Chunk line bytes | 8 KiB | `crates/h12tiny-client-sync/src/lib.rs` | Fixed; not configurable |
| Trailer section bytes | 64 KiB | `crates/h12tiny-client-sync/src/lib.rs` | Fixed; not configurable |
| Connect timeout | none | `crates/h12tiny-client-sync/src/lib.rs` | `Builder::connect_timeout`; zero is rejected, and DNS runs through `ToSocketAddrs` without preemption |
| Request write plus response-header deadline | none | `crates/h12tiny-client-sync/src/lib.rs` | `Client::request_with_timeout`; not a whole-response deadline, zero is rejected |
| Body read timeout | none | `crates/h12tiny-client-sync/src/lib.rs` | `ResponseBody::set_read_timeout`, applied per read; the platform rejects zero |

## Body utilities (`h12tiny-util`)

| Bound | Default | Source | Configuration |
| --- | --- | --- | --- |
| `ReaderStream` chunk size | 16 KiB | `crates/h12tiny-util/src/lib.rs` | `ReaderStream::with_chunk_size`; clamped to 1..=16 MiB by `MAX_READER_STREAM_CHUNK_SIZE` |
| Body collection limit | none; the caller supplies it | `crates/h12tiny-util/src/lib.rs` | `collect_bytes_limited` and `ResponseBodyExt` take the limit from the caller |
| Body idle timeout | none; the caller supplies it | `crates/h12tiny-util/src/lib.rs` | `IdleTimeoutBody::new` or `IdleTimeoutExt::with_idle_timeout` |

## Server (`h12tiny-server`)

| Bound | Default | Source | Configuration |
| --- | --- | --- | --- |
| Active connection drivers | unlimited (no limit) | `crates/h12tiny-server/src/lifecycle.rs` | `Serve::max_connections` and `TlsServe::max_connections`; zero rejects every accepted stream. `production()` uses 1024 |
| HTTP/1 header-read timeout | none unless a timer and timeout are set | `crates/h12tiny-server/src/lifecycle.rs`, `crates/h12tiny-server/src/conn/auto.rs` | `Serve::production()` installs a timer and 10 s timeout; `with_bounds` takes explicit values |
| At-capacity rejection | drop the accepted stream | `crates/h12tiny-server/src/lifecycle.rs` | `on_connection_rejected` observes the drop |
| Transient accept retry | retry with 10 ms backoff | `crates/h12tiny-server/src/lifecycle.rs` | Internal; fatal errors stop the loop |
| Graceful drain deadline | none | `crates/h12tiny-server/src/lifecycle.rs` | Drains until active connections finish; h12tiny applies no timer |
| Server request body limit | none imposed by h12tiny | `crates/h12tiny-server/src/lifecycle.rs` | The service receives `hyper::body::Incoming`; the application owns its body limit |

## Web (`h12tiny-web`)

| Bound | Default | Source | Configuration |
| --- | --- | --- | --- |
| Request body limit | none, collected as `usize::MAX` | `crates/h12tiny-web/src/lib.rs` | `Router::body_limit` or `MethodRouter::body_limit`; a fallback handler starts with no limit |
| Handler deadline | none | `crates/h12tiny-web/src/lib.rs` | `Router::timeout` or `MethodRouter::timeout`; `without_timeout` clears it |
| SSE keepalive interval | 15 s | `crates/h12tiny-web/src/lib.rs` | `KeepAlive::interval`; a zero interval is rejected |
