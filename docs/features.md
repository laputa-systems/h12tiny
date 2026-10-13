# Feature selection

All facade and component crates default to no optional roles or protocols.
Choose a role, then enable only the protocols and conveniences it needs.

## Facade features

| Feature | Effect |
| --- | --- |
| `client`, `client-sync`, `server`, `util`, `web` | Select the corresponding component; `web` also selects server and util |
| `http1`, `http2`, `tls` | Forward to already-selected async client/server roles and, for TLS, the selected blocking client |
| `json` | Selects util JSON and forwards it to web if selected |
| `query`, `sse`, `cors` | Selects the corresponding web convenience |
| `upgrade` | Selects server HTTP/1.1 raw upgrade support |
| `websocket` | Selects web, raw HTTP/1.1 upgrade, and the RFC 6455 adapter |
| `full` | Explicitly selects all components and optional features |

Protocol features do not select a role by themselves. For example, `http1`
alone does not add a client or server. `web` does not select H1, H2, or TLS.

## Component features

`h12tiny-client` and `h12tiny-server` default to no protocol. Their
`http1`, `http2`, and `tls` features are independent. The blocking client
is intrinsically HTTP/1.1; its optional `tls` feature adds synchronous Rustls
only.

The util crate's `json` feature is optional. Web `json`, `query`, `sse`,
`cors`, `upgrade`, and `websocket` are optional; `websocket` depends on
the HTTP/1.1 upgrade surface. HTTP/2 extended CONNECT is not implemented.

Feature isolation is part of the observable contract: an H1-only build does
not acquire H2, a client-only build does not acquire server/web, plain util
does not acquire JSON, and plain web does not acquire JSON, query, or
WebSocket support. Run [the feature check](../scripts/check-features.sh) after
changing this topology.
