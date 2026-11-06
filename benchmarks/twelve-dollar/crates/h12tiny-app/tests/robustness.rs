use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;

const JWT_SECRET: &str = "twelve-dollar-challenge";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(40);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const CLOSE_POLL: Duration = Duration::from_millis(50);
const FD_TOLERANCE: usize = 16;

static SCRATCH_ID: AtomicU64 = AtomicU64::new(0);

struct Response {
    status: u16,
    body: Vec<u8>,
}

impl Response {
    fn text(&self) -> &str {
        std::str::from_utf8(&self.body).unwrap_or("<non-utf8 body>")
    }

    fn json(&self) -> deser_value::Value {
        deser_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("invalid JSON response ({error}): {:?}", self.text()))
    }
}

struct Client {
    stream: TcpStream,
    buffer: Vec<u8>,
}

impl Client {
    fn connect(address: SocketAddr) -> io::Result<Self> {
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(CLOSE_POLL))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        Ok(Self {
            stream,
            buffer: Vec::new(),
        })
    }

    fn request(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> io::Result<Response> {
        self.write_request(method, path, headers, body)?;
        self.read_response(Instant::now() + REQUEST_TIMEOUT)
    }

    fn write_request(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> io::Result<()> {
        let mut request = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
        for (name, value) in headers {
            request.push_str(name);
            request.push_str(": ");
            request.push_str(value);
            request.push_str("\r\n");
        }
        if let Some(body) = body {
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");
        self.stream.write_all(request.as_bytes())?;
        if let Some(body) = body {
            self.stream.write_all(body)?;
        }
        self.stream.flush()
    }

    fn read_response(&mut self, deadline: Instant) -> io::Result<Response> {
        let header_end = loop {
            if let Some(position) = self
                .buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
            {
                break position;
            }
            self.fill(deadline)?;
        };

        let head = String::from_utf8_lossy(&self.buffer[..header_end]).into_owned();
        self.buffer.drain(..header_end + 4);

        let mut lines = head.split("\r\n");
        let status_line = lines
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing status line"))?;
        let status = status_line
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing status code"))?
            .parse::<u16>()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

        let mut content_length = 0usize;
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                if name.trim().eq_ignore_ascii_case("content-length") {
                    content_length = value
                        .trim()
                        .parse::<usize>()
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                }
            }
        }

        while self.buffer.len() < content_length {
            self.fill(deadline)?;
        }
        let body = self.buffer.drain(..content_length).collect();
        Ok(Response { status, body })
    }

    fn fill(&mut self, deadline: Instant) -> io::Result<()> {
        let mut chunk = [0u8; 8192];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "deadline elapsed while waiting for the response",
                ));
            }
            self.stream
                .set_read_timeout(Some(remaining.min(CLOSE_POLL)))?;
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "server closed the connection",
                    ));
                }
                Ok(count) => {
                    self.buffer.extend_from_slice(&chunk[..count]);
                    return Ok(());
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }
}

enum CloseObservation {
    Closed { elapsed: Duration, bytes: Vec<u8> },
    Open,
}

fn observe_close(stream: &mut TcpStream, start: Instant, deadline: Instant) -> CloseObservation {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return CloseObservation::Open;
        }
        if stream
            .set_read_timeout(Some((deadline - now).min(CLOSE_POLL)))
            .is_err()
        {
            return CloseObservation::Closed {
                elapsed: now.saturating_duration_since(start),
                bytes,
            };
        }
        match stream.read(&mut chunk) {
            Ok(0) => {
                return CloseObservation::Closed {
                    elapsed: now.saturating_duration_since(start),
                    bytes,
                };
            }
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => {
                return CloseObservation::Closed {
                    elapsed: now.saturating_duration_since(start),
                    bytes,
                };
            }
        }
    }
}

fn read_until_close(stream: &mut TcpStream, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let now = Instant::now();
        if now >= deadline {
            if bytes.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "connection stayed open with no response",
                ));
            }
            return Ok(bytes);
        }
        stream.set_read_timeout(Some((deadline - now).min(CLOSE_POLL)))?;
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(bytes),
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::BrokenPipe
                ) =>
            {
                return Ok(bytes);
            }
            Err(error) => return Err(error),
        }
    }
}

struct Server {
    child: Child,
    address: SocketAddr,
    scratch: PathBuf,
    stdout_log: Arc<Mutex<String>>,
    stderr_log: Arc<Mutex<String>>,
    stdout_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
    exited: bool,
}

impl Server {
    fn start(name: &str) -> Self {
        let scratch = scratch_dir(name);
        let database = scratch.join("feed.db");
        let seed = challenge_dir().join("seed").join("feed.db");
        fs::copy(&seed, &database).unwrap_or_else(|error| {
            panic!("copy {} to {}: {error}", seed.display(), database.display())
        });

        let executable = env!("CARGO_BIN_EXE_h12tiny-twelve");
        let mut child = Command::new(executable)
            .env("SQLITE_PATH", &database)
            .env("JWT_SECRET", JWT_SECRET)
            .env("HOST", "127.0.0.1")
            .env("PORT", "0")
            .env("TWELVE_DOLLAR_READERS", "2")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("spawn {executable}: {error}"));

        let stdout_log = Arc::new(Mutex::new(String::new()));
        let stderr_log = Arc::new(Mutex::new(String::new()));
        let (address_sender, address_receiver) = mpsc::channel();

        let stdout_thread = {
            let stdout_log = Arc::clone(&stdout_log);
            let stdout = child.stdout.take().expect("server stdout");
            thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if let Some(address) = listening_address(&line) {
                        let _ = address_sender.send(address);
                    }
                    let mut log = stdout_log.lock().expect("stdout log");
                    log.push_str(&line);
                    log.push('\n');
                }
            })
        };
        let stderr_thread = {
            let stderr_log = Arc::clone(&stderr_log);
            let mut stderr = child.stderr.take().expect("server stderr");
            thread::spawn(move || {
                let mut text = String::new();
                let _ = stderr.read_to_string(&mut text);
                stderr_log.lock().expect("stderr log").push_str(&text);
            })
        };

        let address = match address_receiver.recv_timeout(STARTUP_TIMEOUT) {
            Ok(address) => address,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                panic!(
                    "server did not report a listening address ({error});\nstdout:\n{}\nstderr:\n{}",
                    stdout_log.lock().expect("stdout log").clone(),
                    stderr_log.lock().expect("stderr log").clone(),
                );
            }
        };

        let server = Server {
            child,
            address,
            scratch,
            stdout_log,
            stderr_log,
            stdout_thread: Some(stdout_thread),
            stderr_thread: Some(stderr_thread),
            exited: false,
        };
        server.wait_until_healthy();
        server
    }

    fn wait_until_healthy(&self) {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            if let Ok(mut client) = Client::connect(self.address) {
                if let Ok(response) = client.request("GET", "/health", &[], None) {
                    if response.status == 200 {
                        return;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "server did not become healthy within {STARTUP_TIMEOUT:?}; {}",
                self.logs()
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn stdout_text(&self) -> String {
        self.stdout_log.lock().expect("stdout log").clone()
    }

    fn stderr_text(&self) -> String {
        self.stderr_log.lock().expect("stderr log").clone()
    }

    fn logs(&self) -> String {
        format!(
            "stdout:\n{}\nstderr:\n{}",
            self.stdout_text(),
            self.stderr_text()
        )
    }

    fn send_signal(&self, signal: &str) {
        let status = Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(self.child.id().to_string())
            .status()
            .expect("run kill");
        assert!(status.success(), "kill -{signal} failed with {status}");
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.exited = true;
                    self.join_output();
                    return status;
                }
                Ok(None) => {}
                Err(error) => panic!("poll server process: {error}; {}", self.logs()),
            }
            assert!(
                Instant::now() < deadline,
                "server did not exit within {timeout:?}; {}",
                self.logs()
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn join_output(&mut self) {
        if let Some(handle) = self.stdout_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.stderr_thread.take() {
            let _ = handle.join();
        }
    }

    fn shutdown_summary(&self) -> HashMap<String, u64> {
        let stdout = self.stdout_text();
        let line = stdout
            .lines()
            .find(|line| line.starts_with("h12tiny-twelve shutdown "))
            .unwrap_or_else(|| panic!("missing shutdown summary; {}", self.logs()));
        line.split_whitespace()
            .skip(2)
            .map(|field| {
                let (name, value) = field
                    .split_once('=')
                    .unwrap_or_else(|| panic!("malformed summary field {field:?} in {line:?}"));
                let value = value
                    .parse()
                    .unwrap_or_else(|error| panic!("parse {field:?}: {error}"));
                (name.to_owned(), value)
            })
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if !self.exited {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        self.join_output();
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

fn listening_address(line: &str) -> Option<SocketAddr> {
    let rest = line.split("listening on http://").nth(1)?;
    rest.split_whitespace().next()?.parse().ok()
}

fn challenge_dir() -> PathBuf {
    match std::env::var_os("TWELVE_DOLLAR_CHALLENGE") {
        Some(path) => PathBuf::from(path),
        None => Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../.work/twelve-dollar/challenge"),
    }
}

fn scratch_dir(name: &str) -> PathBuf {
    let id = SCRATCH_ID.fetch_add(1, Ordering::Relaxed);
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "robustness-{name}-{}-{id}",
        std::process::id()
    ));
    fs::create_dir_all(&path)
        .unwrap_or_else(|error| panic!("create {}: {error}", path.display()));
    path
}

struct SeedToken {
    username: String,
    token: String,
}

fn seed_tokens() -> &'static [SeedToken] {
    static TOKENS: OnceLock<Vec<SeedToken>> = OnceLock::new();
    TOKENS.get_or_init(|| {
        let path = challenge_dir().join("seed").join("tokens.json");
        let text = fs::read(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let value: deser_value::Value =
            deser_json::from_slice(&text).unwrap_or_else(|error| panic!("parse tokens.json: {error}"));
        value
            .as_seq()
            .expect("tokens.json is an array")
            .iter()
            .map(|entry| SeedToken {
                username: entry["username"]
                    .as_str()
                    .expect("token username")
                    .to_owned(),
                token: entry["token"].as_str().expect("token value").to_owned(),
            })
            .collect()
    })
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after the epoch")
        .as_secs()
}

type HmacSha256 = Hmac<Sha256>;

fn sign_parts(header: &str, payload: &str, secret: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(format!("{header}.{payload}").as_bytes());
    let signature = mac.finalize().into_bytes();
    format!("{header}.{payload}.{}", URL_SAFE_NO_PAD.encode(signature))
}

fn sign_payload(payload: &deser_value::Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
    let payload =
        URL_SAFE_NO_PAD.encode(deser_json::to_string(payload).map(String::into_bytes).expect("serialize token payload"));
    sign_parts(&header, &payload, JWT_SECRET.as_bytes())
}

fn sign_payload_with_secret(payload: &deser_value::Value, secret: &[u8]) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
    let payload =
        URL_SAFE_NO_PAD.encode(deser_json::to_string(payload).map(String::into_bytes).expect("serialize token payload"));
    sign_parts(&header, &payload, secret)
}

fn sign_alg_none(payload: &deser_value::Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let payload =
        URL_SAFE_NO_PAD.encode(deser_json::to_string(payload).map(String::into_bytes).expect("serialize token payload"));
    format!("{header}.{payload}.")
}

fn post_json(body: &str) -> Vec<u8> {
    deser_json::to_string(&deser_value::value!({ "body": body })).map(String::into_bytes).expect("serialize request body")
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn create_post(client: &mut Client, token: &str, body: &str) -> deser_value::Value {
    let authorization = bearer(token);
    let body = post_json(body);
    let response = client
        .request(
            "POST",
            "/posts",
            &[
                ("Authorization", authorization.as_str()),
                ("Content-Type", "application/json"),
            ],
            Some(&body),
        )
        .expect("create post");
    assert_eq!(response.status, 201, "create body: {}", response.text());
    response.json()
}

fn assert_error(
    client: &mut Client,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    status: u16,
    message: &str,
) {
    let response = client
        .request(method, path, headers, body)
        .expect("error response");
    assert_eq!(
        response.status,
        status,
        "{method} {path}: body {}",
        response.text()
    );
    assert_eq!(
        response.text(),
        format!("{{\"error\":\"{message}\"}}"),
        "{method} {path}"
    );
}

fn raw_exchange(address: SocketAddr, request: &[u8]) -> io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))?;
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(request)?;
    let _ = stream.shutdown(Shutdown::Write);
    read_until_close(&mut stream, Instant::now() + Duration::from_secs(10))
}

fn assert_answered_or_closed(address: SocketAddr, label: &str, request: &[u8]) {
    match raw_exchange(address, request) {
        Ok(bytes) => {
            if bytes.is_empty() {
                return;
            }
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                text.starts_with("HTTP/1.1 "),
                "{label}: non-HTTP response {text:?}"
            );
            let status = text[9..12]
                .parse::<u16>()
                .unwrap_or_else(|_| panic!("{label}: malformed status line {text:?}"));
            assert!(
                (400..600).contains(&status),
                "{label}: unexpected status {status} in {text:?}"
            );
        }
        Err(error) if error.kind() == io::ErrorKind::TimedOut => {
            panic!("{label}: server neither answered nor closed: {error}");
        }
        Err(_) => {}
    }
}

fn process_fd_count(pid: u32) -> usize {
    #[cfg(target_os = "linux")]
    {
        fs::read_dir(format!("/proc/{pid}/fd"))
            .unwrap_or_else(|error| panic!("read /proc/{pid}/fd: {error}"))
            .count()
    }
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("lsof")
            .args(["-nP", "-p", &pid.to_string()])
            .output()
            .expect("run lsof");
        let text = String::from_utf8_lossy(&output.stdout);
        text.lines().filter(|line| !line.starts_with("COMMAND")).count()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        panic!("fd counting is implemented for Linux and macOS only");
    }
}

fn stable_fd_count(pid: u32, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    let mut previous = process_fd_count(pid);
    let mut stable_samples = 0;
    loop {
        thread::sleep(Duration::from_millis(50));
        let current = process_fd_count(pid);
        if current == previous {
            stable_samples += 1;
            if stable_samples >= 3 {
                return current;
            }
        } else {
            previous = current;
            stable_samples = 0;
        }
        if Instant::now() >= deadline {
            return current;
        }
    }
}

#[test]
fn keep_alive_reuses_one_connection() {
    let server = Server::start("keep-alive");
    let tokens = seed_tokens();
    let mut client = Client::connect(server.address).expect("connect");
    for index in 0..20 {
        let health = client
            .request("GET", "/health", &[], None)
            .expect("health on the shared connection");
        assert_eq!(health.status, 200);
        assert_eq!(health.json()["status"], "ok");

        let feed = client
            .request("GET", "/feed", &[], None)
            .expect("feed on the shared connection");
        assert_eq!(feed.status, 200);
        assert_eq!(feed.json()["posts"].as_seq().expect("feed posts").len(), 20);

        let body = format!("keep alive {index}");
        let created = create_post(&mut client, &tokens[index].token, &body);
        assert_eq!(created["post"]["body"], body);
        assert_eq!(created["post"]["author"], tokens[index].username);
    }
}

#[test]
fn idle_keep_alive_survives_66_seconds() {
    let server = Server::start("idle-keep-alive");
    let mut client = Client::connect(server.address).expect("connect");
    let first = client
        .request("GET", "/health", &[], None)
        .expect("first health");
    assert_eq!(first.status, 200);

    thread::sleep(Duration::from_secs(66));

    let second = client
        .request("GET", "/health", &[], None)
        .unwrap_or_else(|error| panic!("health after 66 s idle failed: {error}; {}", server.logs()));
    assert_eq!(second.status, 200);
    let feed = client
        .request("GET", "/feed", &[], None)
        .expect("feed after the idle wait");
    assert_eq!(feed.status, 200);
}

#[test]
fn production_preset_closes_idle_keep_alive_connections() {
    use h12tiny::runtime::{BoxExecutor, BoxSendFuture, FnExecutor};
    use h12tiny::server::serve;
    use h12tiny::web::{Router, get};

    let listener = smol::block_on(async {
        smol::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind production listener")
    });
    let address = listener.local_addr().expect("production listener address");
    let executor = Arc::new(smol::Executor::new());
    let box_executor = BoxExecutor::new(FnExecutor::new({
        let executor = Arc::clone(&executor);
        move |future: BoxSendFuture| {
            executor.spawn(future).detach();
        }
    }));
    let (executor_sender, executor_receiver) = smol::channel::unbounded::<()>();
    let executor_thread = thread::spawn({
        let executor = Arc::clone(&executor);
        move || {
            smol::block_on(executor.run(async move {
                let _ = executor_receiver.recv().await;
            }))
        }
    });
    let router = Router::new().route("/", get(|| async { "ok" }));
    let (shutdown_sender, shutdown_receiver) = smol::channel::bounded::<()>(1);
    let server_thread = thread::spawn(move || {
        smol::block_on(
            serve(listener, router, box_executor)
                .production()
                .shutdown_on(async move {
                    let _ = shutdown_receiver.recv().await;
                }),
        )
    });

    let mut client = Client::connect(address).expect("connect production server");
    let response = client
        .request("GET", "/", &[], None)
        .expect("first production request");
    assert_eq!(response.status, 200);
    assert_eq!(response.text(), "ok");

    let start = Instant::now();
    match observe_close(&mut client.stream, start, start + Duration::from_secs(15)) {
        CloseObservation::Closed { elapsed, bytes } => {
            eprintln!("production() closed the idle keep-alive connection after {elapsed:?}");
            assert!(
                elapsed >= Duration::from_secs(5),
                "production() closed the idle connection too early: {elapsed:?}"
            );
            assert!(
                elapsed <= Duration::from_secs(15),
                "production() closed the idle connection later than 15 s: {elapsed:?}"
            );
            if !bytes.is_empty() {
                eprintln!(
                    "production() sent {} bytes before closing: {:?}",
                    bytes.len(),
                    String::from_utf8_lossy(&bytes)
                );
            }
            let next = client.request("GET", "/", &[], None);
            assert!(
                next.is_err(),
                "production() answered a request on a connection it had already closed: {:?}",
                next.map(|response| (response.status, response.text().to_owned()))
            );
        }
        CloseObservation::Open => {
            let next = client.request("GET", "/", &[], None);
            match next {
                Ok(response) => {
                    eprintln!(
                        "production() kept the idle connection open past 15 s; second request returned {}",
                        response.status
                    );
                    assert_eq!(response.status, 200);
                }
                Err(error) => panic!(
                    "production() neither closed the idle connection within 15 s nor answered the second request: {error}"
                ),
            }
        }
    }

    smol::block_on(shutdown_sender.send(()))
        .expect("send production server shutdown");
    let result = server_thread.join().expect("join production server");
    assert!(result.is_ok(), "production server failed: {result:?}");
    smol::block_on(executor_sender.send(()))
        .expect("stop the production executor");
    executor_thread.join().expect("join production executor");
}

#[test]
fn request_body_fragmentation_is_accepted() {
    let server = Server::start("fragmented-body");
    let tokens = seed_tokens();
    let token = &tokens[0];
    let body = br#"{"body":"  fragmented request body  "}"#;
    let head = format!(
        "POST /posts HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        bearer(&token.token),
        body.len()
    );

    let mut client = Client::connect(server.address).expect("connect");
    client.stream.write_all(head.as_bytes()).expect("write head");
    for byte in body {
        client
            .stream
            .write_all(&[*byte])
            .expect("write one body byte");
    }
    client.stream.flush().expect("flush body");

    let response = client
        .read_response(Instant::now() + REQUEST_TIMEOUT)
        .expect("fragmented create response");
    assert_eq!(response.status, 201, "body: {}", response.text());
    let created = response.json();
    assert_eq!(created["post"]["body"], "fragmented request body");
    assert_eq!(created["post"]["author"], token.username);
    assert_eq!(created["post"]["like_count"], 0);
}

#[test]
fn slow_headers_within_bound_get_response() {
    let server = Server::start("slow-headers-within");
    let request = b"GET /feed HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: keep-alive\r\n\r\n";
    let mut client = Client::connect(server.address).expect("connect");
    let started = Instant::now();
    for byte in request {
        client
            .stream
            .write_all(&[*byte])
            .expect("write one header byte");
        client.stream.flush().expect("flush header byte");
        thread::sleep(Duration::from_millis(250));
    }
    let elapsed = started.elapsed();
    eprintln!("slow header request delivered in {elapsed:?}");
    assert!(elapsed > Duration::from_secs(10));

    let response = client
        .read_response(Instant::now() + REQUEST_TIMEOUT)
        .expect("slow header response");
    assert_eq!(response.status, 200, "body: {}", response.text());
    assert_eq!(
        response.json()["posts"].as_seq().expect("feed posts").len(),
        20
    );
}

#[test]
fn partial_headers_close_at_header_timeout() {
    let server = Server::start("partial-headers");
    let mut stream = TcpStream::connect_timeout(&server.address, Duration::from_secs(10))
        .expect("connect");
    stream.set_nodelay(true).expect("set nodelay");
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Partial: value")
        .expect("write partial headers");

    let start = Instant::now();
    match observe_close(&mut stream, start, start + Duration::from_secs(90)) {
        CloseObservation::Closed { elapsed, bytes } => {
            eprintln!("partial-header connection closed after {elapsed:?} with {} bytes", bytes.len());
            assert!(
                elapsed >= Duration::from_secs(70),
                "partial headers closed well before the 75 s bound: {elapsed:?}"
            );
            assert!(
                elapsed <= Duration::from_secs(90),
                "partial headers stayed open past 90 s: {elapsed:?}"
            );
        }
        CloseObservation::Open => {
            panic!("server kept a partial-header connection open past 90 s; {}", server.logs());
        }
    }

    let mut client = Client::connect(server.address).expect("connect after timeout");
    assert_eq!(
        client.request("GET", "/health", &[], None).expect("health").status,
        200
    );
}

#[test]
fn concurrent_disconnects_keep_server_healthy() {
    let server = Server::start("disconnects");
    for round in 0..2 {
        thread::scope(|scope| {
            for index in 0..100 {
                let address = server.address;
                scope.spawn(move || {
                    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))
                        .unwrap_or_else(|error| {
                            panic!("round {round} client {index}: connect failed: {error}")
                        });
                    stream.set_nodelay(true).expect("set nodelay");
                    let partial = b"POST /posts HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 100\r\n\r\n{\"body\":";
                    let _ = stream.write_all(partial);
                    let _ = stream.shutdown(Shutdown::Both);
                });
            }
        });
    }

    let mut client = Client::connect(server.address).expect("health connect");
    let health = client.request("GET", "/health", &[], None).expect("health");
    assert_eq!(health.status, 200, "body: {}", health.text());
    let feed = client.request("GET", "/feed", &[], None).expect("feed");
    assert_eq!(feed.status, 200);
}

#[test]
fn malformed_framing_is_answered_or_closed() {
    let server = Server::start("malformed");

    assert_answered_or_closed(
        server.address,
        "duplicate content-length",
        b"POST /posts HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
    );
    assert_answered_or_closed(
        server.address,
        "content-length and transfer-encoding",
        b"POST /posts HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
    );
    assert_answered_or_closed(
        server.address,
        "bad request line",
        b"GARBAGE\r\nHost: 127.0.0.1\r\n\r\n",
    );

    let mut client = Client::connect(server.address).expect("health connect");
    let health = client.request("GET", "/health", &[], None).expect("health");
    assert_eq!(health.status, 200, "body: {}", health.text());
}

#[test]
fn concurrent_writes_and_duplicate_likes() {
    let server = Server::start("concurrent-writes");
    let tokens = seed_tokens();
    let address = server.address;

    let created = thread::scope(|scope| {
        let handles: Vec<_> = (0..32)
            .map(|index| {
                scope.spawn(move || {
                    let mut client = Client::connect(address).expect("create connect");
                    let body = format!("concurrent create {index}");
                    let value = create_post(&mut client, &seed_tokens()[index].token, &body);
                    (value["post"]["id"].as_i64().expect("post id"), body)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("create thread"))
            .collect::<Vec<_>>()
    });
    assert_eq!(created.len(), 32);

    let (target, _) = created
        .iter()
        .max_by_key(|(id, _)| *id)
        .expect("created post");
    let target = *target;

    let like_user = &tokens[1];
    let statuses = thread::scope(|scope| {
        let handles: Vec<_> = (0..32)
            .map(|_| {
                scope.spawn(move || {
                    let mut client = Client::connect(address).expect("like connect");
                    let authorization = bearer(&like_user.token);
                    let response = client
                        .request(
                            "POST",
                            &format!("/posts/{target}/like"),
                            &[("Authorization", authorization.as_str())],
                            None,
                        )
                        .expect("like request");
                    let value = response.json();
                    (
                        response.status,
                        value["liked"].as_bool().expect("liked bool"),
                        value["already_liked"].as_bool().expect("already_liked bool"),
                        value["post_id"].as_i64().expect("like post id"),
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("like thread"))
            .collect::<Vec<_>>()
    });

    let first_time = statuses
        .iter()
        .filter(|(status, _, already_liked, _)| *status == 201 && !already_liked)
        .count();
    let repeats = statuses
        .iter()
        .filter(|(status, _, already_liked, _)| *status == 200 && *already_liked)
        .count();
    assert_eq!(first_time, 1, "statuses: {statuses:?}");
    assert_eq!(repeats, 31, "statuses: {statuses:?}");
    assert_eq!(statuses.len(), 32);
    for (_, liked, _, post_id) in &statuses {
        assert!(*liked);
        assert_eq!(*post_id, target);
    }

    let mut client = Client::connect(address).expect("read-back connect");
    let read_back = client
        .request("GET", &format!("/posts/{target}"), &[], None)
        .expect("read back");
    assert_eq!(read_back.status, 200);
    assert_eq!(read_back.json()["post"]["like_count"], 1);

    let feed = client.request("GET", "/feed", &[], None).expect("feed");
    assert_eq!(feed.status, 200);
    let posts = feed.json()["posts"].as_seq().expect("feed posts").clone();
    assert_eq!(posts.len(), 20);
    let visible = posts
        .iter()
        .find(|post| post["id"].as_i64() == Some(target))
        .unwrap_or_else(|| panic!("new post {target} is not in the feed"));
    assert_eq!(visible["like_count"], 1);
}

#[test]
fn invalid_and_expired_tokens_use_spec_messages() {
    let server = Server::start("tokens");
    let tokens = seed_tokens();
    let valid = &tokens[0];
    let now = unix_now();
    let mut client = Client::connect(server.address).expect("connect");

    let body = post_json("token check");
    let authorization = bearer(&valid.token);
    let created = client
        .request(
            "POST",
            "/posts",
            &[
                ("Authorization", authorization.as_str()),
                ("Content-Type", "application/json"),
            ],
            Some(&body),
        )
        .expect("valid token create");
    assert_eq!(created.status, 201);

    assert_error(
        &mut client,
        "POST",
        "/posts",
        &[],
        Some(&body),
        401,
        "missing bearer token",
    );
    assert_error(
        &mut client,
        "POST",
        "/posts",
        &[("Authorization", "Basic abc")],
        Some(&body),
        401,
        "missing bearer token",
    );

    let garbage = bearer("abc");
    assert_error(
        &mut client,
        "POST",
        "/posts",
        &[("Authorization", garbage.as_str())],
        Some(&body),
        401,
        "invalid or expired token",
    );

    let expired = sign_payload(&deser_value::value!({
        "sub": valid.username.clone(),
        "username": valid.username.clone(),
        "iat": now - 7200,
        "exp": now - 3600,
    }));
    let authorization = bearer(&expired);
    assert_error(
        &mut client,
        "POST",
        "/posts",
        &[("Authorization", authorization.as_str())],
        Some(&body),
        401,
        "invalid or expired token",
    );

    let bad_signature = sign_payload_with_secret(
        &deser_value::value!({
            "sub": "1",
            "username": valid.username.clone(),
            "iat": now,
            "exp": now + 3600,
        }),
        b"not-the-challenge-secret",
    );
    let authorization = bearer(&bad_signature);
    assert_error(
        &mut client,
        "POST",
        "/posts",
        &[("Authorization", authorization.as_str())],
        Some(&body),
        401,
        "invalid or expired token",
    );

    let none = sign_alg_none(&deser_value::value!({
        "sub": "1",
        "username": valid.username.clone(),
        "iat": now,
        "exp": now + 3600,
    }));
    let authorization = bearer(&none);
    assert_error(
        &mut client,
        "POST",
        "/posts",
        &[("Authorization", authorization.as_str())],
        Some(&body),
        401,
        "invalid or expired token",
    );

    let bad_sub = sign_payload(&deser_value::value!({
        "sub": "abc",
        "username": valid.username.clone(),
        "iat": now,
        "exp": now + 3600,
    }));
    let authorization = bearer(&bad_sub);
    assert_error(
        &mut client,
        "POST",
        "/posts",
        &[("Authorization", authorization.as_str())],
        Some(&body),
        401,
        "invalid token payload",
    );

    let zero_sub = sign_payload(&deser_value::value!({
        "sub": "0",
        "username": valid.username.clone(),
        "iat": now,
        "exp": now + 3600,
    }));
    let authorization = bearer(&zero_sub);
    assert_error(
        &mut client,
        "POST",
        "/posts",
        &[("Authorization", authorization.as_str())],
        Some(&body),
        401,
        "invalid token payload",
    );

    let no_username = sign_payload(&deser_value::value!({
        "sub": "1",
        "iat": now,
        "exp": now + 3600,
    }));
    let authorization = bearer(&no_username);
    assert_error(
        &mut client,
        "POST",
        "/posts",
        &[("Authorization", authorization.as_str())],
        Some(&body),
        401,
        "invalid token payload",
    );

    let numeric_username = sign_payload(&deser_value::value!({
        "sub": "1",
        "username": 7,
        "iat": now,
        "exp": now + 3600,
    }));
    let authorization = bearer(&numeric_username);
    assert_error(
        &mut client,
        "POST",
        "/posts",
        &[("Authorization", authorization.as_str())],
        Some(&body),
        401,
        "invalid token payload",
    );
}

fn exercise_signal_shutdown(name: &str, signal: &str) {
    let mut server = Server::start(name);
    let mut client = Client::connect(server.address).expect("signal connect");
    assert_eq!(
        client.request("GET", "/health", &[], None).expect("health").status,
        200
    );
    assert_eq!(
        client.request("GET", "/feed", &[], None).expect("feed").status,
        200
    );

    let started = Instant::now();
    server.send_signal(signal);
    match observe_close(&mut client.stream, started, started + Duration::from_secs(10)) {
        CloseObservation::Closed { elapsed, .. } => {
            eprintln!("{signal}: open connection closed after {elapsed:?}");
        }
        CloseObservation::Open => {
            panic!("{signal}: the open connection was not closed during shutdown; {}", server.logs());
        }
    }
    drop(client);

    let status = server.wait_for_exit(Duration::from_secs(30));
    assert_eq!(
        status.code(),
        Some(0),
        "{signal}: exit status {status:?}; {}",
        server.logs()
    );

    let summary = server.shutdown_summary();
    for key in [
        "accepted",
        "rejected",
        "responses",
        "status_200",
        "status_201",
        "status_400",
        "status_401",
        "status_404",
        "status_500",
        "status_other",
    ] {
        assert!(
            summary.contains_key(key),
            "{signal}: shutdown summary is missing {key}; {}",
            server.logs()
        );
    }
    assert!(
        summary["accepted"] >= 1,
        "{signal}: summary accepted={}; {}",
        summary["accepted"],
        server.logs()
    );
    assert_eq!(summary["rejected"], 0, "{signal}: unexpected rejected count");
    assert!(
        summary["responses"] >= 2,
        "{signal}: summary responses={}; {}",
        summary["responses"],
        server.logs()
    );
    assert!(
        summary["status_200"] >= 2,
        "{signal}: summary status_200={}; {}",
        summary["status_200"],
        server.logs()
    );
}

#[test]
fn sigterm_and_sigint_drain_and_report() {
    exercise_signal_shutdown("signal-term", "TERM");
    exercise_signal_shutdown("signal-int", "INT");
}

#[test]
fn file_descriptors_return_to_baseline() {
    let server = Server::start("file-descriptors");
    let pid = server.child.id();
    let baseline = stable_fd_count(pid, Duration::from_secs(5));

    for index in 0..1000 {
        let stream = TcpStream::connect_timeout(&server.address, Duration::from_secs(10))
            .unwrap_or_else(|error| panic!("connect cycle {index}: {error}"));
        stream.set_nodelay(true).expect("set nodelay");
        let _ = stream.shutdown(Shutdown::Both);
        drop(stream);
        if index % 100 == 99 {
            let mut client = Client::connect(server.address).expect("pace connect");
            let health = client.request("GET", "/health", &[], None).expect("pace health");
            assert_eq!(health.status, 200);
        }
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut final_count = process_fd_count(pid);
    while Instant::now() < deadline {
        final_count = process_fd_count(pid);
        if final_count <= baseline + FD_TOLERANCE {
            eprintln!(
                "fd baseline={baseline} final={final_count} tolerance={FD_TOLERANCE}"
            );
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "file descriptors did not return to baseline: baseline={baseline}, final={final_count}, tolerance={FD_TOLERANCE}; {}",
        server.logs()
    );
}
