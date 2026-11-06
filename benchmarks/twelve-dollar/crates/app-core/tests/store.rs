mod common;

use std::collections::HashSet;
use std::sync::Arc;
use std::thread;

use app_core::{AppError, Config, Identity, Store, StoreError};
use common::{TestDb, block_on};
use rusqlite::params;

fn config(db: &TestDb) -> Config {
    Config {
        sqlite_path: db.path.clone(),
        jwt_secret: b"twelve-dollar-challenge".to_vec(),
        host: "127.0.0.1".parse().unwrap(),
        port: 3000,
    }
}

fn open_store(db: &TestDb) -> Store {
    Store::open(&config(db), 2).expect("open store")
}

fn seed_users(db: &TestDb) {
    db.connection()
        .execute_batch(
            "INSERT INTO users (id, username) VALUES
                 (1, 'alice'),
                 (2, 'bob'),
                 (3, 'carol');",
        )
        .unwrap();
}

fn identity(user_id: i64, username: &str) -> Identity {
    Identity {
        user_id,
        username: username.to_owned(),
    }
}

#[test]
fn enables_wal_on_the_database() {
    let db = TestDb::new("pragmas");
    let store = open_store(&db);
    let connection = db.connection();
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
    drop(store);
}

#[test]
fn rejects_reader_counts_outside_one_to_four() {
    let db = TestDb::new("reader-count");
    for readers in [0, 5] {
        let error = Store::open(&config(&db), readers).err().unwrap();
        assert!(matches!(error, StoreError::ReaderCount(value) if value == readers));
    }
}

#[test]
fn rejects_a_missing_database_file() {
    let db = TestDb::new("missing-db");
    let mut settings = config(&db);
    settings.sqlite_path = db.path.with_file_name("absent.db");
    let error = Store::open(&settings, 1).err().unwrap();
    assert!(matches!(error, StoreError::Sqlite(_)));
}

#[test]
fn feed_returns_the_twenty_newest_posts_in_order() {
    let db = TestDb::new("feed-order");
    seed_users(&db);
    let connection = db.connection();
    for id in 1..=20 {
        connection
            .execute(
                "INSERT INTO posts (id, user_id, body, created_at)
                 VALUES (?1, 1, ?2, '2025-01-01T00:00:00.000Z')",
                params![id, format!("post {id}")],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO posts (id, user_id, body, created_at)
             VALUES (21, 1, 'post 21', '2025-01-01T00:00:01.000Z')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO posts (id, user_id, body, created_at)
             VALUES (22, 1, 'post 22', '2024-12-31T23:59:59.000Z')",
            [],
        )
        .unwrap();
    drop(connection);

    let store = open_store(&db);
    let feed = block_on(store.feed()).unwrap();
    assert_eq!(feed.posts.len(), 20);
    let ids: Vec<i64> = feed.posts.iter().map(|post| post.id).collect();
    let expected: Vec<i64> = (2..=21).rev().collect();
    assert_eq!(ids, expected);
    assert_eq!(feed.posts[0].body, "post 21");
    assert!(feed.posts.iter().all(|post| post.author == "alice"));
    assert!(feed.posts.iter().all(|post| post.like_count == 0));
}

#[test]
fn reads_a_single_post_and_reports_missing_posts() {
    let db = TestDb::new("single-post");
    seed_users(&db);
    db.connection()
        .execute(
            "INSERT INTO posts (id, user_id, body, created_at)
             VALUES (1, 2, 'hello', '2025-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
    let store = open_store(&db);

    let response = block_on(store.post(1)).unwrap();
    assert_eq!(response.post.id, 1);
    assert_eq!(response.post.body, "hello");
    assert_eq!(response.post.author, "bob");

    for id in [0, 2, 999999] {
        assert_eq!(block_on(store.post(id)), Err(AppError::PostNotFound));
    }
}

#[test]
fn creates_a_trimmed_post_that_is_immediately_readable() {
    let db = TestDb::new("create");
    seed_users(&db);
    let store = open_store(&db);
    let author = identity(2, "bob");

    let created = block_on(store.create_post(&author, "  test: <b> & café ✓  ")).unwrap();
    let post = &created.post;
    assert_eq!(post.body, "test: <b> & café ✓");
    assert_eq!(post.author, "bob");
    assert_eq!(post.like_count, 0);
    assert_eq!(post.created_at.len(), 24);
    assert_eq!(post.created_at.as_bytes()[10], b'T');
    assert_eq!(post.created_at.as_bytes()[19], b'.');
    assert!(post.created_at.ends_with('Z'));

    let read = block_on(store.post(post.id)).unwrap();
    assert_eq!(&read.post, post);

    let feed = block_on(store.feed()).unwrap();
    assert_eq!(&feed.posts[0], post);

    let five_hundred = "é".repeat(500);
    let long = block_on(store.create_post(&author, &five_hundred)).unwrap();
    assert_eq!(long.post.body.chars().count(), 500);

    let five_hundred_one = "é".repeat(501);
    assert_eq!(
        block_on(store.create_post(&author, &five_hundred_one)),
        Err(AppError::BodyTooLong)
    );
    assert_eq!(
        block_on(store.create_post(&author, "   \n\t ")),
        Err(AppError::BodyRequired)
    );

    let count: i64 = db
        .connection()
        .query_row("SELECT count(*) FROM posts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn likes_are_idempotent_per_user_and_visible_immediately() {
    let db = TestDb::new("likes");
    seed_users(&db);
    let store = open_store(&db);
    let author = identity(2, "bob");
    let alice = identity(1, "alice");
    let carol = identity(3, "carol");
    let post = block_on(store.create_post(&author, "like me"))
        .unwrap()
        .post;

    let first = block_on(store.like_post(&alice, post.id)).unwrap();
    assert!(first.liked);
    assert!(!first.already_liked);
    assert_eq!(first.post_id, post.id);
    assert_eq!(first.status_code(), 201);

    let read = block_on(store.post(post.id)).unwrap();
    assert_eq!(read.post.like_count, 1);
    let feed = block_on(store.feed()).unwrap();
    assert_eq!(feed.posts[0].like_count, 1);

    let repeat = block_on(store.like_post(&alice, post.id)).unwrap();
    assert!(repeat.already_liked);
    assert_eq!(repeat.status_code(), 200);

    let other = block_on(store.like_post(&carol, post.id)).unwrap();
    assert!(!other.already_liked);
    assert_eq!(other.status_code(), 201);

    let read = block_on(store.post(post.id)).unwrap();
    assert_eq!(read.post.like_count, 2);

    assert_eq!(
        block_on(store.like_post(&alice, 999999)),
        Err(AppError::PostNotFound)
    );
}

#[test]
fn concurrent_creates_are_all_committed_before_they_return() {
    let db = TestDb::new("concurrent-creates");
    seed_users(&db);
    let store = Arc::new(open_store(&db));
    let mut handles = Vec::new();

    for thread_index in 0..8 {
        let store = Arc::clone(&store);
        handles.push(thread::spawn(move || {
            let author = identity(1, "alice");
            let mut ids = Vec::new();
            for post_index in 0..10 {
                let body = format!("thread {thread_index} post {post_index}");
                let created = block_on(store.create_post(&author, &body)).unwrap();
                assert_eq!(created.post.body, body);
                assert_eq!(created.post.author, "alice");
                assert!(!created.post.created_at.is_empty());
                ids.push(created.post.id);
            }
            ids
        }));
    }

    let mut ids = HashSet::new();
    for handle in handles {
        for id in handle.join().unwrap() {
            assert!(ids.insert(id), "duplicate id {id}");
        }
    }
    assert_eq!(ids.len(), 80);

    let count: i64 = db
        .connection()
        .query_row("SELECT count(*) FROM posts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 80);

    let feed = block_on(store.feed()).unwrap();
    assert_eq!(feed.posts.len(), 20);
    assert!(feed.posts.windows(2).all(|pair| {
        (pair[0].created_at.as_str(), pair[0].id) >= (pair[1].created_at.as_str(), pair[1].id)
    }));
}

#[test]
fn store_can_be_shared_across_tasks() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Store>();
    assert_send_sync::<Config>();
}

#[test]
fn health_reports_ok_and_uptime() {
    let db = TestDb::new("health");
    let store = open_store(&db);
    let health = block_on(store.health()).unwrap();
    assert_eq!(health.status, "ok");
    assert_eq!(health.db, "ok");
    assert!(health.uptime_s < 60);
    let json = deser_json::to_string(&health).unwrap();
    assert!(json.starts_with(r#"{"status":"ok","db":"ok","uptime_s":"#));
    assert!(json.ends_with('}'));
}
