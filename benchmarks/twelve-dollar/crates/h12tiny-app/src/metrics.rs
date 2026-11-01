use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use h12tiny::web::StatusCode;

const TRACKED_STATUSES: [u16; 6] = [200, 201, 400, 401, 404, 500];

pub struct Metrics {
    accepted: AtomicU64,
    rejected: AtomicU64,
    statuses: [AtomicU64; TRACKED_STATUSES.len()],
    other: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            statuses: std::array::from_fn(|_| AtomicU64::new(0)),
            other: AtomicU64::new(0),
        }
    }

    pub fn accepted(&self) {
        self.accepted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn response(&self, status: StatusCode) {
        let code = status.as_u16();
        match TRACKED_STATUSES.iter().position(|&tracked| tracked == code) {
            Some(index) => {
                self.statuses[index].fetch_add(1, Ordering::Relaxed);
            }
            None => {
                self.other.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn summary(&self) -> String {
        let mut responses = 0;
        let mut statuses = String::new();
        for (code, count) in TRACKED_STATUSES.iter().zip(&self.statuses) {
            let count = count.load(Ordering::Relaxed);
            responses += count;
            let _ = write!(statuses, " status_{code}={count}");
        }
        let other = self.other.load(Ordering::Relaxed);
        let mut line = format!(
            "accepted={} rejected={} responses={}",
            self.accepted.load(Ordering::Relaxed),
            self.rejected.load(Ordering::Relaxed),
            responses + other
        );
        line.push_str(&statuses);
        let _ = write!(line, " status_other={other}");
        line
    }
}
