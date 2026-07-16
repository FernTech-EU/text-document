//! Typed long operation handle.

use std::thread;
use std::time::Duration;

use crate::Result;

use frontend::AppContext;

/// Function that polls the long-operation manager for a result.
type ResultFn<T> = Box<dyn Fn(&AppContext, &str) -> Option<Result<T>> + Send>;

/// Shared state for a single long operation.
pub(crate) struct OperationState {
    ctx: AppContext,
}

impl OperationState {
    pub fn new(ctx: &AppContext) -> Self {
        Self { ctx: ctx.clone() }
    }
}

/// A handle to a running long operation (Markdown/HTML import, DOCX export).
///
/// Provides typed access to progress, cancellation, and the result.
/// Progress events are also emitted via [`DocumentEvent::LongOperationProgress`](crate::DocumentEvent::LongOperationProgress)
/// and [`DocumentEvent::LongOperationFinished`](crate::DocumentEvent::LongOperationFinished)
/// for the callback/polling path.
///
/// Retrieve the result via [`wait()`](Self::wait) (blocking, consumes the handle)
/// or [`try_result()`](Self::try_result) (non-blocking, can be called repeatedly).
pub struct Operation<T> {
    id: String,
    state: OperationState,
    result_fn: ResultFn<T>,
}

impl<T> Operation<T> {
    pub(crate) fn new(id: String, ctx: &AppContext, result_fn: ResultFn<T>) -> Self {
        Self {
            id,
            state: OperationState::new(ctx),
            result_fn,
        }
    }

    /// The operation ID (for matching with [`DocumentEvent`](crate::DocumentEvent) variants).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Get the current progress, if available.
    /// Returns `(percent, message)` where percent is 0.0–100.0.
    pub fn progress(&self) -> Option<(f64, String)> {
        let mgr = self.state.ctx.long_operation_manager.lock();
        mgr.get_operation_progress(&self.id)
            .map(|p| (p.percentage as f64, p.message.unwrap_or_default()))
    }

    /// Returns `true` if the operation has finished (success or failure).
    pub fn is_done(&self) -> bool {
        (self.result_fn)(&self.state.ctx, &self.id).is_some()
    }

    /// Cancel the operation. No-op if already finished.
    pub fn cancel(&self) {
        self.state
            .ctx
            .long_operation_manager
            .lock()
            .cancel_operation(&self.id);
    }

    /// Block the calling thread until the operation completes and return
    /// the typed result. Consumes the handle.
    pub fn wait(self) -> Result<T> {
        loop {
            if let Some(result) = (self.result_fn)(&self.state.ctx, &self.id) {
                return result;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Block until the operation completes or the timeout expires.
    /// Returns `None` if the timeout elapsed before the operation finished.
    pub fn wait_timeout(self, timeout: Duration) -> Option<Result<T>> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(result) = (self.result_fn)(&self.state.ctx, &self.id) {
                return Some(result);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            thread::sleep(remaining.min(Duration::from_millis(50)));
        }
    }

    /// Non-blocking: returns the result if the operation has completed,
    /// `None` if still running. Can be called repeatedly.
    pub fn try_result(&mut self) -> Option<Result<T>> {
        (self.result_fn)(&self.state.ctx, &self.id)
    }
}

// ── Result types ────────────────────────────────────────────────

/// Result of a Markdown import (`set_markdown`).
#[derive(Debug, Clone)]
pub struct MarkdownImportResult {
    pub block_count: usize,
}

/// Result of an HTML import (`set_html`).
#[derive(Debug, Clone)]
pub struct HtmlImportResult {
    pub block_count: usize,
}

/// Result of a djot import (`set_djot`).
#[derive(Debug, Clone)]
pub struct DjotImportResult {
    pub block_count: usize,
}

/// Result of a DOCX export (`to_docx`).
#[derive(Debug, Clone)]
pub struct DocxExportResult {
    pub file_path: String,
    pub paragraph_count: usize,
}

/// Result of an EPUB export (`to_epub`).
#[derive(Debug, Clone)]
pub struct EpubExportResult {
    pub file_path: String,
    pub chapter_count: usize,
}

/// Result of a PDF export (`to_pdf`).
#[derive(Debug, Clone)]
pub struct PdfExportResult {
    pub file_path: String,
    pub page_count: usize,
}
