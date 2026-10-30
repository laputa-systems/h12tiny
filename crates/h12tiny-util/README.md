# h12tiny-util

Protocol-neutral HTTP body conveniences for h12tiny, including bounded
collection and streaming helpers. It knows nothing about DNS, TCP, TLS, client
pools, server listeners, or routing.

## Features

- `json`: JSON bodies through `miniserde`. This is the only serialization surface.

See the [application layer guide](../../docs/application.md).
