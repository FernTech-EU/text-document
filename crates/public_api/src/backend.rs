// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 Cyril Jacquet

//! A backend several documents can share.
//!
//! Every [`TextDocument`](crate::TextDocument) built by [`TextDocument::new`](crate::TextDocument::new)
//! owns a whole application context: a store, an undo manager, an event hub, and
//! an OS thread draining that hub. That is right for one document and wrong for
//! a hundred. A host that opens a document per scene of a manuscript pays a
//! hundred threads to display one book, and each thread reserves eight megabytes
//! of address space for a loop that is idle almost all of the time.
//!
//! # What can be shared, and what cannot
//!
//! Not the store, and not the undo stack. Every repository's `snapshot` and
//! `restore` take and put back the **whole** store (see
//! `Transaction::snapshot_store`), so two documents sharing one would undo and
//! roll each other back. Each document keeps its own.
//!
//! The event hub can be shared, and that is where the thread is. One hub means
//! one drain, so a backend holds one [`EventHubClient`] and one thread however
//! many documents are built in it.
//!
//! # Telling one document's events from another's
//!
//! With a shared hub, every document's long-operation subscription sees every
//! document's long-operation events. Each document therefore records the ids of
//! the operations it started and ignores an event carrying any other id. The
//! filter lives in the document, inside the lock the callback already takes, so
//! there is no second structure to keep in step and no second lock to order
//! against the first.
//!
//! # Lifetime
//!
//! The pump stops when the backend drops, not when a document does: a document
//! that shut the hub down on its own way out would stop delivery for every
//! sibling still open. Hold the backend for as long as any document built in it.

use std::sync::Arc;

use frontend::AppContext;
use frontend::event_hub_client::EventHubClient;

/// A shared document backend: one event hub, one pump thread, one
/// long-operation manager, for any number of documents.
///
/// Cheap to clone (an `Arc`), and every clone names the same backend. Build one
/// per project, or per whatever scope wants its documents to share a thread, and
/// create documents in it with
/// [`TextDocument::new_in`](crate::TextDocument::new_in).
#[derive(Clone)]
pub struct DocumentBackend {
    inner: Arc<BackendInner>,
}

struct BackendInner {
    /// The context whose hub, shutdown channel and long-operation manager every
    /// document in this backend shares. Its own store and undo stack are unused:
    /// each document brings its own, because undo works on a whole store.
    ctx: AppContext,
    /// The one drain, and the one thread. Documents subscribe here.
    client: EventHubClient,
}

impl DocumentBackend {
    /// Build a backend, starting its single event pump.
    pub fn new() -> Self {
        let ctx = AppContext::new();
        let client = EventHubClient::new(&ctx.event_hub);
        client.start(ctx.shutdown_rx.clone());
        Self {
            inner: Arc::new(BackendInner { ctx, client }),
        }
    }

    /// The context documents built here share.
    pub(crate) fn shared_ctx(&self) -> &AppContext {
        &self.inner.ctx
    }

    /// The client documents built here subscribe on.
    pub(crate) fn client(&self) -> &EventHubClient {
        &self.inner.client
    }
}

impl Default for DocumentBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for DocumentBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentBackend")
            .field(
                "documents_sharing_it",
                &(Arc::strong_count(&self.inner) - 1),
            )
            .finish()
    }
}

impl Drop for BackendInner {
    /// Stop the pump. This is the only place that may: a document doing it on
    /// its own way out would stop delivery for every sibling still open.
    fn drop(&mut self) {
        self.ctx.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocumentEvent, TextDocument};
    use frontend::common::event::{LongOperationEvent, Origin};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    /// How long a step that takes microseconds is given before the test calls the
    /// backend stuck. A deadlock fails the test the same way however long this is,
    /// so it is generous enough that a loaded CI runner cannot fail it by being slow.
    const LIMIT: Duration = Duration::from_secs(10);

    /// A document whose last handle is dropped while the pump is inside one of its
    /// callbacks is destroyed on the pump thread, and the backend has to survive it.
    ///
    /// A long-operation callback upgrades the document's weak reference for as long
    /// as it runs. When the host drops every handle of its own meanwhile, which is
    /// what closing a tab or ending a test does right after an import returned, that
    /// upgrade is the last reference, and the document's subscription tokens drop on
    /// the pump. The pump used to hold the subscriber map across every callback, so
    /// those tokens waited on the pump itself, and every later subscribe or
    /// unsubscribe in the backend waited on them: building or dropping any sibling
    /// document hung the host thread for good.
    ///
    /// The interleaving is forced rather than hoped for. The doomed document's
    /// `Completed` callback takes its undo manager after upgrading, so holding that
    /// lock parks the pump inside the callback with the reference in hand for as long
    /// as the test needs to drop its own.
    #[test]
    fn a_document_destroyed_on_the_pump_leaves_the_backend_usable() {
        let backend = DocumentBackend::new();

        // Registered before any document, so on a `Completed` event the pump calls
        // it first: once it has spoken, every `Progress` event is behind the pump.
        let (completing_tx, completing_rx) = mpsc::channel();
        let probe = backend.client().subscribe(
            Origin::LongOperation(LongOperationEvent::Completed),
            move |_| {
                let _ = completing_tx.send(());
            },
        );

        let doomed = TextDocument::new_in(&backend);
        // Outlives `doomed`. Dropping it afterwards is what used to hang the host.
        let sibling = TextDocument::new_in(&backend);

        // Taken before the import starts, so the pump cannot get past it first. The
        // import itself never touches the undo manager, only the callback does.
        let history = Arc::clone(&doomed.inner.lock().ctx.undo_redo_manager);
        let held = history.lock();
        let operation = doomed
            .set_djot("Gone by the time its import is announced.")
            .expect("start the import");

        let reached_callback = completing_rx.recv_timeout(LIMIT).is_ok() && {
            // The pump's upgrade is the second reference, beside the test's own.
            let deadline = Instant::now() + LIMIT;
            while Arc::strong_count(&doomed.inner) < 2 && Instant::now() < deadline {
                thread::yield_now();
            }
            Arc::strong_count(&doomed.inner) >= 2
        };
        assert!(
            reached_callback,
            "the import's completion did not reach the document's callback within \
             {LIMIT:?}, so the interleaving this test forces never happened"
        );

        // The pump now holds the only reference. Releasing the undo manager lets the
        // callback finish and destroy the document on the pump thread.
        drop(doomed);
        drop(held);
        drop(operation);

        // Everything the host does next goes through the subscriber map, so it runs
        // on a thread of its own: a backend that deadlocked fails this test instead
        // of hanging the test binary.
        let (outcome_tx, outcome_rx) = mpsc::channel();
        let host_backend = backend.clone();
        thread::spawn(move || {
            drop(sibling);
            let fresh = TextDocument::new_in(&host_backend);
            let imported = fresh
                .set_djot("Still delivered.")
                .ok()
                .and_then(|op| op.wait_timeout(LIMIT))
                .is_some_and(|result| result.is_ok());
            // `wait` only proves the worker ran. The finished event proves the pump
            // is still delivering, since it is what bridges that event in.
            let deadline = Instant::now() + LIMIT;
            let mut delivered = false;
            while imported && !delivered && Instant::now() < deadline {
                delivered = fresh
                    .poll_events()
                    .iter()
                    .any(|e| matches!(e, DocumentEvent::LongOperationFinished { .. }));
                if !delivered {
                    thread::sleep(Duration::from_millis(5));
                }
            }
            let _ = outcome_tx.send((imported, delivered));
        });

        let outcome = outcome_rx.recv_timeout(LIMIT * 3);
        if outcome.is_err() {
            // Dropping the probe would lock the map the stuck pump holds, and the
            // test would hang in its own unwinding instead of failing.
            std::mem::forget(probe);
        }
        let (imported, delivered) = outcome.unwrap_or_else(|_| {
            panic!(
                "dropping a sibling document and building a new one in the backend did \
                 not return within {:?} after a document was destroyed on the pump: the \
                 pump is waiting on the subscriber map it holds itself",
                LIMIT * 3
            )
        });
        assert!(imported, "the new document's import did not complete");
        assert!(
            delivered,
            "the new document's import finished but its event never arrived: the pump \
             stopped delivering after destroying a document"
        );
    }
}
