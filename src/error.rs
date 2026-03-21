//! Error types for the wsproxy library.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::{fmt, io};

/// Result type alias using the library's Error type.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur in the wsproxy library.
pub struct Error {
    kind: ErrorKind,
    backtrace: Backtrace,
}

#[derive(Debug)]
enum ErrorKind {
    /// I/O error.
    Io(io::Error),
    /// WebSocket error.
    WebSocket(tokio_tungstenite::tungstenite::Error),
    /// No route found for the requested path.
    NoRouteFound(String),
    /// Configuration error.
    Config(String),
}

impl Error {
    /// Create a new NoRouteFound error.
    pub fn no_route_found(path: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::NoRouteFound(path.into()),
            backtrace: Backtrace::capture(),
        }
    }

    /// Create a new Config error.
    pub fn config(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Config(message.into()),
            backtrace: Backtrace::capture(),
        }
    }

    /// Returns the backtrace associated with this error.
    pub fn backtrace(&self) -> &Backtrace {
        &self.backtrace
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // anyhow-style formatting
        writeln!(f, "{self}")?;

        // Print error chain
        let mut source = std::error::Error::source(self);
        if source.is_some() {
            writeln!(f)?;
            writeln!(f, "Caused by:")?;
            let mut i = 0;
            while let Some(err) = source {
                writeln!(f, "    {i}: {err}")?;
                source = err.source();
                i += 1;
            }
        }

        // Print backtrace if captured
        if self.backtrace.status() == BacktraceStatus::Captured {
            writeln!(f)?;
            writeln!(f, "Stack backtrace:")?;
            write!(f, "{}", self.backtrace)?;
        }

        Ok(())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ErrorKind::Io(e) => write!(f, "I/O error: {e}"),
            ErrorKind::WebSocket(e) => write!(f, "WebSocket error: {e}"),
            ErrorKind::NoRouteFound(path) => write!(f, "No route found for path: {path}"),
            ErrorKind::Config(msg) => write!(f, "Configuration error: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            ErrorKind::Io(e) => Some(e),
            ErrorKind::WebSocket(e) => Some(e),
            ErrorKind::NoRouteFound(_) | ErrorKind::Config(_) => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Self {
            kind: ErrorKind::Io(err),
            backtrace: Backtrace::capture(),
        }
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for Error {
    fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
        Self {
            kind: ErrorKind::WebSocket(err),
            backtrace: Backtrace::capture(),
        }
    }
}
