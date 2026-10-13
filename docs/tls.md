# TLS configuration

TLS is selected with the `tls` feature on the client, server, or blocking
client role. The blocking client uses only synchronous Rustls and remains
HTTP/1.1-only.

The async and blocking client configuration builders choose Graviola explicitly
and start with WebPKI roots. The async client offers its compiled protocols,
preferring `h2` when both are enabled;
the blocking client offers only `http/1.1`. Client builders can replace the
root store, add a root, configure client authentication, or select protocol
versions. The async client can also receive a caller-built Rustls config.

`http1_only()` restricts the client builder's ALPN to HTTP/1.1, including a
caller-built TLS configuration. Other custom ALPN offerings remain explicit
caller policy.

Server applications provide their own Rustls server configuration and
`TlsAcceptor`; advertise `h2` and `http/1.1` when serving both protocols.
Select a crypto provider per Rustls configuration. h12tiny does not install or
consult a process-global provider.

The normal dependency check rejects the `ring`, AWS-LC, OpenSSL, and native-TLS
backends. Rustls certificate validation remains part of the TLS stack.
See [dependency policy](architecture.md#dependency-constraints).
