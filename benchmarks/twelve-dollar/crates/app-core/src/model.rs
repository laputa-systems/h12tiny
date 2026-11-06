use deser::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Post {
    pub id: i64,
    pub body: String,
    pub created_at: String,
    pub author: String,
    pub like_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeedResponse {
    pub posts: Vec<Post>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PostResponse {
    pub post: Post,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LikeResponse {
    pub liked: bool,
    pub already_liked: bool,
    pub post_id: i64,
}

impl LikeResponse {
    pub fn status_code(&self) -> u16 {
        if self.already_liked { 200 } else { 201 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthOk {
    pub status: &'static str,
    pub db: &'static str,
    pub uptime_s: u64,
}

impl HealthOk {
    pub const STATUS_CODE: u16 = 200;

    pub fn new(uptime_s: u64) -> Self {
        Self {
            status: "ok",
            db: "ok",
            uptime_s,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthDegraded {
    pub status: &'static str,
    pub db: &'static str,
    pub error: String,
}

impl HealthDegraded {
    pub const STATUS_CODE: u16 = 503;

    pub fn new(error: String) -> Self {
        Self {
            status: "degraded",
            db: "unreachable",
            error,
        }
    }
}
