use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use app_core::{
    AppError, ErrorResponse, Store, parse_create_body, parse_post_id, verify_authorization,
};
use h12tiny::web::{
    Bytes, FromRequest, HeaderValue, IntoResponse, Path, Rejection, Request, RequestMeta, Response,
    Router, State, StatusCode, get, post,
};

const BODY_LIMIT: usize = 64 * 1024;
const INTERNAL_ERROR_BODY: &[u8] = br#"{"error":"internal server error"}"#;

pub struct AppState {
    pub store: Store,
    pub secret: Vec<u8>,
}

pub fn build(state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/health", get(health))
        .route("/feed", get(feed))
        .route("/posts", post(create_post).body_limit(BODY_LIMIT))
        .route("/posts/{id}", get(post_by_id))
        .route("/posts/{id}/like", post(like_post).body_limit(BODY_LIMIT))
        .fallback(not_found)
        .with_state(state)
}

pub struct Authorization(Option<HeaderValue>);

impl Authorization {
    fn token(&self) -> Option<&str> {
        self.0.as_ref().and_then(|value| value.to_str().ok())
    }
}

impl<S> FromRequest<S> for Authorization
where
    S: Send + Sync + 'static,
{
    type Rejection = Rejection;

    fn from_request(
        request: Request,
        _state: &Option<S>,
        _meta: &RequestMeta,
    ) -> Pin<Box<dyn Future<Output = Result<(Self, Request), Self::Rejection>> + Send>> {
        let value = request.headers().get("authorization").cloned();
        Box::pin(async move { Ok((Self(value), request)) })
    }
}

async fn health(State(state): State<Arc<AppState>>) -> Response {
    match state.store.health().await {
        Ok(health) => encode(StatusCode::OK, || deser_json::to_string(&health).map(String::into_bytes)),
        Err(degraded) => encode(StatusCode::SERVICE_UNAVAILABLE, || {
            deser_json::to_string(&degraded).map(String::into_bytes)
        }),
    }
}

async fn feed(State(state): State<Arc<AppState>>) -> Response {
    match state.store.feed().await {
        Ok(feed) => encode(StatusCode::OK, || deser_json::to_string(&feed).map(String::into_bytes)),
        Err(error) => error_response(error),
    }
}

async fn post_by_id(State(state): State<Arc<AppState>>, Path(raw): Path<String>) -> Response {
    let id = match parse_post_id(&raw) {
        Ok(id) => id,
        Err(error) => return error_response(error),
    };
    match state.store.post(id).await {
        Ok(post) => encode(StatusCode::OK, || deser_json::to_string(&post).map(String::into_bytes)),
        Err(error) => error_response(error),
    }
}

async fn create_post(
    State(state): State<Arc<AppState>>,
    authorization: Authorization,
    body: Bytes,
) -> Response {
    let identity = match verify_authorization(authorization.token(), &state.secret) {
        Ok(identity) => identity,
        Err(error) => return error_response(error),
    };
    let body = match parse_create_body(&body) {
        Ok(body) => body,
        Err(error) => return error_response(error),
    };
    match state.store.create_post(&identity, &body).await {
        Ok(post) => encode(StatusCode::CREATED, || deser_json::to_string(&post).map(String::into_bytes)),
        Err(error) => error_response(error),
    }
}

async fn like_post(
    State(state): State<Arc<AppState>>,
    authorization: Authorization,
    Path(raw): Path<String>,
) -> Response {
    let identity = match verify_authorization(authorization.token(), &state.secret) {
        Ok(identity) => identity,
        Err(error) => return error_response(error),
    };
    let id = match parse_post_id(&raw) {
        Ok(id) => id,
        Err(error) => return error_response(error),
    };
    match state.store.like_post(&identity, id).await {
        Ok(like) => {
            let status = StatusCode::from_u16(like.status_code()).unwrap_or(StatusCode::OK);
            encode(status, || deser_json::to_string(&like).map(String::into_bytes))
        }
        Err(error) => error_response(error),
    }
}

async fn not_found(_request: Request) -> Response {
    error_response(AppError::NotFound)
}

fn error_response(error: AppError) -> Response {
    let status =
        StatusCode::from_u16(error.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    encode(status, || deser_json::to_string(&ErrorResponse::from(error)).map(String::into_bytes))
}

fn encode<E>(status: StatusCode, body: impl FnOnce() -> Result<Vec<u8>, E>) -> Response {
    match body() {
        Ok(bytes) => with_body(status, bytes),
        Err(_) => with_body(
            StatusCode::INTERNAL_SERVER_ERROR,
            INTERNAL_ERROR_BODY.to_vec(),
        ),
    }
}

fn with_body(status: StatusCode, body: Vec<u8>) -> Response {
    let mut response = body.into_response();
    *response.status_mut() = status;
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/json"),
    );
    response
}
