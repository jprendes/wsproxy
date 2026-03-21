//! Error types for the wsproxy library.

use thiserror::Error;

/// Result type alias using the library's Error type.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur in the wsproxy library.
#[derive(Debug, Error)]
pub enum Error {
    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// WebSocket error.
    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    /// No route found for the requested path.
    #[error("No route found for path: {0}")]
    NoRouteFound(String),

    /// Configuration error.
    #[error("Configuration error: {0}")]
    Config(String),
}
