use std::io;
use std::path::PathBuf;
use thiserror::Error;

/// Errors surfaced by Aster's user-facing application services.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("failed to {operation} '{}': {source}", path.display())]
    FileOperation {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl AppError {
    /// Adds the attempted operation and path to an I/O failure.
    pub fn file_operation(
        operation: &'static str,
        path: impl Into<PathBuf>,
        source: io::Error,
    ) -> Self {
        Self::FileOperation {
            operation,
            path: path.into(),
            source,
        }
    }
}

pub type AppResult<T> = Result<T, AppError>;
