use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use deser_value::Value;
use sha2::Sha256;

use crate::AppError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub user_id: i64,
    pub username: String,
}

pub fn verify_authorization(header: Option<&str>, secret: &[u8]) -> Result<Identity, AppError> {
    let token = header
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(AppError::MissingBearerToken)?;
    verify_token(token, secret)
}

fn verify_token(token: &str, secret: &[u8]) -> Result<Identity, AppError> {
    let mut parts = token.split('.');
    let header = parts.next().ok_or(AppError::InvalidToken)?;
    let payload = parts.next().ok_or(AppError::InvalidToken)?;
    let signature = parts.next().ok_or(AppError::InvalidToken)?;
    if parts.next().is_some() {
        return Err(AppError::InvalidToken);
    }

    let header_bytes = URL_SAFE_NO_PAD
        .decode(header)
        .map_err(|_| AppError::InvalidToken)?;
    let header_value: Value =
        deser_json::from_slice(&header_bytes).map_err(|_| AppError::InvalidToken)?;
    if header_value.get("alg").and_then(|value| value.as_str()) != Some("HS256") {
        return Err(AppError::InvalidToken);
    }

    let signature_bytes = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| AppError::InvalidToken)?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret).map_err(|_| AppError::InvalidToken)?;
    mac.update(header.as_bytes());
    mac.update(b".");
    mac.update(payload.as_bytes());
    mac.verify_slice(&signature_bytes)
        .map_err(|_| AppError::InvalidToken)?;

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| AppError::InvalidToken)?;
    let payload_value: Value =
        deser_json::from_slice(&payload_bytes).map_err(|_| AppError::InvalidToken)?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppError::Internal)?
        .as_secs_f64();
    let expires_at = payload_value
        .get("exp")
        .and_then(|value| value.as_f64())
        .ok_or(AppError::InvalidToken)?;
    if expires_at <= now {
        return Err(AppError::InvalidToken);
    }

    let sub = payload_value
        .get("sub")
        .and_then(|value| value.as_str())
        .ok_or(AppError::InvalidTokenPayload)?;
    if sub.is_empty() || !sub.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AppError::InvalidTokenPayload);
    }
    let user_id: i64 = sub.parse().map_err(|_| AppError::InvalidTokenPayload)?;
    if user_id <= 0 {
        return Err(AppError::InvalidTokenPayload);
    }
    let username = payload_value
        .get("username")
        .and_then(|value| value.as_str())
        .ok_or(AppError::InvalidTokenPayload)?
        .to_owned();

    Ok(Identity { user_id, username })
}
