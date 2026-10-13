//! Direct-origin request normalization shared by h12tiny client transports.
//!
//! This crate has no transport dependency. It owns the stable direct-origin
//! boundary: an absolute URI chooses the peer, while HTTP/1 receives an
//! origin-form request target and an explicit `Host` header. Keeping these
//! rules here prevents the blocking and futures-I/O clients from diverging on
//! default ports, IPv6 authorities, or `CONNECT` targets.

use std::fmt;

use http::header::{HeaderValue, HOST};
use http::uri::{Authority, Scheme};
use http::{Method, Request, Uri};

/// Direct-origin identity from the parsed URI scheme and authority.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Origin {
    scheme: Scheme,
    authority: Authority,
}

impl Origin {
    /// Returns the connection scheme.
    pub fn scheme(&self) -> &Scheme {
        &self.scheme
    }

    /// Returns the parsed authority, retaining an explicitly supplied port.
    pub fn authority(&self) -> &Authority {
        &self.authority
    }

    /// Returns this origin as an absolute URI with the root request target.
    pub fn uri(&self) -> Uri {
        http::uri::Builder::new()
            .scheme(self.scheme.clone())
            .authority(self.authority.clone())
            .path_and_query("/")
            .build()
            .expect("an existing origin is a valid URI")
    }

    /// Formats the stable direct-origin identity for diagnostics.
    pub fn display(&self) -> String {
        format!("{}://{}", self.scheme, self.authority)
    }
}

/// Failure to derive a direct-origin transport target from a request URI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A direct-origin request needs an absolute URI to identify its peer.
    AbsoluteUriRequired,
    /// An explicit authority port is empty, malformed, or outside `u16` range.
    InvalidPort,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AbsoluteUriRequired => "client requests require an absolute URI",
            Self::InvalidPort => "URI authority contains an invalid port",
        })
    }
}

impl std::error::Error for Error {}

/// Determines the direct-origin identity before a request is converted to the
/// HTTP/1 wire representation.
///
/// A `CONNECT` authority target is accepted and assigned `https` only when it
/// names port 443; other authority targets use `http` to select the transport.
pub fn extract_origin(uri: &mut Uri, is_connect: bool) -> Result<Origin, Error> {
    let original = uri.clone();
    if let Some(authority) = original.authority() {
        validate_port(authority)?;
    }
    match (original.scheme(), original.authority()) {
        (Some(scheme), Some(authority)) => Ok(Origin {
            scheme: scheme.clone(),
            authority: authority.clone(),
        }),
        (None, Some(authority)) if is_connect => {
            let scheme = match authority.port_u16() {
                Some(443) => Scheme::HTTPS,
                _ => Scheme::HTTP,
            };
            set_connect_scheme(uri, scheme.clone());
            Ok(Origin {
                scheme,
                authority: authority.clone(),
            })
        }
        _ => Err(Error::AbsoluteUriRequired),
    }
}

fn validate_port(authority: &Authority) -> Result<(), Error> {
    // Authority::port() treats malformed and empty explicit ports as absent,
    // so inspect the suffix before a connector can substitute a default.
    let host_port = authority
        .as_str()
        .rsplit('@')
        .next()
        .expect("split always returns the authority's host and port");
    let suffix = &host_port[authority.host().len()..];
    let Some(port) = suffix.strip_prefix(':') else {
        return if suffix.is_empty() {
            Ok(())
        } else {
            Err(Error::InvalidPort)
        };
    };
    if !port.is_empty()
        && port.bytes().all(|byte| byte.is_ascii_digit())
        && port.parse::<u16>().is_ok()
    {
        Ok(())
    } else {
        Err(Error::InvalidPort)
    }
}

/// Applies direct-origin HTTP/1 target and `Host` rules.
pub fn normalize_http1_request<B>(request: &mut Request<B>, set_host: bool) {
    if set_host {
        synthesize_host(request);
    }
    if request.method() == Method::CONNECT {
        authority_form(request.uri_mut());
    } else {
        origin_form(request.uri_mut());
    }
}

/// Inserts `Host` only when callers did not explicitly supply one. Default
/// ports are deliberately omitted, matching normal HTTP origin authority.
pub fn synthesize_host<B>(request: &mut Request<B>) {
    if request.headers().contains_key(HOST) {
        return;
    }

    let uri = request.uri();
    let host = uri
        .host()
        .expect("origin validation guarantees an authority host");
    let value = match non_default_port(uri) {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    let value = HeaderValue::from_str(&value).expect("a URI host is a valid header value");
    request.headers_mut().insert(HOST, value);
}

/// Converts an absolute URI to origin form, preserving an empty/default path
/// as `/` and retaining a query without copying it.
pub fn origin_form(uri: &mut Uri) {
    let normalized = match uri.path_and_query() {
        Some(path) if path.as_str() != "/" => {
            let mut parts = http::uri::Parts::default();
            parts.path_and_query = Some(path.clone());
            Uri::from_parts(parts).expect("a URI path and query remain valid")
        }
        _ => Uri::default(),
    };
    *uri = normalized;
}

/// Converts a CONNECT URI to HTTP/1 authority form.
pub fn authority_form(uri: &mut Uri) {
    let authority = uri
        .authority()
        .cloned()
        .expect("CONNECT normalization follows origin validation");
    let mut parts = http::uri::Parts::default();
    parts.authority = Some(authority);
    *uri = Uri::from_parts(parts).expect("an existing URI authority is valid");
}

fn set_connect_scheme(uri: &mut Uri, scheme: Scheme) {
    debug_assert!(uri.scheme().is_none());
    let previous = std::mem::take(uri);
    let mut parts: http::uri::Parts = previous.into();
    parts.scheme = Some(scheme);
    parts.path_and_query = Some("/".parse().expect("a slash is a valid path"));
    *uri = Uri::from_parts(parts).expect("adding a scheme to CONNECT authority is valid");
}

fn non_default_port(uri: &Uri) -> Option<http::uri::Port<&str>> {
    match (uri.port().map(|port| port.as_u16()), is_secure_scheme(uri)) {
        (Some(443), true) | (Some(80), false) => None,
        _ => uri.port(),
    }
}

fn is_secure_scheme(uri: &Uri) -> bool {
    matches!(uri.scheme_str(), Some("https") | Some("wss"))
}

#[cfg(test)]
mod tests {
    use http::header::HOST;
    use http::{Method, Request, Uri};

    use super::{extract_origin, normalize_http1_request, Error};

    fn normalized(uri: &str) -> Request<()> {
        let mut request = Request::builder().uri(uri).body(()).unwrap();
        extract_origin(request.uri_mut(), false).unwrap();
        normalize_http1_request(&mut request, true);
        request
    }

    #[test]
    fn root_uses_origin_form_and_synthesizes_host() {
        let request = normalized("http://example.test");
        assert_eq!(request.uri(), &Uri::from_static("/"));
        assert_eq!(request.headers()[HOST], "example.test");
    }

    #[test]
    fn paths_and_queries_stay_in_origin_form() {
        let request = normalized("https://example.test/foo?x=1");
        assert_eq!(request.uri(), &Uri::from_static("/foo?x=1"));
        assert_eq!(request.headers()[HOST], "example.test");
    }

    #[test]
    fn explicit_host_is_preserved_and_default_ports_are_hidden() {
        let mut request = Request::builder()
            .uri("https://example.test:443/a")
            .header(HOST, "chosen.test")
            .body(())
            .unwrap();
        extract_origin(request.uri_mut(), false).unwrap();
        normalize_http1_request(&mut request, true);
        assert_eq!(request.headers()[HOST], "chosen.test");
        assert_eq!(request.uri(), &Uri::from_static("/a"));

        let request = normalized("http://example.test:8080/a");
        assert_eq!(request.headers()[HOST], "example.test:8080");
    }

    #[test]
    fn ipv4_and_ipv6_hosts_are_accepted() {
        let ipv4 = normalized("http://127.0.0.1:8080/a");
        assert_eq!(ipv4.headers()[HOST], "127.0.0.1:8080");

        let ipv6 = normalized("http://[::1]:8080/a");
        assert_eq!(ipv6.headers()[HOST], "[::1]:8080");
    }

    #[test]
    fn explicit_ports_must_be_nonempty_decimal_u16_values() {
        for input in [
            "http://example.test:8080x/a",
            "http://example.test:65536/a",
            "http://example.test:/a",
        ] {
            let mut uri = input.parse::<Uri>().unwrap();
            assert_eq!(
                extract_origin(&mut uri, false),
                Err(Error::InvalidPort),
                "{input}"
            );
        }
    }

    #[test]
    fn absent_ports_and_bracketed_ipv6_origins_remain_valid() {
        let mut no_port = "https://example.test/path".parse::<Uri>().unwrap();
        assert!(extract_origin(&mut no_port, false).is_ok());

        let mut ipv6 = "https://[::1]:8443/path".parse::<Uri>().unwrap();
        let origin = extract_origin(&mut ipv6, false).unwrap();
        assert_eq!(origin.authority().as_str(), "[::1]:8443");
    }

    #[test]
    fn connect_uses_authority_form_and_accepts_authority_uri() {
        let mut request = Request::builder()
            .method(Method::CONNECT)
            .uri("example.test:443")
            .body(())
            .unwrap();
        let origin = extract_origin(request.uri_mut(), true).unwrap();
        assert_eq!(origin.scheme(), &http::uri::Scheme::HTTPS);
        normalize_http1_request(&mut request, true);
        assert_eq!(request.uri(), &Uri::from_static("example.test:443"));
    }

    #[test]
    fn relative_and_malformed_direct_uris_are_rejected() {
        let mut relative = Uri::from_static("/only-a-path");
        assert_eq!(
            extract_origin(&mut relative, false),
            Err(Error::AbsoluteUriRequired)
        );
        assert!("http:/broken".parse::<Uri>().is_err());
    }
}
