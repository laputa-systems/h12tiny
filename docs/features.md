# Feature selection

All facade and component crates default to no optional roles or protocols.
Choose a role, then enable only the protocols and conveniences it needs.

## Facade features

| Feature | Effect |
| --- | --- |
| `core`, `client`, `client-sync`, `server`, `util`, `web` | Select the corresponding component; `client` and `server` also select core, and `web` also selects server and util |
| `http1`, `http2`, `tls` | Forward to already-selected async client/server roles and, for TLS, the selected blocking client |
| `json` | Selects util JSON and forwards it to web if selected |
| `query`, `sse`, `cors` | Selects the corresponding web convenience |
| `upgrade` | Selects `server` and `http1`, plus server HTTP/1.1 raw upgrade support and the same support in web when selected |
| `websocket` | Selects `web` and `upgrade`, so it also selects `server`, `util`, and `http1`, plus the RFC 6455 adapter |
| `websocket-client` | Selects `web` and `core`, plus the client-role RFC 6455 opening handshake over a caller-dialed stream |
| `full` | Explicitly selects all components and optional features |

Protocol features do not select a role by themselves. For example, `http1`
alone does not add a client or server. `web` does not select H1, H2, or TLS.
`upgrade` and `websocket` are role-selecting conveniences rather than
protocol flags.

## Component features

`h12tiny-client` and `h12tiny-server` default to no protocol. Their
`http1`, `http2`, and `tls` features are independent. The blocking client
is intrinsically HTTP/1.1; its optional `tls` feature adds synchronous Rustls
only.

The util crate's `json` feature is optional. Web `json`, `query`, `sse`,
`cors`, `upgrade`, `websocket`, and `websocket-client` are optional;
`websocket` depends on the HTTP/1.1 upgrade surface, and `websocket-client`
selects Hyper's HTTP/1.1 client without the server crate. HTTP/2 extended CONNECT is not implemented.

Feature isolation is part of the observable contract: an H1-only build does
not acquire H2, a client-only build does not acquire server/web, plain util
does not acquire JSON, and plain web does not acquire JSON, query, or
WebSocket support. Run [the feature check](../scripts/check-features.sh) after
changing this topology.
