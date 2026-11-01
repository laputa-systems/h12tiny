mod common;

use std::time::{SystemTime, UNIX_EPOCH};

use app_core::{
    AppError, ErrorResponse, FeedResponse, HealthDegraded, HealthOk, Identity, LikeResponse, Post,
    PostResponse, parse_create_body, parse_post_id, verify_authorization,
};

use common::{sign_token, token};

const SECRET: &[u8] = b"twelve-dollar-challenge";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn verify(token: &str) -> Result<Identity, AppError> {
    verify_authorization(Some(&bearer(token)), SECRET)
}

fn valid_payload() -> String {
    let now = now();
    format!(
        r#"{{"sub":"7","username":"alice","iat":{now},"exp":{}}}"#,
        now + 3600
    )
}

#[test]
fn accepts_a_valid_token() {
    let identity = verify(&token(SECRET, &valid_payload())).unwrap();
    assert_eq!(
        identity,
        Identity {
            user_id: 7,
            username: "alice".to_owned()
        }
    );
}

#[test]
fn accepts_numeric_fractional_expiry_and_extra_claims() {
    let now = now();
    let payload = format!(
        r#"{{"sub":"12","username":"bob","exp":{}.5,"scope":"read"}}"#,
        now + 60
    );
    let identity = verify(&token(SECRET, &payload)).unwrap();
    assert_eq!(identity.user_id, 12);
    assert_eq!(identity.username, "bob");
}

#[test]
fn missing_and_foreign_authorization_headers_are_missing_bearer_tokens() {
    for header in [
        None,
        Some("Basic abc"),
        Some("Bearer"),
        Some("bearer abc"),
        Some("Bearer\t"),
        Some(""),
    ] {
        assert_eq!(
            verify_authorization(header, SECRET),
            Err(AppError::MissingBearerToken),
            "header {header:?}"
        );
    }
}

#[test]
fn malformed_tokens_are_invalid() {
    let valid = token(SECRET, &valid_payload());
    let signature = valid.rsplit('.').next().unwrap();
    let mut bad_signature = String::from(&valid[..valid.len() - signature.len()]);
    let first = if signature.starts_with('A') { 'B' } else { 'A' };
    bad_signature.push(first);
    bad_signature.push_str(&signature[1..]);

    let non_json_header = sign_token(SECRET, "not json", &valid_payload());
    let non_json_payload = sign_token(SECRET, r#"{"alg":"HS256","typ":"JWT"}"#, "not json");

    for token in [
        String::new(),
        "abc".to_owned(),
        "a.b".to_owned(),
        "a.b.c.d".to_owned(),
        "!!.!!.!!".to_owned(),
        format!("Bearer {valid}"),
        bad_signature,
        non_json_header,
        non_json_payload,
        "eyJhbGciOiJIUzI1NiJ9.e30.".to_owned(),
    ] {
        assert_eq!(verify(&token), Err(AppError::InvalidToken), "token {token:?}");
    }
}

#[test]
fn rejects_other_algorithms_even_with_a_valid_signature() {
    let payload = valid_payload();
    for header in [
        r#"{"alg":"none","typ":"JWT"}"#,
        r#"{"alg":"HS512","typ":"JWT"}"#,
        r#"{"alg":"RS256","typ":"JWT"}"#,
        r#"{"typ":"JWT"}"#,
        r#"{"alg":256}"#,
    ] {
        assert_eq!(
            verify(&sign_token(SECRET, header, &payload)),
            Err(AppError::InvalidToken),
            "header {header}"
        );
    }
}

#[test]
fn rejects_expired_and_missing_expiry() {
    let now = now();
    let expired = format!(r#"{{"sub":"7","username":"alice","exp":{}}}"#, now - 3600);
    let exact_now = format!(r#"{{"sub":"7","username":"alice","exp":{now}}}"#);
    let missing = r#"{"sub":"7","username":"alice"}"#.to_owned();
    let as_string = format!(r#"{{"sub":"7","username":"alice","exp":"{}"}}"#, now + 3600);
    let as_bool = r#"{"sub":"7","username":"alice","exp":true}"#.to_owned();

    for payload in [expired, exact_now, missing, as_string, as_bool] {
        assert_eq!(
            verify(&token(SECRET, &payload)),
            Err(AppError::InvalidToken),
            "payload {payload}"
        );
    }
}

#[test]
fn rejects_bad_payload_claims_only_after_a_valid_signature() {
    let now = now();
    let expiry = now + 3600;
    for payload in [
        format!(r#"{{"username":"alice","exp":{expiry}}}"#),
        format!(r#"{{"sub":"0","username":"alice","exp":{expiry}}}"#),
        format!(r#"{{"sub":"-1","username":"alice","exp":{expiry}}}"#),
        format!(r#"{{"sub":"abc","username":"alice","exp":{expiry}}}"#),
        format!(r#"{{"sub":"1.5","username":"alice","exp":{expiry}}}"#),
        format!(r#"{{"sub":" 1","username":"alice","exp":{expiry}}}"#),
        format!(r#"{{"sub":"99999999999999999999999","username":"alice","exp":{expiry}}}"#),
        format!(r#"{{"sub":7,"username":"alice","exp":{expiry}}}"#),
        format!(r#"{{"sub":"7","exp":{expiry}}}"#),
        format!(r#"{{"sub":"7","username":7,"exp":{expiry}}}"#),
        format!(r#"{{"sub":"7","username":null,"exp":{expiry}}}"#),
    ] {
        assert_eq!(
            verify(&token(SECRET, &payload)),
            Err(AppError::InvalidTokenPayload),
            "payload {payload}"
        );
    }
}

#[test]
fn rejects_a_valid_payload_signed_with_another_key() {
    let other = token(b"other-secret", &valid_payload());
    assert_eq!(verify(&other), Err(AppError::InvalidToken));
}

#[test]
fn parses_only_positive_integer_ids() {
    assert_eq!(parse_post_id("1"), Ok(1));
    assert_eq!(parse_post_id("500000"), Ok(500000));
    assert_eq!(parse_post_id("007"), Ok(7));

    for raw in [
        "",
        "0",
        "-1",
        "+1",
        "abc",
        "1.5",
        " 1",
        "1 ",
        "1e3",
        "0x10",
        "١",
        "99999999999999999999999",
    ] {
        assert_eq!(parse_post_id(raw), Err(AppError::InvalidPostId), "id {raw:?}");
    }
}

#[test]
fn parses_and_trims_create_bodies() {
    assert_eq!(
        parse_create_body(br#"{"body":"  hello  "}"#).unwrap(),
        "hello"
    );
    assert_eq!(
        parse_create_body(br#"{"body":"\n\t spaced \r\n"}"#).unwrap(),
        "spaced"
    );

    let five_hundred = "é".repeat(500);
    let parsed =
        parse_create_body(format!(r#"{{"body":"  {five_hundred}  "}}"#).as_bytes()).unwrap();
    assert_eq!(parsed.chars().count(), 500);
    assert_eq!(parsed.len(), 1000);

    let five_hundred_one = "é".repeat(501);
    assert_eq!(
        parse_create_body(format!(r#"{{"body":"{five_hundred_one}"}}"#).as_bytes()),
        Err(AppError::BodyTooLong)
    );

    let emoji = "✓".repeat(500);
    assert_eq!(
        parse_create_body(format!(r#"{{"body":"{emoji}"}}"#).as_bytes())
            .unwrap()
            .chars()
            .count(),
        500
    );
}

#[test]
fn rejects_invalid_create_bodies() {
    for body in [
        br#"{"body":""}"#.as_slice(),
        br#"{"body":"   "}"#.as_slice(),
        br#"{"body":123}"#.as_slice(),
        br#"{"body":null}"#.as_slice(),
        br#"{"body":["x"]}"#.as_slice(),
        br#"{}"#.as_slice(),
        br#"[]"#.as_slice(),
        br#"null"#.as_slice(),
        br#""text""#.as_slice(),
    ] {
        assert_eq!(
            parse_create_body(body),
            Err(AppError::BodyRequired),
            "body {:?}",
            String::from_utf8_lossy(body)
        );
    }

    for body in [
        br#"{"body":"x""#.as_slice(),
        br#"{"body":}"#.as_slice(),
        br#"{"body":"x",}"#.as_slice(),
        b"".as_slice(),
        b"\xff\xfe".as_slice(),
    ] {
        assert_eq!(
            parse_create_body(body),
            Err(AppError::MalformedJsonBody),
            "body {:?}",
            String::from_utf8_lossy(body)
        );
    }
}

#[test]
fn serializes_posts_and_responses_compactly_in_spec_order() {
    let post = Post {
        id: 7,
        body: "hi".to_owned(),
        created_at: "2025-01-01T00:00:00.000Z".to_owned(),
        author: "alice".to_owned(),
        like_count: 2,
    };
    assert_eq!(
        serde_json::to_string(&PostResponse { post: post.clone() }).unwrap(),
        r#"{"post":{"id":7,"body":"hi","created_at":"2025-01-01T00:00:00.000Z","author":"alice","like_count":2}}"#
    );
    assert_eq!(
        serde_json::to_string(&FeedResponse {
            posts: vec![post.clone()]
        })
        .unwrap(),
        r#"{"posts":[{"id":7,"body":"hi","created_at":"2025-01-01T00:00:00.000Z","author":"alice","like_count":2}]}"#
    );
    assert_eq!(
        serde_json::to_string(&LikeResponse {
            liked: true,
            already_liked: false,
            post_id: 7
        })
        .unwrap(),
        r#"{"liked":true,"already_liked":false,"post_id":7}"#
    );
    assert_eq!(
        serde_json::to_string(&HealthOk::new(42)).unwrap(),
        r#"{"status":"ok","db":"ok","uptime_s":42}"#
    );
    assert_eq!(
        serde_json::to_string(&HealthDegraded::new("disk I/O error".to_owned())).unwrap(),
        r#"{"status":"degraded","db":"unreachable","error":"disk I/O error"}"#
    );
}

#[test]
fn maps_every_error_to_its_spec_status_and_message() {
    let cases = [
        (AppError::NotFound, 404, "not found"),
        (AppError::InvalidPostId, 400, "invalid post id"),
        (AppError::PostNotFound, 404, "post not found"),
        (AppError::MissingBearerToken, 401, "missing bearer token"),
        (AppError::InvalidToken, 401, "invalid or expired token"),
        (AppError::InvalidTokenPayload, 401, "invalid token payload"),
        (AppError::MalformedJsonBody, 400, "malformed JSON body"),
        (AppError::BodyRequired, 400, "body is required"),
        (
            AppError::BodyTooLong,
            400,
            "body must be at most 500 characters",
        ),
        (AppError::Internal, 500, "internal server error"),
    ];

    for (error, status, message) in cases {
        assert_eq!(error.status_code(), status);
        assert_eq!(error.message(), message);
        assert_eq!(
            serde_json::to_string(&ErrorResponse::from(error)).unwrap(),
            format!(r#"{{"error":"{message}"}}"#)
        );
    }
}

#[test]
fn like_response_status_depends_on_already_liked() {
    assert_eq!(
        LikeResponse {
            liked: true,
            already_liked: false,
            post_id: 1
        }
        .status_code(),
        201
    );
    assert_eq!(
        LikeResponse {
            liked: true,
            already_liked: true,
            post_id: 1
        }
        .status_code(),
        200
    );
}
