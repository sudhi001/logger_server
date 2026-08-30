//! Application error type and its HTTP mapping.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

#[derive(Debug)]
pub enum AppError {
    /// Ingest queue is full — shed load rather than growing memory.
    Overloaded,
    Unauthorized,
    RateLimited,
    BadRequest(String),
    Internal(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overloaded => write!(f, "server overloaded"),
            Self::Unauthorized => write!(f, "unauthorized"),
            Self::RateLimited => write!(f, "rate limit exceeded"),
            Self::BadRequest(m) => write!(f, "{m}"),
            Self::Internal(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Overloaded => StatusCode::SERVICE_UNAVAILABLE,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };

        // Internal detail is logged, never echoed to the caller.
        let public = match &self {
            Self::Internal(detail) => {
                tracing::error!(error = %detail, "internal error");
                "internal error".to_string()
            }
            other => other.to_string(),
        };

        let body = serde_json::json!({ "error": public }).to_string();
        let mut resp = (status, body).into_response();
        resp.headers_mut()
            .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        if matches!(self, Self::Overloaded | Self::RateLimited) {
            resp.headers_mut()
                .insert(header::RETRY_AFTER, "1".parse().unwrap());
        }
        resp
    }
}
