//! Static and parameterized router endpoints for dispatch benchmarks.
//!
//! The transport, executor, and response payloads match `interop-server`; the
//! only difference is that routing runs through `h12tiny::web::Router` instead
//! of the direct benchmark service.

use std::env;

use async_net::TcpListener;
use bytes::Bytes;
use h12tiny::io::FuturesIo;
use h12tiny::runtime::{BoxExecutor, BoxSendFuture};
use h12tiny::server::conn::auto;
use h12tiny::web::{get, Router};

static BODY_1K: [u8; 1024] = [b'x'; 1024];
static BODY_64K: [u8; 64 * 1024] = [b'x'; 64 * 1024];

#[derive(Clone, Copy, Debug)]
struct SmolExecutor;

impl hyper::rt::Executor<BoxSendFuture> for SmolExecutor {
    fn execute(&self, future: BoxSendFuture) {
        smol::spawn(future).detach();
    }
}

fn router() -> Router {
    Router::new()
        .route("/0", get(|| async { Bytes::new() }))
        .route("/1k", get(|| async { Bytes::from_static(&BODY_1K) }))
        .route("/64k", get(|| async { Bytes::from_static(&BODY_64K) }))
        .route(
            "/param/:value",
            get(|h12tiny::web::Path(value): h12tiny::web::Path<String>| async move { value }),
        )
        .route("/static", get(|| async { "static route" }))
}

fn usage() -> &'static str {
    "usage: cargo run --example router-server -- [--addr ADDR]\n\n\
default: --addr 127.0.0.1:3002\n\
endpoints: /0, /1k, /64k, /param/:value, /static"
}

fn address() -> Result<String, String> {
    let mut address = "127.0.0.1:3002".to_owned();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            "--addr" => address = args.next().ok_or("--addr needs ADDR".to_owned())?,
            other => return Err(format!("unknown option {other:?}\n\n{}", usage())),
        }
    }
    Ok(address)
}

async fn serve(listener: TcpListener, executor: BoxExecutor, router: Router) {
    loop {
        let (stream, peer) = listener.accept().await.expect("router listener failed");
        let executor = executor.clone();
        let router = router.clone();
        smol::spawn(async move {
            if let Err(error) = auto::Builder::new(executor)
                .serve_connection(FuturesIo::new(stream), router)
                .await
            {
                eprintln!("router connection {peer} ended with error: {error}");
            }
        })
        .detach();
    }
}

fn main() {
    let address = match address() {
        Ok(address) => address,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    };

    smol::block_on(async move {
        let listener = TcpListener::bind(&address).await?;
        eprintln!(
            "h12tiny router server listening on http://{}",
            listener.local_addr()?
        );
        serve(listener, BoxExecutor::new(SmolExecutor), router()).await;
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .unwrap_or_else(|error| {
        eprintln!("router server failed: {error}");
        std::process::exit(1);
    });
}
