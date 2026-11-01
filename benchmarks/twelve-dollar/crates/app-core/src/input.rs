use crate::AppError;

const MAX_BODY_CHARS: usize = 500;

pub fn parse_post_id(raw: &str) -> Result<i64, AppError> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AppError::InvalidPostId);
    }
    let id: i64 = raw.parse().map_err(|_| AppError::InvalidPostId)?;
    if id <= 0 {
        return Err(AppError::InvalidPostId);
    }
    Ok(id)
}

pub fn parse_create_body(bytes: &[u8]) -> Result<String, AppError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| AppError::MalformedJsonBody)?;
    let body = value
        .get("body")
        .and_then(serde_json::Value::as_str)
        .ok_or(AppError::BodyRequired)?;
    Ok(validate_body(body)?.to_owned())
}

pub(crate) fn validate_body(body: &str) -> Result<&str, AppError> {
    let body = body.trim();
    if body.is_empty() {
        return Err(AppError::BodyRequired);
    }
    if body.chars().count() > MAX_BODY_CHARS {
        return Err(AppError::BodyTooLong);
    }
    Ok(body)
}
