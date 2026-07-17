//! Typed long operation handle.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::Result;

use common::long_operation::{OperationCompletion, OperationStatus};
use frontend::AppContext;

/// Backstop for a completion signal that never arrives.
///
/// This is **not** a polling interval: a wait is woken by the manager's condvar
/// the instant its operation publishes completion, so in normal operation this
/// timeout is never reached. It exists only so that a signal lost to an
/// abnormally-terminated worker — one killed between storing its result and
/// publishing — degrades into a slow re-check instead of a permanent hang.
const COMPLETION_BACKSTOP: Duration = Duration::from_secs(1);

/// Function that reads the long-operation manager for a result.
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
    ///
    /// Reads the completion signal, not just the result slot: a cancelled or
    /// failed operation stores no result but is very much finished.
    pub fn is_done(&self) -> bool {
        self.completion().is_finished(&self.id)
    }

    /// This operation's completion signal, with the manager lock **released**.
    ///
    /// Never block while holding `long_operation_manager`: a wait lasts as long
    /// as the operation, and every other operation query would queue behind it.
    fn completion(&self) -> Arc<OperationCompletion> {
        self.state
            .ctx
            .long_operation_manager
            .lock()
            .completion_signal()
    }

    /// The error for an operation that finished without producing a result —
    /// which means it was cancelled, or it failed.
    fn terminal_error(&self) -> crate::DocumentError {
        let status = self
            .state
            .ctx
            .long_operation_manager
            .lock()
            .get_operation_status(&self.id);
        match status {
            Some(OperationStatus::Failed(err)) => anyhow::anyhow!("{err}").into(),
            Some(OperationStatus::Cancelled) => anyhow::anyhow!("operation was cancelled").into(),
            // Finished, no result, no live handle: it was cleaned up out from
            // under us, so the outcome is no longer knowable.
            _ => anyhow::anyhow!("operation finished without producing a result").into(),
        }
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
    ///
    /// The wait is **signal-driven**: the manager wakes it the moment the
    /// operation publishes completion, so a short operation costs only its own
    /// runtime. It previously re-checked on a 50 ms timer, which put a ~50 ms
    /// floor under *every* call however trivial the work — invisible for one
    /// import, but seconds of dead time for a caller loading documents in a loop.
    ///
    /// A cancelled or failed operation now returns `Err` rather than blocking
    /// forever: neither ever stores a result, so the old "loop until a result
    /// appears" never terminated for them.
    pub fn wait(self) -> Result<T> {
        let completion = self.completion();
        loop {
            // Read completion *before* polling. The worker stores its result
            // before publishing, so if the operation had already finished at this
            // point, the read below is authoritative — a miss then means no
            // result was ever produced, not that one has yet to land.
            let finished = completion.is_finished(&self.id);
            if let Some(result) = (self.result_fn)(&self.state.ctx, &self.id) {
                return result;
            }
            if finished {
                return Err(self.terminal_error());
            }
            completion.wait_for(&self.id, Some(COMPLETION_BACKSTOP));
        }
    }

    /// Block until the operation completes or the timeout expires.
    /// Returns `None` if the timeout elapsed before the operation finished.
    ///
    /// Like [`wait`](Self::wait), this blocks on the completion signal rather
    /// than a timer, so it returns as soon as the operation ends instead of on
    /// the next 50 ms boundary.
    pub fn wait_timeout(self, timeout: Duration) -> Option<Result<T>> {
        let completion = self.completion();
        let deadline = Instant::now() + timeout;
        loop {
            // Same ordering rule as `wait`: completion first, then the result.
            let finished = completion.is_finished(&self.id);
            if let Some(result) = (self.result_fn)(&self.state.ctx, &self.id) {
                return Some(result);
            }
            if finished {
                return Some(Err(self.terminal_error()));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            // Wait out the real remaining time in one blocking call — capped by
            // the backstop so a lost signal still re-checks — rather than in
            // 50 ms slices.
            completion.wait_for(&self.id, Some(remaining.min(COMPLETION_BACKSTOP)));
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
