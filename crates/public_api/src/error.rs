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
/// The `Display` text is the original human-readable message (for
/// [`TransientEdits`](DocumentError::TransientEdits), its reason's); the variant
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

    /// The requested operation is not available in this build — e.g. [`crate::TextDocument::to_pdf`]
    /// called against a `text-document` build that did not enable the `pdf` cargo feature.
    #[error("{0}")]
    Unsupported(String),

    /// Refused because of the document's [transient edits](crate::transient): the
    /// [`TransientRefusal`] says which case it is.
    #[error("{0}")]
    TransientEdits(TransientRefusal),

    /// Any other error propagated from the backend layers.
    #[error(transparent)]
    Internal(anyhow::Error),
}

/// Why the document's [transient edits](crate::transient) refused an operation
/// ([`DocumentError::TransientEdits`]). Marked `#[non_exhaustive]`: match with a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum TransientRefusal {
    /// A transient edit whose selection a removal would not take as it stands: one across a
    /// table's cells, or one holding part of a table and text outside it, which
    /// [`TextCursor::set_position`](crate::TextCursor::set_position) never leaves but
    /// another cursor's edit can
    /// ([`TextCursor::insert_text_transient`](crate::TextCursor::insert_text_transient) and
    /// its siblings). Nothing changes.
    #[error(
        "a transient edit must change one run of text: this selection crosses a table's \
         cells, or holds part of a table and text outside it"
    )]
    CrossesTableCells,
    /// A transient edit while a load in place
    /// ([`TextDocument::set_djot`](crate::TextDocument::set_djot) and its siblings) is still
    /// writing the document. Nothing changes.
    #[error("a load in place is writing the document: no transient edit is made until it ends")]
    LoadInPlace,
    /// A transient edit while another write to the document's store is open, which the
    /// store refuses so that the recorded content the transient edits are made over is
    /// whole. The one write of this document left open across its calls is a load in place,
    /// reported as [`LoadInPlace`](Self::LoadInPlace) before the store is asked; this is the
    /// store's own refusal of any other. Nothing changes.
    #[error("a write to the document is in progress: no transient edit is made until it ends")]
    WriteInProgress,
    /// A commit of transient edits made in more than one place, which would replace the
    /// recorded text between them as well
    /// ([`TextCursor::commit_transient_text`](crate::TextCursor::commit_transient_text)).
    /// The transient edits stay as they were.
    #[error(
        "the transient edits changed the document in more than one place: committing them \
         would replace the recorded text between them"
    )]
    CommitInSeveralPlaces,
    /// A [`TextCursor::replace`](crate::TextCursor::replace) whose range held only
    /// transient text: the revert it begins with took that text away, and nothing is
    /// replaced.
    #[error("the range held only transient text, which was reverted: nothing to replace")]
    RangeOfTransientTextOnly,
    /// A recorded write that found transient edits outstanding when it began, which the
    /// document's store refuses before anything is written. Every write this crate offers
    /// reverts them first, so they were made since, by another thread between that revert
    /// and the write. Nothing is recorded, and an open edit block keeps what it holds.
    #[error(
        "transient edits are outstanding: the document's store refuses a recorded write over them"
    )]
    LayerOutstanding,
}

impl From<anyhow::Error> for DocumentError {
    /// [`DocumentError::Internal`], except for the store's refusals anywhere in the error's
    /// chain: a write over outstanding transient edits is
    /// [`TransientRefusal::LayerOutstanding`], a transient write refused is
    /// [`TransientRefusal::WriteInProgress`].
    fn from(error: anyhow::Error) -> Self {
        use common::database::rope_store::{TransientEditsOutstanding, TransientWriteRefused};
        for cause in error.chain() {
            if cause.downcast_ref::<TransientEditsOutstanding>().is_some() {
                return DocumentError::TransientEdits(TransientRefusal::LayerOutstanding);
            }
            if let Some(refused) = cause.downcast_ref::<TransientWriteRefused>() {
                return DocumentError::TransientEdits((*refused).into());
            }
        }
        DocumentError::Internal(error)
    }
}

impl From<common::database::rope_store::TransientWriteRefused> for TransientRefusal {
    /// The store's refusal to begin a transient write. Both of its cases are another write
    /// in progress on the store: a transient one (which the document's lock keeps from
    /// happening) or a recorded one.
    fn from(_: common::database::rope_store::TransientWriteRefused) -> Self {
        TransientRefusal::WriteInProgress
    }
}

/// Result alias used throughout the public API.
pub type Result<T> = std::result::Result<T, DocumentError>;
