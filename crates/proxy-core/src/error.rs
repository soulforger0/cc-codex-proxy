use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("not authenticated: {0}")]
    NotAuthenticated(String),

    #[error("upstream returned {status}: {body}")]
    Upstream { status: StatusCode, body: String, retry_after: Option<String> },

    #[error("upstream transport failed: {0}")]
    Transport(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl ProxyError {
    pub fn status(&self) -> StatusCode {
        match self {
            ProxyError::InvalidRequest(_) | ProxyError::Config(_) => StatusCode::BAD_REQUEST,
            ProxyError::NotAuthenticated(_) => StatusCode::UNAUTHORIZED,
            ProxyError::Upstream { status, .. } => *status,
            ProxyError::Transport(_) => StatusCode::BAD_GATEWAY,
            ProxyError::Io(_) | ProxyError::Json(_) | ProxyError::Reqwest(_) | ProxyError::Other(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            ProxyError::InvalidRequest(_) | ProxyError::Config(_) => "invalid_request_error",
            ProxyError::NotAuthenticated(_) => "authentication_error",
            ProxyError::Upstream { .. } => "upstream_error",
            ProxyError::Transport(_) => "transport_error",
            ProxyError::Io(_) | ProxyError::Json(_) | ProxyError::Reqwest(_) | ProxyError::Other(_) => {
                "internal_error"
            }
        }
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status(),
            Json(json!({
                "type": "error",
                "error": {
                    "type": self.type_name(),
                    "message": self.to_string(),
                }
            })),
        )
            .into_response();

        if let ProxyError::Upstream { retry_after: Some(value), .. } = &self {
            if let Ok(header_value) = value.parse() {
                response.headers_mut().insert(axum::http::header::RETRY_AFTER, header_value);
            }
        }

        response
    }
}

pub type Result<T> = std::result::Result<T, ProxyError>;

