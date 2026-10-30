# h12tiny-client-normalize

Direct-origin request normalization shared by the h12tiny client transports.
An absolute URI chooses the peer; HTTP/1 receives an origin-form request target
and an explicit `Host` header. Keeping these rules in one transport-free crate
stops the blocking and futures-I/O clients from diverging on default ports,
IPv6 authorities, or `CONNECT` targets.

This crate is an implementation detail of `h12tiny-client` and
`h12tiny-client-sync`. See the [client guide](../../docs/client.md).
