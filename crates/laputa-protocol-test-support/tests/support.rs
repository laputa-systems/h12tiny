use std::future::Future;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures_lite::io::{AsyncReadExt, AsyncWriteExt};
use laputa_protocol_test_support::executor::{block_on_with_limit, block_on_with_stats};
use laputa_protocol_test_support::mock_io::Builder;
use laputa_protocol_test_support::pipe::BudgetPipe;
use laputa_protocol_test_support::timer;

fn pending_once(yielded: &mut bool) -> impl Future<Output = ()> + '_ {
    std::future::poll_fn(move |cx| {
        if *yielded {
            Poll::Ready(())
        } else {
            *yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
}

#[test]
fn wake_loss_is_detected() {
    let mut polls = 0;
    let result = block_on_with_limit(
        std::future::poll_fn(|_| {
            polls += 1;
            Poll::Pending::<()>
        }),
        Duration::from_millis(100),
    );

    let stall = result.expect_err("a future that never wakes must stall");
    assert_eq!(polls, 1, "the executor must not poll again without a wake");
    assert_eq!(stall.stats.polls, 1);
    assert_eq!(stall.stats.wakes, 0);
}

#[test]
fn self_wake_causes_exactly_one_repoll() {
    let mut yielded = false;
    let (output, stats) = block_on_with_stats(
        async move {
            pending_once(&mut yielded).await;
            42
        },
        Duration::from_secs(2),
    )
    .expect("self-waking future completes");

    assert_eq!(output, 42);
    assert_eq!(stats.polls, 2);
    assert_eq!(stats.wakes, 1);
}

#[test]
fn timer_wakes_the_executor_from_another_thread() {
    let start = Instant::now();
    block_on_with_limit(timer::sleep(Duration::from_millis(20)), Duration::from_secs(2))
        .expect("timer wake reaches the executor");
    assert!(start.elapsed() >= Duration::from_millis(20));
}

#[test]
fn budget_pipe_blocks_writes_until_the_peer_reads() {
    let (mut writer, mut reader) = BudgetPipe::pair(4);

    let mut write = Box::pin(async move {
        writer.write_all(b"abcdef").await.unwrap();
        writer
    });
    let waker = std::task::Waker::noop();
    let mut cx = Context::from_waker(waker);

    assert!(matches!(write.as_mut().poll(&mut cx), Poll::Pending));

    let mut buf = [0; 2];
    block_on_with_limit(async { reader.read_exact(&mut buf).await.unwrap() }, Duration::from_secs(2))
        .expect("peer read");
    assert_eq!(&buf, b"ab");

    assert!(matches!(write.as_mut().poll(&mut cx), Poll::Ready(_)));
    let mut rest = [0; 4];
    let count = block_on_with_limit(async { reader.read(&mut rest).await.unwrap() }, Duration::from_secs(2))
        .expect("peer drain");
    assert_eq!(&rest[..count], b"cdef");
}

#[test]
fn scripted_mock_serves_reads_and_writes_in_order() {
    let mut builder = Builder::new();
    builder.name("scripted").read(&[1, 2]).write(&[3, 4]);
    let mut io = builder.build();

    block_on_with_limit(
        async {
            let mut read = [0; 2];
            io.read_exact(&mut read).await.unwrap();
            assert_eq!(read, [1, 2]);
            io.write_all(&[3, 4]).await.unwrap();
        },
        Duration::from_secs(2),
    )
    .expect("scripted exchange");
}

#[test]
fn stall_reports_the_timeout_it_used() {
    let limit = Duration::from_millis(30);
    let stall = block_on_with_limit(std::future::pending::<()>(), limit).unwrap_err();
    assert_eq!(stall.limit, limit);
    assert!(stall.to_string().contains("no progress"));
}
