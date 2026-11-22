use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;


/// The one message shown when a payment option is unavailable because its
/// backend (barkd, the Ark server, bitcoind or the price feed) is not
/// healthy. Details stay in the logs.
pub const UNAVAILABLE_MESSAGE: &str = "This payment method is temporarily unavailable. Try again later.";

/// An API error. Internal and upstream errors are logged in full and shown
/// to the caller with a generic message.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
	#[error("{0}")]
	BadRequest(String),
	#[error("the token is missing, not valid, or revoked")]
	Unauthorized,
	#[error("{0}")]
	NotFound(String),
	#[error("{0}")]
	Conflict(String),
	#[error("too many requests; try again later")]
	TooManyRequests,
	/// A backend failed or is unhealthy: the payment option is withdrawn,
	/// with one fixed message.
	#[error("{}", UNAVAILABLE_MESSAGE)]
	Upstream(anyhow::Error),
	/// The service or a backend is set up so that the request cannot
	/// work. The message says what to change.
	#[error("{0}")]
	Misconfigured(String),
	#[error("internal error")]
	Internal(#[from] anyhow::Error),
}

pub type ApiResult<T> = Result<T, ApiError>;

pub fn bad_request(msg: impl Into<String>) -> ApiError {
	ApiError::BadRequest(msg.into())
}

pub fn not_found(msg: impl Into<String>) -> ApiError {
	ApiError::NotFound(msg.into())
}

pub fn conflict(msg: impl Into<String>) -> ApiError {
	ApiError::Conflict(msg.into())
}

impl From<tokio_postgres::Error> for ApiError {
	fn from(e: tokio_postgres::Error) -> Self {
		ApiError::Internal(e.into())
	}
}

impl From<bb8::RunError<tokio_postgres::Error>> for ApiError {
	fn from(e: bb8::RunError<tokio_postgres::Error>) -> Self {
		ApiError::Internal(anyhow::anyhow!("database pool: {}", e))
	}
}

impl IntoResponse for ApiError {
	fn into_response(self) -> Response {
		let status = match &self {
			ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
			ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
			ApiError::NotFound(_) => StatusCode::NOT_FOUND,
			ApiError::Conflict(_) => StatusCode::CONFLICT,
			ApiError::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
			ApiError::Upstream(e) => {
				tracing::warn!("backend unavailable: {:#}", e);
				StatusCode::SERVICE_UNAVAILABLE
			},
			ApiError::Misconfigured(_) => StatusCode::SERVICE_UNAVAILABLE,
			ApiError::Internal(e) => {
				tracing::error!("internal error: {:#}", e);
				StatusCode::INTERNAL_SERVER_ERROR
			},
		};
		(status, Json(json!({ "error": self.to_string() }))).into_response()
	}
}
