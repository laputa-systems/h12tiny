use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

pub const STALL_LIMIT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Stats {
    pub polls: usize,
    pub wakes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stall {
    pub stats: Stats,
    pub limit: Duration,
}

impl fmt::Display for Stall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "future made no progress for {:?} after {} poll(s) and {} wake(s)",
            self.limit, self.stats.polls, self.stats.wakes
        )
    }
}

impl std::error::Error for Stall {}

struct WakeState {
    tx: mpsc::Sender<()>,
    wakes: AtomicUsize,
}

impl Wake for WakeState {
    fn wake(self: Arc<Self>) {
        self.wakes.fetch_add(1, Ordering::SeqCst);
        let _ = self.tx.send(());
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wakes.fetch_add(1, Ordering::SeqCst);
        let _ = self.tx.send(());
    }
}

pub fn block_on<F: Future>(future: F) -> Result<F::Output, Stall> {
    block_on_with_limit(future, STALL_LIMIT)
}

pub fn block_on_with_limit<F: Future>(
    future: F,
    limit: Duration,
) -> Result<F::Output, Stall> {
    block_on_with_stats(future, limit).map(|(output, _)| output)
}

pub fn block_on_with_stats<F: Future>(
    future: F,
    limit: Duration,
) -> Result<(F::Output, Stats), Stall> {
    let (tx, rx) = mpsc::channel();
    let wake = Arc::new(WakeState {
        tx,
        wakes: AtomicUsize::new(0),
    });
    let waker = Waker::from(wake.clone());
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    let mut polls = 0;

    loop {
        polls += 1;
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return Ok((
                output,
                Stats {
                    polls,
                    wakes: wake.wakes.load(Ordering::SeqCst),
                },
            ));
        }

        match rx.recv_timeout(limit) {
            Ok(()) => {}
            Err(RecvTimeoutError::Timeout) => {
                return Err(Stall {
                    stats: Stats {
                        polls,
                        wakes: wake.wakes.load(Ordering::SeqCst),
                    },
                    limit,
                });
            }
            Err(RecvTimeoutError::Disconnected) => {
                unreachable!("the executor owns the wake sender")
            }
        }
    }
}

pub fn poll_once<F: Future + Unpin>(future: &mut F, waker: &Waker) -> Poll<F::Output> {
    let mut cx = Context::from_waker(waker);
    Pin::new(future).poll(&mut cx)
}
