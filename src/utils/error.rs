use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum AppError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Image processing error: {0}")]
    ImageProcessing(String),
    
    #[error("Cache error: {0}")]
    Cache(String),
    
    #[error("Configuration error: {0}")]
    Config(String),
    
    #[error("Meme not found: {id}")]
    MemeNotFound { id: u32 },
    
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
    
    #[error("Not found: {0}")]
    NotFound(String),
    
    #[error("{0}")]
    Internal(String),
    
    #[error("Bad request: {0}")]
    BadRequest(String),
    
    #[error("File system error: {0}")]
    FileSystem(#[from] notify::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_message) = match self {
            AppError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"),
            AppError::ImageProcessing(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Image processing error"),
            AppError::Cache(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Cache error"),
            AppError::Config(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Configuration error"),
            AppError::MemeNotFound { .. } => (StatusCode::NOT_FOUND, "Meme not found"),
            AppError::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "Invalid request"),
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, "Not found"),
            AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "Bad request"),
            AppError::FileSystem(_) => (StatusCode::INTERNAL_SERVER_ERROR, "File system error"),
        };

        let body = Json(json!({
            "error": error_message,
            "message": self.to_string()
        }));

        (status, body).into_response()
    }
}

pub type Result<T> = std::result::Result<T, AppError>;