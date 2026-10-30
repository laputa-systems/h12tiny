# h12tiny-core

Shared low-level facilities for the h12tiny component crates: the futures-I/O
bridge and the runtime-neutral executor and timer boundary. It has no client
policy, server accept loop, TLS, routing, JSON, or application abstractions.

Applications normally use the `h12tiny` facade instead of this crate. See
[architecture](../../docs/architecture.md) for crate ownership.
