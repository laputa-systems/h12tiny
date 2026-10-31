use futures::StreamExt;
use h2_support::prelude::*;

use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::thread;

struct Server {
    addr: SocketAddr,
    reqs: Arc<AtomicUsize>,
    _join: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn serve<F>(mk_data: F) -> Self
    where
        F: Fn() -> Bytes,
        F: Send + Sync + 'static,
    {
        let mk_data = Arc::new(mk_data);
        let listener =
            smol::block_on(smol::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))))
                .unwrap();
        let addr = listener.local_addr().unwrap();
        let reqs = Arc::new(AtomicUsize::new(0));
        let reqs2 = reqs.clone();
        let join = thread::spawn(move || {
            smol::block_on(async move {
                loop {
                    let socket = listener.accept().await.map(|(s, _)| s);
                    let reqs = reqs2.clone();
                    let mk_data = mk_data.clone();
                    smol::spawn(async move {
                        if let Err(e) = handle_request(socket, reqs, mk_data).await {
                            eprintln!("serve conn error: {:?}", e)
                        }
                    })
                    .detach();
                }
            });
        });

        Self {
            addr,
            reqs,
            _join: Some(join),
        }
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }

    fn request_count(&self) -> usize {
        self.reqs.load(Ordering::Acquire)
    }
}

async fn handle_request<F>(
    socket: std::io::Result<smol::net::TcpStream>,
    reqs: Arc<AtomicUsize>,
    mk_data: Arc<F>,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: Fn() -> Bytes,
    F: Send + Sync + 'static,
{
    let socket = socket?;
    socket.set_nodelay(true)?;
    let mut conn = server::handshake(socket).await?;
    while let Some(result) = conn.next().await {
        let (_, mut respond) = result?;
        reqs.fetch_add(1, Ordering::Release);
        let response = Response::builder().status(StatusCode::OK).body(()).unwrap();
        let mut send = respond.send_response(response, false)?;
        send.send_data(mk_data(), true)?;
    }
    Ok(())
}

#[test]
fn hammer_client_concurrency() {
    // This reproduces issue #326.
    const N: usize = 5000;

    let server = Server::serve(|| Bytes::from_static(b"hello world!"));

    let addr = server.addr();
    let rsps = Arc::new(AtomicUsize::new(0));

    for _ in 0..N {
        let rsps = rsps.clone();
        smol::block_on(async move {
            let tcp = smol::net::TcpStream::connect(addr).await.unwrap();
            tcp.set_nodelay(true).unwrap();
            let (mut client, h2) = client::handshake(tcp).await.unwrap();
            let request = Request::builder()
                .uri("https://http2.akamai.com/")
                .body(())
                .unwrap();

            let (response, mut stream) = client.send_request(request, false).unwrap();
            stream.send_trailers(HeaderMap::new()).unwrap();
            drop(client);

            smol::spawn(async move {
                let _ = h2.await;
            })
            .detach();

            let response = response.await.unwrap();
            let mut body = response.into_body();
            while let Some(res) = body.data().await {
                res.unwrap();
            }
            body.trailers().await.unwrap();
            rsps.fetch_add(1, Ordering::Release);
        });
    }

    assert_eq!(N, rsps.load(Ordering::Acquire));
    assert_eq!(N, server.request_count());
}
