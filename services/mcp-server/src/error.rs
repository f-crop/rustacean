use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug)]
pub enum AppError {
    InvalidInput,
    NotFound,
    Unauthorized,
    ServiceUnavailable,
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal(err)
    }
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::RowNotFound => Self::NotFound,
            _ => Self::Internal(err.into()),
        }
    }
}

impl From<rb_query::QueryError> for AppError {
    fn from(_err: rb_query::QueryError) -> Self {
        Self::ServiceUnavailable
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::InvalidInput => (StatusCode::BAD_REQUEST, "invalid input"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found"),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::ServiceUnavailable => (StatusCode::SERVICE_UNAVAILABLE, "service unavailable"),
            Self::Internal(e) => {
                tracing::error!(error = %e, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
            }
        };

        (status, message).into_response()
    }
}
