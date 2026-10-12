use axum::Json;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use serde_json::{Value, json};
use uuid::Uuid;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    Validation(String),
    #[error("authentication required")]
    Unauthorized,
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("too many requests")]
    TooManyRequests { retry_after: i64 },
    #[error("service unavailable: {0}")]
    Unavailable(String),
    #[error("internal server error")]
    Internal(#[from] anyhow::Error),
}

#[derive(Debug, Serialize)]
pub struct ProblemJson {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    pub trace_id: String,
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }

    pub fn not_found(what: impl Into<String>) -> Self {
        Self::NotFound(format!("{} not found", what.into()))
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(msg.into())
    }

    pub fn internal_msg(err: impl std::fmt::Display) -> Self {
        tracing::error!(error = %err, "internal error");
        Self::Internal(anyhow::anyhow!("{err}"))
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::Validation(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::TooManyRequests { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "validation_error",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden(_) => "forbidden",
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::TooManyRequests { .. } => "rate_limited",
            Self::Unavailable(_) => "unavailable",
            Self::Internal(_) => "internal_error",
        }
    }

    fn trace_id(&self) -> String {
        Uuid::new_v4().simple().to_string()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let Self::Internal(err) = &self {
            tracing::error!(error = ?err, "request failed");
        }
        let status = self.status();
        let retry_after = matches!(self, Self::TooManyRequests { .. });
        let body = ProblemJson {
            code: self.code(),
            message: self.to_string(),
            details: None,
            trace_id: self.trace_id(),
        };
        let mut response = (status, Json(body)).into_response();
        if retry_after {
            if let Ok(v) = header::HeaderValue::from_str("60") {
                response.headers_mut().insert(header::RETRY_AFTER, v);
            }
        }
        response
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        match &err {
            sqlx::Error::RowNotFound => Self::NotFound("resource".into()),
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                Self::Conflict("resource already exists".into())
            }
            _ => Self::internal_msg(err),
        }
    }
}

impl From<std::io::Error> for ApiError {
    fn from(err: std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound("file".into()),
            _ => Self::internal_msg(err),
        }
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(err: reqwest::Error) -> Self {
        Self::Unavailable(format!("upstream request failed: {err}"))
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(err: serde_json::Error) -> Self {
        Self::internal_msg(err)
    }
}

impl Serialize for ApiError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("ApiError", 2)?;
        s.serialize_field("code", self.code())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

#[allow(dead_code)]
pub fn problem_body(code: &'static str, message: impl Into<String>) -> Value {
    json!({ "code": code, "message": message.into() })
}
