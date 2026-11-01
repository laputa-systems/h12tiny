use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppError {
    NotFound,
    InvalidPostId,
    PostNotFound,
    MissingBearerToken,
    InvalidToken,
    InvalidTokenPayload,
    MalformedJsonBody,
    BodyRequired,
    BodyTooLong,
    Internal,
}

impl AppError {
    pub fn status_code(self) -> u16 {
        match self {
            Self::NotFound | Self::PostNotFound => 404,
            Self::InvalidPostId | Self::MalformedJsonBody | Self::BodyRequired | Self::BodyTooLong => 400,
            Self::MissingBearerToken | Self::InvalidToken | Self::InvalidTokenPayload => 401,
            Self::Internal => 500,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::NotFound => "not found",
            Self::InvalidPostId => "invalid post id",
            Self::PostNotFound => "post not found",
            Self::MissingBearerToken => "missing bearer token",
            Self::InvalidToken => "invalid or expired token",
            Self::InvalidTokenPayload => "invalid token payload",
            Self::MalformedJsonBody => "malformed JSON body",
            Self::BodyRequired => "body is required",
            Self::BodyTooLong => "body must be at most 500 characters",
            Self::Internal => "internal server error",
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for AppError {}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

impl ErrorResponse {
    pub fn from_error(error: AppError) -> Self {
        Self {
            error: error.message().to_owned(),
        }
    }
}

impl From<AppError> for ErrorResponse {
    fn from(error: AppError) -> Self {
        Self::from_error(error)
    }
}
