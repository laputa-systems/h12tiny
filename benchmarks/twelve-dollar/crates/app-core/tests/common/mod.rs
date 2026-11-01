#![allow(dead_code)]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use rusqlite::Connection;
use sha2::Sha256;

pub struct TestDb {
    dir: PathBuf,
    pub path: PathBuf,
}

impl TestDb {
    pub fn new(name: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("app-core-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test directory");
        let path = dir.join("feed.db");
        let schema_path = challenge_dir().join("schema.sql");
        let schema = std::fs::read_to_string(&schema_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", schema_path.display()));
        let connection = Connection::open(&path).expect("open test database");
        connection.execute_batch(&schema).expect("apply schema");
        Self { dir, path }
    }

    pub fn connection(&self) -> Connection {
        Connection::open(&self.path).expect("open test connection")
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn challenge_dir() -> PathBuf {
    std::env::var_os("TWELVE_DOLLAR_CHALLENGE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../.work/twelve-dollar/challenge")
        })
}

pub fn sign_token(secret: &[u8], header: &str, payload: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(header);
    let payload = URL_SAFE_NO_PAD.encode(payload);
    let signing_input = format!("{header}.{payload}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("hmac key");
    mac.update(signing_input.as_bytes());
    let signature = mac.finalize().into_bytes();
    format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature))
}

pub fn token(secret: &[u8], payload: &str) -> String {
    sign_token(secret, r#"{"alg":"HS256","typ":"JWT"}"#, payload)
}

pub fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::park(),
        }
    }
}

struct ThreadWaker(thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}
