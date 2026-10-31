pub use async_io::Timer;

use std::future::Future;
use std::time::{Duration, Instant};

pub fn sleep(duration: Duration) -> Timer {
    Timer::after(duration)
}

pub fn sleep_until(deadline: Instant) -> Timer {
    Timer::at(deadline)
}

pub fn now() -> Instant {
    Instant::now()
}

pub async fn timeout<F: Future>(duration: Duration, future: F) -> Option<F::Output> {
    futures_lite::future::or(async { Some(future.await) }, async {
        Timer::after(duration).await;
        None
    })
    .await
}
