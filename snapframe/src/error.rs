/// The application's single error type. Its text is shown to the user as-is.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Msg(String),
    #[error("No recording is in progress")]
    NoRecording,
    #[error("This feature is only available on Windows")]
    #[allow(dead_code)]
    Unsupported,
    #[error("Input/output error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// Errors from WinRT APIs such as OCR.
    #[cfg(windows)]
    #[error("Windows error: {0}")]
    Windows(#[from] windows::core::Error),
}

impl From<String> for AppError {
    fn from(value: String) -> Self {
        AppError::Msg(value)
    }
}

impl From<&str> for AppError {
    fn from(value: &str) -> Self {
        AppError::Msg(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
