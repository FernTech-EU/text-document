// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 Cyril Jacquet

//! A backend several documents can share.
//!
//! Every [`TextDocument`](crate::TextDocument) built by [`TextDocument::new`]
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
