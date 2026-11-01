//! Small internal load example for h12tiny's direct-origin client.
//!
//! This intentionally uses a batch of futures rather than a benchmark
//! framework. It reports elapsed time, throughput, protocol versions, errors,
//! and deterministic body mismatches. Opt-in client debug events provide the
//! connection/session counts without adding a metrics or logging dependency.

use std::convert::Infallible;
use std::env;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Instant;

use bytes::Bytes;
use futures_util::future::join_all;
use h12tiny::client::{Client, ConnectionProtocol, DebugEvent, DebugEventLog};
use h12tiny::runtime::BoxSendFuture;
use http::Uri;
use http_body::{Body, Frame};
use hyper::body::Incoming;

#[derive(Clone, Copy, Debug)]
struct SmolExecutor;

impl hyper::rt::Executor<BoxSendFuture> for SmolExecutor {
    fn execute(&self, future: BoxSendFuture) {
        smol::spawn(future).detach();
    }
}

#[derive(Debug, Default)]
struct EmptyBody;

impl Body for EmptyBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(None)
    }

    fn is_end_stream(&self) -> bool {
        true
    }
}

#[derive(Debug)]
struct Options {
    uri: Uri,
    requests: usize,
    concurrency: usize,
    http2: bool,
    debug_events: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct EventCounts {
    tcp_connections: usize,
    tls_handshakes: usize,
    h1_connections: usize,
    h2_sessions: usize,
}

fn count_events(events: impl IntoIterator<Item = DebugEvent>) -> EventCounts {
    let mut counts = EventCounts::default();
    for event in events {
        match event {
            // The client records this only after its direct connector has
            // established the TCP (and, for HTTPS, TLS) transport.
            DebugEvent::ConnectionEstablished { protocol, .. } => {
                counts.tcp_connections += 1;
                match protocol {
                    ConnectionProtocol::Http1 => counts.h1_connections += 1,
                    ConnectionProtocol::Http2 => counts.h2_sessions += 1,
                    _ => {}
                }
            }
            // The public event is emitted once the connector has completed a
            // TLS handshake and observed ALPN. It is absent for cleartext.
            DebugEvent::AlpnSelected { .. } => counts.tls_handshakes += 1,
            _ => {}
        }
    }
    counts
}

#[derive(Clone, Debug, Default)]
struct Concurrency {
    in_flight: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl Concurrency {
    fn enter(&self) -> InFlight {
        let current = self.in_flight.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak.fetch_max(current, Ordering::Relaxed);
        InFlight {
            in_flight: Arc::clone(&self.in_flight),
        }
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
struct InFlight {
    in_flight: Arc<AtomicUsize>,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

fn usage() -> &'static str {
    "usage: cargo run --release --example client-load -- URI [--requests N] [--concurrency N] [--http2] [--debug-events]\n\n\
URI should end in /0, /1k, /64k, /1m, or /headers when body-size
validation is desired.\n\
HTTPS uses the system web PKI; the committed localhost fixture is intended\n\
for the server-side curl/nghttp checks, not for this default client policy.\n\n\
--debug-events retains endpoint lifecycle observations and is intended for\n\
diagnostic runs, not throughput or memory measurements."
}

fn positive(value: &str, flag: &str) -> Result<usize, String> {
    let value = value
        .parse::<usize>()
        .map_err(|_| format!("{flag} expects a positive integer, got {value:?}"))?;
    if value == 0 {
        return Err(format!("{flag} expects a positive integer"));
    }
    Ok(value)
}

fn parse_options(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Err(usage().to_owned());
    };
    if first == "--help" || first == "-h" {
        println!("{}", usage());
        std::process::exit(0);
    }
    let uri = first
        .parse::<Uri>()
        .map_err(|error| format!("invalid URI: {error}"))?;
    let mut requests = 1_000;
    let mut concurrency = 16;
    let mut http2 = false;
    let mut debug_events = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--requests" => {
                let value = args.next().ok_or("--requests needs N".to_owned())?;
                requests = positive(&value, "--requests")?;
            }
            "--concurrency" => {
                let value = args.next().ok_or("--concurrency needs N".to_owned())?;
                concurrency = positive(&value, "--concurrency")?;
            }
            "--http2" => http2 = true,
            "--debug-events" => debug_events = true,
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("unknown option {other:?}\n\n{}", usage())),
        }
    }
    Ok(Options {
        uri,
        requests,
        concurrency,
        http2,
        debug_events,
    })
}

fn options() -> Result<Options, String> {
    parse_options(env::args().skip(1))
}

fn expected_body_len(uri: &Uri) -> Option<usize> {
    match uri.path() {
        "/0" => Some(0),
        "/1k" => Some(1024),
        "/64k" => Some(64 * 1024),
        "/1m" => Some(1024 * 1024),
        "/headers" => Some(0),
        _ => None,
    }
}

async fn body_len(mut body: Incoming) -> Result<usize, String> {
    let mut length = 0;
    while let Some(frame) =
        futures_util::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
    {
        let frame = frame.map_err(|error| error.to_string())?;
        if let Ok(data) = frame.into_data() {
            length += data.len();
        }
    }
    Ok(length)
}

async fn one_request(
    client: Client<EmptyBody>,
    uri: Uri,
    expected: Option<usize>,
    concurrency: Concurrency,
) -> Result<(http::Version, bool), String> {
    let _in_flight = concurrency.enter();
    let response = client
        .get(uri)
        .await
        .map_err(|error| format!("{} ({:?})", error, error.kind()))?;
    let version = response.version();
    let length = body_len(response.into_body()).await?;
    let body_ok = expected.is_none_or(|expected| expected == length);
    Ok((version, body_ok))
}

fn main() {
    let options = match options() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    };

    let result = smol::block_on(async move {
        let mut builder = Client::builder(SmolExecutor);
        if options.http2 {
            builder.http2_only(true);
        }
        let debug_events = options.debug_events.then(DebugEventLog::default);
        if let Some(debug_events) = &debug_events {
            builder.debug_event_log(debug_events.clone());
        }
        let client = builder.build::<EmptyBody>();
        let expected = expected_body_len(&options.uri);
        let concurrency = Concurrency::default();
        let started = Instant::now();
        let mut completed = 0;
        let mut errors = 0;
        let mut body_mismatches = 0;
        let mut http11 = 0;
        let mut http2 = 0;

        while completed < options.requests {
            let batch = (options.requests - completed).min(options.concurrency);
            let requests = (0..batch)
                .map(|_| {
                    one_request(
                        client.clone(),
                        options.uri.clone(),
                        expected,
                        concurrency.clone(),
                    )
                })
                .collect::<Vec<_>>();
            for result in join_all(requests).await {
                match result {
                    Ok((version, body_ok)) => {
                        match version {
                            http::Version::HTTP_11 => http11 += 1,
                            http::Version::HTTP_2 => http2 += 1,
                            _ => {}
                        }
                        if !body_ok {
                            body_mismatches += 1;
                        }
                    }
                    Err(error) => {
                        errors += 1;
                        if errors == 1 {
                            eprintln!("first request error: {error}");
                        }
                    }
                }
            }
            completed += batch;
        }
        let elapsed = started.elapsed();
        let seconds = elapsed.as_secs_f64();
        println!("uri={}", options.uri);
        println!("protocol={}", if options.http2 { "h2" } else { "auto" });
        println!("requests={}", options.requests);
        println!("concurrency={}", options.concurrency);
        println!("logical_concurrency={}", options.concurrency);
        println!("peak_concurrency={}", concurrency.peak());
        println!("elapsed_seconds={seconds:.6}");
        println!(
            "requests_per_second={:.3}",
            options.requests as f64 / seconds
        );
        println!("ok={}", options.requests - errors - body_mismatches);
        println!("errors={errors}");
        println!("body_mismatches={body_mismatches}");
        println!("http1_responses={http11}");
        println!("http2_responses={http2}");
        if let Some(debug_events) = debug_events {
            let event_counts = count_events(debug_events.drain());
            println!("debug_events=enabled");
            println!("tcp_connections={}", event_counts.tcp_connections);
            println!("tls_handshakes={}", event_counts.tls_handshakes);
            println!("h1_connections={}", event_counts.h1_connections);
            println!("h2_sessions={}", event_counts.h2_sessions);
        } else {
            println!("debug_events=disabled");
        }
        if errors != 0 || body_mismatches != 0 {
            return Err("load run did not complete cleanly".to_owned());
        }
        Ok::<(), String>(())
    });
    if let Err(error) = result {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_counts_report_transport_and_protocol_sessions() {
        let events = [
            DebugEvent::PoolCheckout {
                origin: "http://example.test".to_owned(),
            },
            DebugEvent::ConnectionEstablished {
                origin: "http://example.test".to_owned(),
                protocol: ConnectionProtocol::Http1,
            },
            DebugEvent::ConnectionEstablished {
                origin: "https://example.test".to_owned(),
                protocol: ConnectionProtocol::Http2,
            },
            DebugEvent::AlpnSelected {
                origin: "https://example.test".to_owned(),
                protocol: ConnectionProtocol::Http2,
            },
            DebugEvent::ConnectionPooled {
                origin: "https://example.test".to_owned(),
            },
        ];

        assert_eq!(
            count_events(events),
            EventCounts {
                tcp_connections: 2,
                tls_handshakes: 1,
                h1_connections: 1,
                h2_sessions: 1,
            }
        );
    }

    #[test]
    fn concurrency_tracks_peak_until_each_request_finishes() {
        let concurrency = Concurrency::default();
        let first = concurrency.enter();
        assert_eq!(concurrency.peak(), 1);
        {
            let second = concurrency.enter();
            assert_eq!(concurrency.peak(), 2);
            drop(second);
        }
        assert_eq!(concurrency.peak(), 2);
        drop(first);
        assert_eq!(concurrency.in_flight.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn debug_event_collection_is_opt_in() {
        let default = parse_options(vec!["http://example.test/64k".to_owned()])
            .expect("default benchmark options must parse");
        assert!(!default.debug_events);

        let diagnostic = parse_options(vec![
            "http://example.test/64k".to_owned(),
            "--debug-events".to_owned(),
        ])
        .expect("diagnostic benchmark options must parse");
        assert!(diagnostic.debug_events);
    }
}
