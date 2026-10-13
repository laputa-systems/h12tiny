#![cfg(all(
    feature = "client",
    feature = "server",
    feature = "web",
    feature = "http1",
    feature = "http2"
))]

mod support;

use async_net::TcpListener;
use h12tiny::client::Client;
use h12tiny::io::FuturesIo;
use h12tiny::runtime::BoxExecutor;
use h12tiny::server::conn::auto;
use h12tiny::util::{self, BoxBody, ResponseBodyExt};
use h12tiny::web::{Router, get};
use http::{Request, StatusCode};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use support::SmolExecutor;

struct CountingAllocator;

// Keep unrelated test and worker-thread allocations outside this measurement.
thread_local! {
    static ROUTER_CLONE_ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

fn record_router_clone_allocation() {
    let _ = ROUTER_CLONE_ALLOCATIONS.try_with(|allocations| {
        if let Some(count) = allocations.get() {
            allocations.set(Some(count + 1));
        }
    });
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_router_clone_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_router_clone_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_router_clone_allocation();
        unsafe { System.realloc(pointer, layout, size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

async fn serve_once(h2: bool) -> (std::net::SocketAddr, smol::Task<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = smol::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let builder = auto::Builder::new(BoxExecutor::new(SmolExecutor));
        let builder = if h2 {
            builder.http2_only()
        } else {
            builder.http1_only()
        };
        let router = Router::<()>::new().route("/web", get(|| async { "router" }));
        builder
            .serve_connection(FuturesIo::new(stream), router)
            .await
            .unwrap();
    });
    (address, server)
}

async fn request_web(h2: bool) {
    let (address, server) = serve_once(h2).await;
    let mut builder = Client::builder(SmolExecutor);
    if h2 {
        builder.http2_only(true);
    }
    let client = builder.build::<BoxBody>();
    let response = client
        .request(
            Request::get(format!("http://{address}/web"))
                .body(util::boxed_body(util::empty_body()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text_limited(64).await.unwrap(), "router");
    drop(client);
    server.await;
}

#[test]
fn router_is_protocol_neutral_over_h1_and_h2() {
    smol::block_on(async {
        request_web(false).await;
        request_web(true).await;
    });
}

#[test]
fn router_clone_shares_routes_without_allocating_and_builder_changes_are_isolated() {
    let router = Router::<()>::new().route(
        "/base/{id}",
        get(|_: h12tiny::web::Path<String>| async { "base" }),
    );

    let (cloned, allocation_count) = ROUTER_CLONE_ALLOCATIONS.with(|allocations| {
        allocations.set(Some(0));
        let cloned = router.clone();
        let allocation_count = allocations
            .replace(None)
            .expect("router clone allocation measurement is active");
        (cloned, allocation_count)
    });

    assert_eq!(allocation_count, 0);

    let extended = cloned.route("/added", get(|| async { "added" }));
    let request = |path| {
        Request::get(path)
            .body(util::boxed_body(util::empty_body()))
            .unwrap()
    };
    assert_eq!(
        smol::block_on(router.call(request("/base/1"))).status(),
        StatusCode::OK
    );
    assert_eq!(
        smol::block_on(router.call(request("/added"))).status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        smol::block_on(extended.call(request("/added"))).status(),
        StatusCode::OK
    );
}
