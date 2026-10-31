use futures::channel::oneshot;
use futures::StreamExt;
use h2_support::prelude::*;
use laputa_protocol_test_support::block_on_with_limit;
use std::task::Poll;
use std::time::Duration;

const WRITE_CAPACITY: usize = 1024;
const REQUEST_DATA: usize = 64 * 1024;
const RESPONSE_DATA: usize = 8 * 1024;

fn queue_request_body(send: &mut SendStream<Bytes>) {
    let chunk = Bytes::from(vec![b'q'; 16 * 1024]);
    let mut remaining = REQUEST_DATA;
    while remaining > chunk.len() {
        send.send_data(chunk.clone(), false).unwrap();
        remaining -= chunk.len();
    }
    send.send_data(chunk.slice(..remaining), true).unwrap();
}

async fn run_case(user_ping: bool) {
    let (io, mut peer) = mock::new_with_write_capacity(WRITE_CAPACITY);
    let (handshake, ()) = futures::future::join(client::handshake(io), async {
        peer.read_preface().await.unwrap();
        peer.send_bytes(frames::SETTINGS).await;
        peer.send_bytes(frames::SETTINGS_ACK).await;
    })
    .await;
    let (mut sender, mut conn) = handshake.unwrap();
    peer.set_write_capacity(WRITE_CAPACITY);

    let mut user_pings = client::Connection::ping_pong(&mut conn);
    let request = Request::builder()
        .method(Method::POST)
        .uri("https://example.com/")
        .body(())
        .unwrap();
    let (response, mut send) = sender.send_request(request, false).unwrap();
    queue_request_body(&mut send);
    drop(send);
    drop(sender);

    let mut driver = Box::pin(async move {
        let result = conn.await;
        result.unwrap()
    });
    futures::future::poll_fn(|cx| {
        if peer.write_capacity() == 0 {
            return Poll::Ready(());
        }
        assert!(driver.as_mut().poll(cx).is_pending());
        if peer.write_capacity() == 0 {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;

    if user_ping {
        user_pings
            .as_mut()
            .expect("user pings")
            .send_ping(Ping::opaque())
            .unwrap();
    }

    peer.send_frame(frames::headers(1).response(200)).await;
    peer.send_frame(frames::ping([7; 8])).await;
    peer.send_frame(frames::data(1, Bytes::from(vec![b'r'; RESPONSE_DATA])).eos())
        .await;

    let (data_seen_tx, data_seen_rx) = oneshot::channel();
    let body = async move {
        let response = response.await.unwrap();
        let mut body = response.into_body();
        let chunk = body.data().await.expect("a DATA frame").unwrap();
        assert_eq!(chunk.len(), RESPONSE_DATA);
        data_seen_tx.send(()).unwrap();
        assert!(body.data().await.is_none());
        eprintln!("body done user_ping={user_ping}");
    };
    let peer_task = async move {
        data_seen_rx.await.expect("body data delivered");
        peer.unbounded_bytes().await;
        let mut saw_pong = false;
        let mut saw_ping = false;
        while let Some(frame) = peer.next().await {
            if let h2::frame::Frame::Ping(ping) = frame.unwrap() {
                if ping.is_ack() {
                    saw_pong = true;
                } else {
                    saw_ping = true;
                }
                if saw_pong && (saw_ping || !user_ping) {
                    break;
                }
            }
        }
        assert!(saw_pong, "peer never received PONG");
        if user_ping {
            assert!(saw_ping, "peer never received the user PING");
        }
    };

    join3(driver, body, peer_task).await;
}

#[test]
fn pending_control_writes_do_not_stop_reading() {
    block_on_with_limit(
        async move {
            futures::future::join(run_case(false), run_case(true)).await;
        },
        Duration::from_secs(2),
    )
    .expect("both connections complete while a PONG or PING write is pending");
}
