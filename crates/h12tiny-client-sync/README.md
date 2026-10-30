# h12tiny-client-sync

Blocking direct-origin HTTP/1.1 client for h12tiny. It uses only `std::net` and,
with the optional `tls` feature, synchronous Rustls. There is no HTTP/2, executor,
connection pool, proxy policy, redirect policy, or background driver.

Dropping a response body closes its connection, so callers must not assume
connection reuse.

## Features

- `tls`: HTTPS with Rustls and operating-system trust from `h12tiny-tls-system`.

See the [client guide](../../docs/client.md).
