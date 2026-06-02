//! The public error type for the `text-document` API.
//!
//! Every fallible public function returns [`Result<T>`], i.e.
//! `Result<T, DocumentError>`. Unlike the previous opaque
//! `anyhow::Result`, callers can now match on [`DocumentError`] to react
//! to specific failure categories (a cursor used outside a table, a
//! lookup that found nothing, an out-of-range index, …).
//!
//! Errors originating deep inside the backend crates arrive as
//! [`DocumentError::Internal`] via the `From<anyhow::Error>` bridge, so
//! propagation with `?` continues to work unchanged.

use thiserror::Error;

/// Errors returned by the public `text-document` API.
///
/// The `Display` text is the original human-readable message; the variant
/// carries the machine-matchable category. Marked `#[non_exhaustive]` so
/// new categories can be added without breaking callers — match with a
/// `_` arm.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DocumentError {
    /// A referenced entity (block, cell, document, …) does not exist.
    #[error("{0}")]
    NotFound(String),

    /// An operation was attempted with the cursor in the wrong structural
    /// context (e.g. a table operation while not inside a table).
    #[error("{0}")]
    InvalidCursorContext(String),

    /// An index or position was outside the valid range.
    #[error("{0}")]
    OutOfRange(String),

    /// The arguments were individually valid but invalid in combination
    /// (e.g. a selection spanning multiple frames, mismatched tables).
    #[error("{0}")]
    InvalidArgument(String),

    /// Any other error propagated from the backend layers.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

/// Result alias used throughout the public API.
pub type Result<T> = std::result::Result<T, DocumentError>;
