use std::error::Error as StdError;
use std::fmt;
use std::pin::pin;

use base64::Engine as _;
use bytes::Bytes;
use fastwebsockets::{after_handshake_split, FragmentCollectorRead, Role};
use futures_util::future::{select, Either};
use h12tiny_core::io::{FuturesIo, HyperIo};
use http::header::{CONNECTION, HOST, UPGRADE};
use http::{StatusCode, Uri};
use http_body_util::Empty;

use crate::{websocket_accept, WebSocketConnection};

/// A client-role RFC 6455 opening handshake that did not produce a connection.
#[derive(Debug)]
pub enum WebSocketConnectError {
    /// The URI's scheme is not `ws` or `wss`.
    Scheme,
    /// The URI has no authority to send as `Host`.
    MissingAuthority,
    /// The URI or a generated header is not valid HTTP.
    Request(http::Error),
    /// The operating system could not supply a random `Sec-WebSocket-Key`.
    Key(getrandom::Error),
    /// The HTTP/1.1 exchange or the upgrade failed.
    Http(hyper::Error),
    /// The peer answered with a status other than `101 Switching Protocols`.
    Status(StatusCode),
    /// The `101` response lacks `Upgrade: websocket` or `Connection: Upgrade`.
    UpgradeHeaders,
    /// `Sec-WebSocket-Accept` is absent or does not match the sent key.
    Accept,
}

impl fmt::Display for WebSocketConnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scheme => formatter.write_str("WebSocket URIs use the ws or wss scheme"),
            Self::MissingAuthority => formatter.write_str("WebSocket URIs require an authority"),
            Self::Request(error) => write!(formatter, "invalid WebSocket request: {error}"),
            Self::Key(error) => write!(formatter, "no randomness for Sec-WebSocket-Key: {error}"),
            Self::Http(error) => write!(formatter, "WebSocket handshake failed: {error}"),
            Self::Status(status) => write!(formatter, "WebSocket handshake answered {status}"),
            Self::UpgradeHeaders => formatter
                .write_str("WebSocket handshake response lacks Upgrade: websocket and Connection: Upgrade"),
            Self::Accept => formatter.write_str("Sec-WebSocket-Accept does not match the sent key"),
        }
    }
}

impl StdError for WebSocketConnectError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Request(error) => Some(error),
            Self::Http(error) => Some(error),
            _ => None,
        }
    }
}

/// Run the client side of an RFC 6455 opening handshake over an established
/// stream and return a framed client-role connection.
///
/// The caller owns dialing, TLS for `wss`, and any deadline: drop the future
/// to abandon the handshake. `uri` supplies the request target and the `Host`
/// header; no `Origin`, subprotocol, or extension is offered. Every frame the
/// returned writer emits is masked, and the reader collects fragmented
/// messages up to the parser's default message limit.
pub async fn connect_websocket<T>(
    io: T,
    uri: &Uri,
) -> Result<WebSocketConnection, WebSocketConnectError>
where
    T: futures_lite::io::AsyncRead + futures_lite::io::AsyncWrite + Unpin + Send + 'static,
{
    if !matches!(uri.scheme_str(), Some("ws" | "wss")) {
        return Err(WebSocketConnectError::Scheme);
    }
    let authority = uri
        .authority()
        .ok_or(WebSocketConnectError::MissingAuthority)?;
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce).map_err(WebSocketConnectError::Key)?;
    let key = base64::engine::general_purpose::STANDARD.encode(nonce);
    let target = uri.path_and_query().map_or("/", |target| target.as_str());
    let request = http::Request::get(target)
        .header(HOST, authority.as_str())
        .header(CONNECTION, "Upgrade")
        .header(UPGRADE, "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", &key)
        .body(Empty::<Bytes>::new())
        .map_err(WebSocketConnectError::Request)?;

    let (mut sender, connection) = hyper::client::conn::http1::handshake(FuturesIo::new(io))
        .await
        .map_err(WebSocketConnectError::Http)?;
    let mut connection = pin!(connection.with_upgrades());
    let send = pin!(sender.send_request(request));
    let (response, connection_finished) = match select(send, connection.as_mut()).await {
        Either::Left((response, _)) => (response, false),
        Either::Right((Ok(()), send)) => (send.await, true),
        Either::Right((Err(error), _)) => return Err(WebSocketConnectError::Http(error)),
    };
    let mut response = response.map_err(WebSocketConnectError::Http)?;
    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
        return Err(WebSocketConnectError::Status(response.status()));
    }
    let headers = response.headers();
    if !has_token(headers, UPGRADE, "websocket") || !has_token(headers, CONNECTION, "upgrade") {
        return Err(WebSocketConnectError::UpgradeHeaders);
    }
    let expected = websocket_accept(&key);
    if headers
        .get("sec-websocket-accept")
        .is_none_or(|accept| accept.as_bytes() != expected.as_bytes())
    {
        return Err(WebSocketConnectError::Accept);
    }
    let on_upgrade = hyper::upgrade::on(&mut response);
    if !connection_finished {
        connection.await.map_err(WebSocketConnectError::Http)?;
    }
    let upgraded = on_upgrade.await.map_err(WebSocketConnectError::Http)?;
    let (read, write) = futures_lite::io::split(HyperIo::new(upgraded));
    let (reader, writer) = after_handshake_split(read, write, Role::Client);
    Ok(WebSocketConnection {
        reader: FragmentCollectorRead::new(reader),
        writer,
    })
}

fn has_token(headers: &http::HeaderMap, name: http::header::HeaderName, expected: &str) -> bool {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case(expected))
}
