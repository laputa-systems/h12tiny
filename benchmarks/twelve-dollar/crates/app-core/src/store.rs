use std::fmt;
use std::future::{Future, poll_fn};
use std::io;
use std::path::Path;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use futures_channel::mpsc::{self, Receiver, Sender};
use futures_channel::oneshot;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params};

use crate::input::validate_body;
use crate::{
    AppError, Config, FeedResponse, HealthDegraded, HealthOk, Identity, LikeResponse, Post,
    PostResponse,
};

const READ_QUEUE_CAPACITY: usize = 4096;
const WRITE_QUEUE_CAPACITY: usize = 4096;
const BUSY_TIMEOUT_MILLIS: u64 = 5000;

const FEED_SQL: &str = "
    SELECT p.id, p.body, p.created_at, u.username,
           (SELECT count(*) FROM likes l WHERE l.post_id = p.id) AS like_count
      FROM posts p JOIN users u ON u.id = p.user_id
     ORDER BY p.created_at DESC, p.id DESC
     LIMIT 20
";

const POST_SQL: &str = "
    SELECT p.id, p.body, p.created_at, u.username,
           (SELECT count(*) FROM likes l WHERE l.post_id = p.id) AS like_count
      FROM posts p JOIN users u ON u.id = p.user_id
     WHERE p.id = ?1
";

const CREATE_SQL: &str =
    "INSERT INTO posts (user_id, body) VALUES (?1, ?2) RETURNING id, created_at";

const POST_EXISTS_SQL: &str = "SELECT id FROM posts WHERE id = ?1";

const LIKE_SQL: &str =
    "INSERT INTO likes (user_id, post_id) VALUES (?1, ?2) ON CONFLICT (user_id, post_id) DO NOTHING";

const HEALTH_SQL: &str = "SELECT 1";

pub struct Store {
    readers: Vec<Sender<ReadJob>>,
    writer: Sender<WriteJob>,
    next_reader: AtomicUsize,
    start: Instant,
    workers: Vec<JoinHandle<()>>,
}

enum ReadJob {
    Feed(oneshot::Sender<Result<FeedResponse, AppError>>),
    Post {
        id: i64,
        reply: oneshot::Sender<Result<PostResponse, AppError>>,
    },
    Health(oneshot::Sender<Result<(), String>>),
}

enum WriteJob {
    CreatePost {
        user_id: i64,
        username: String,
        body: String,
        reply: oneshot::Sender<Result<PostResponse, AppError>>,
    },
    LikePost {
        user_id: i64,
        post_id: i64,
        reply: oneshot::Sender<Result<LikeResponse, AppError>>,
    },
}

impl Store {
    pub fn open(config: &Config, readers: usize) -> Result<Self, StoreError> {
        if !(1..=4).contains(&readers) {
            return Err(StoreError::ReaderCount(readers));
        }

        let writer_connection = open_connection(&config.sqlite_path)?;
        let mut reader_connections = Vec::with_capacity(readers);
        for _ in 0..readers {
            reader_connections.push(open_connection(&config.sqlite_path)?);
        }

        let mut workers = Vec::with_capacity(readers + 1);
        let mut reader_senders = Vec::with_capacity(readers);
        for (index, connection) in reader_connections.into_iter().enumerate() {
            let (sender, receiver) = mpsc::channel(READ_QUEUE_CAPACITY);
            let handle = thread::Builder::new()
                .name(format!("app-core-read-{index}"))
                .spawn(move || run_reader(connection, receiver))
                .map_err(StoreError::Io)?;
            reader_senders.push(sender);
            workers.push(handle);
        }

        let (writer_sender, writer_receiver) = mpsc::channel(WRITE_QUEUE_CAPACITY);
        let handle = thread::Builder::new()
            .name("app-core-write".to_owned())
            .spawn(move || run_writer(writer_connection, writer_receiver))
            .map_err(StoreError::Io)?;
        workers.push(handle);

        Ok(Self {
            readers: reader_senders,
            writer: writer_sender,
            next_reader: AtomicUsize::new(0),
            start: Instant::now(),
            workers,
        })
    }

    pub async fn feed(&self) -> Result<FeedResponse, AppError> {
        let (reply, receiver) = oneshot::channel();
        self.send_read(ReadJob::Feed(reply)).await?;
        receiver.await.map_err(|_| AppError::Internal)?
    }

    pub async fn post(&self, id: i64) -> Result<PostResponse, AppError> {
        let (reply, receiver) = oneshot::channel();
        self.send_read(ReadJob::Post { id, reply }).await?;
        receiver.await.map_err(|_| AppError::Internal)?
    }

    pub async fn create_post(
        &self,
        author: &Identity,
        body: &str,
    ) -> Result<PostResponse, AppError> {
        let body = validate_body(body)?.to_owned();
        let (reply, receiver) = oneshot::channel();
        self.send_write(WriteJob::CreatePost {
            user_id: author.user_id,
            username: author.username.clone(),
            body,
            reply,
        })
        .await?;
        receiver.await.map_err(|_| AppError::Internal)?
    }

    pub async fn like_post(
        &self,
        user: &Identity,
        post_id: i64,
    ) -> Result<LikeResponse, AppError> {
        let (reply, receiver) = oneshot::channel();
        self.send_write(WriteJob::LikePost {
            user_id: user.user_id,
            post_id,
            reply,
        })
        .await?;
        receiver.await.map_err(|_| AppError::Internal)?
    }

    pub async fn health(&self) -> Result<HealthOk, HealthDegraded> {
        let unavailable = || HealthDegraded::new("database worker unavailable".to_owned());
        let (reply, receiver) = oneshot::channel();
        self.send_read(ReadJob::Health(reply))
            .await
            .map_err(|_| unavailable())?;
        receiver
            .await
            .map_err(|_| unavailable())?
            .map_err(HealthDegraded::new)?;
        Ok(HealthOk::new(self.start.elapsed().as_secs()))
    }

    async fn send_read(&self, job: ReadJob) -> Result<(), AppError> {
        let index = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        send(&self.readers[index], job).await
    }

    async fn send_write(&self, job: WriteJob) -> Result<(), AppError> {
        send(&self.writer, job).await
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        for reader in &mut self.readers {
            reader.close_channel();
        }
        self.readers.clear();
        self.writer.close_channel();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

async fn send<T>(sender: &Sender<T>, job: T) -> Result<(), AppError> {
    let mut sender = sender.clone();
    let mut job = match sender.try_send(job) {
        Ok(()) => return Ok(()),
        Err(error) if error.is_full() => error.into_inner(),
        Err(_) => return Err(AppError::Internal),
    };
    loop {
        match poll_fn(|context| sender.poll_ready(context)).await {
            Ok(()) => {}
            Err(_) => return Err(AppError::Internal),
        }
        match sender.try_send(job) {
            Ok(()) => return Ok(()),
            Err(error) if error.is_full() => job = error.into_inner(),
            Err(_) => return Err(AppError::Internal),
        }
    }
}

fn run_reader(connection: Connection, mut receiver: Receiver<ReadJob>) {
    while let Ok(job) = block_on(receiver.recv()) {
        serve_read(&connection, job);
    }
}

fn run_writer(mut connection: Connection, mut receiver: Receiver<WriteJob>) {
    while let Ok(job) = block_on(receiver.recv()) {
        serve_write(&mut connection, job);
    }
}

fn serve_read(connection: &Connection, job: ReadJob) {
    match job {
        ReadJob::Feed(reply) => {
            let response = query_feed(connection).map_err(|error| internal("feed", error));
            let _ = reply.send(response);
        }
        ReadJob::Post { id, reply } => {
            let response = match query_post(connection, id) {
                Ok(Some(response)) => Ok(response),
                Ok(None) => Err(AppError::PostNotFound),
                Err(error) => Err(internal("post", error)),
            };
            let _ = reply.send(response);
        }
        ReadJob::Health(reply) => {
            let response = query_health(connection).map_err(|error| error.to_string());
            let _ = reply.send(response);
        }
    }
}

fn serve_write(connection: &mut Connection, job: WriteJob) {
    match job {
        WriteJob::CreatePost {
            user_id,
            username,
            body,
            reply,
        } => {
            let response = insert_post(connection, user_id, &username, &body)
                .map_err(|error| internal("create post", error));
            let _ = reply.send(response);
        }
        WriteJob::LikePost {
            user_id,
            post_id,
            reply,
        } => {
            let response = match insert_like(connection, user_id, post_id) {
                Ok(Some(response)) => Ok(response),
                Ok(None) => Err(AppError::PostNotFound),
                Err(error) => Err(internal("like post", error)),
            };
            let _ = reply.send(response);
        }
    }
}

fn query_feed(connection: &Connection) -> rusqlite::Result<FeedResponse> {
    let mut statement = connection.prepare_cached(FEED_SQL)?;
    let rows = statement.query_map(params![], map_post)?;
    let posts = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(FeedResponse { posts })
}

fn query_post(connection: &Connection, id: i64) -> rusqlite::Result<Option<PostResponse>> {
    let mut statement = connection.prepare_cached(POST_SQL)?;
    let post = statement.query_row(params![id], map_post).optional()?;
    Ok(post.map(|post| PostResponse { post }))
}

fn query_health(connection: &Connection) -> rusqlite::Result<()> {
    connection
        .prepare_cached(HEALTH_SQL)?
        .query_row(params![], |_| Ok(()))
}

fn insert_post(
    connection: &Connection,
    user_id: i64,
    username: &str,
    body: &str,
) -> rusqlite::Result<PostResponse> {
    let mut statement = connection.prepare_cached(CREATE_SQL)?;
    let (id, created_at) = statement.query_row(params![user_id, body], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    Ok(PostResponse {
        post: Post {
            id,
            body: body.to_owned(),
            created_at,
            author: username.to_owned(),
            like_count: 0,
        },
    })
}

fn insert_like(
    connection: &mut Connection,
    user_id: i64,
    post_id: i64,
) -> rusqlite::Result<Option<LikeResponse>> {
    let transaction = connection.transaction()?;
    let exists = transaction
        .prepare_cached(POST_EXISTS_SQL)?
        .query_row(params![post_id], |_| Ok(()))
        .optional()?
        .is_some();
    if !exists {
        return Ok(None);
    }
    let inserted = transaction
        .prepare_cached(LIKE_SQL)?
        .execute(params![user_id, post_id])?;
    transaction.commit()?;
    Ok(Some(LikeResponse {
        liked: true,
        already_liked: inserted == 0,
        post_id,
    }))
}

fn map_post(row: &Row<'_>) -> rusqlite::Result<Post> {
    Ok(Post {
        id: row.get(0)?,
        body: row.get(1)?,
        created_at: row.get(2)?,
        author: row.get(3)?,
        like_count: row.get(4)?,
    })
}

fn internal(context: &str, error: rusqlite::Error) -> AppError {
    eprintln!("app-core {context} query failed: {error}");
    AppError::Internal
}

fn open_connection(path: &Path) -> Result<Connection, StoreError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(StoreError::Sqlite)?;
    configure(&connection)?;
    Ok(connection)
}

fn configure(connection: &Connection) -> Result<(), StoreError> {
    let journal_mode: String = connection
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(StoreError::Sqlite)?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::Pragma {
            name: "journal_mode",
            value: journal_mode,
        });
    }

    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(StoreError::Sqlite)?;
    let synchronous: i64 = connection
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .map_err(StoreError::Sqlite)?;
    if synchronous != 1 {
        return Err(StoreError::Pragma {
            name: "synchronous",
            value: synchronous.to_string(),
        });
    }

    connection
        .busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MILLIS))
        .map_err(StoreError::Sqlite)?;
    let busy_timeout: i64 = connection
        .pragma_query_value(None, "busy_timeout", |row| row.get(0))
        .map_err(StoreError::Sqlite)?;
    if busy_timeout != BUSY_TIMEOUT_MILLIS as i64 {
        return Err(StoreError::Pragma {
            name: "busy_timeout",
            value: busy_timeout.to_string(),
        });
    }

    Ok(())
}

fn block_on<F: Future>(future: F) -> F::Output {
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

#[derive(Debug)]
pub enum StoreError {
    ReaderCount(usize),
    Sqlite(rusqlite::Error),
    Pragma {
        name: &'static str,
        value: String,
    },
    Io(io::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReaderCount(readers) => {
                write!(formatter, "reader count must be between 1 and 4, got {readers}")
            }
            Self::Sqlite(error) => write!(formatter, "sqlite: {error}"),
            Self::Pragma { name, value } => {
                write!(formatter, "sqlite pragma {name} has unexpected value {value}")
            }
            Self::Io(error) => write!(formatter, "worker thread: {error}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::ReaderCount(_) | Self::Pragma { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::open_connection;

    #[test]
    fn opens_connections_with_the_required_pragmas() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/app-core-unit");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pragmas.db");
        let base = path.to_string_lossy().into_owned();
        for suffix in ["", "-wal", "-shm"] {
            let _ = fs::remove_file(format!("{base}{suffix}"));
        }
        drop(rusqlite::Connection::open(&path).unwrap());

        let connection = open_connection(&path).unwrap();
        let journal_mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");
        let synchronous: i64 = connection
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .unwrap();
        assert_eq!(synchronous, 1);
        let busy_timeout: i64 = connection
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();
        assert_eq!(busy_timeout, 5000);
    }
}
