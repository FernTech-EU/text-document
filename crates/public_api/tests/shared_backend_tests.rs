// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 Cyril Jacquet

//! Documents that share one backend: one thread between them, and no other
//! sharing at all.
//!
//! `TextDocument::new` owns a whole application context, including an OS thread
//! draining its event hub. A host that opens one document per scene of a
//! manuscript therefore starts one thread per scene. `DocumentBackend` shares
//! the hub, the pump and the long-operation manager, and nothing else: the store
//! and the undo stack stay private, because undo snapshots and restores a whole
//! store and two documents sharing one would roll each other back.

use text_document::{DocumentBackend, TextDocument};

/// Every test in this file, one at a time.
///
/// `a_backend_costs_one_thread_where_standalone_documents_cost_one_each` reads a
/// **process-global** number (`/proc/self/status`), and cargo runs the tests of one
/// binary in parallel threads of one process. Its three siblings each build a
/// `DocumentBackend` — a thread apiece — so without this they are inside its
/// measurement window, and `shared_cost <= 1` fails for a reason that has nothing to
/// do with the property it names. Measuring both halves inside one test is necessary
/// but not sufficient: what has to stop is the *siblings* running alongside it.
///
/// A poisoned lock is not interesting here (it only means another test in this file
/// already failed), so every taker recovers from it rather than cascading.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Live thread count of this process.
#[cfg(target_os = "linux")]
fn process_thread_count() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .expect("/proc/self/status")
        .lines()
        .find_map(|l| l.strip_prefix("Threads:"))
        .and_then(|v| v.trim().parse().ok())
        .expect("Threads: line")
}

/// The reason the backend exists, measured against the thing it replaces.
///
/// A manuscript stream over a book-length project opens a document per scene; at
/// a thread each, that is a hundred threads to show one book, and each reserves
/// megabytes of address space for a loop that is idle almost all of the time.
///
/// Both halves are measured inside one test on one thread. Cargo runs the tests
/// in a binary in parallel, and several of them build documents, so two separate
/// tests comparing absolute thread counts would be measuring each other.
#[cfg(target_os = "linux")]
#[test]
fn a_backend_costs_one_thread_where_standalone_documents_cost_one_each() {
    const N: usize = 16;
    let _serial = serial();

    // Standalone: one pump each.
    let before_standalone = process_thread_count();
    let standalone: Vec<TextDocument> = (0..N).map(|_| TextDocument::new()).collect();
    let standalone_cost = process_thread_count().saturating_sub(before_standalone);

    // Shared: one pump between them, started with the backend.
    let backend = DocumentBackend::new();
    let before_shared = process_thread_count();
    let shared: Vec<TextDocument> = (0..N).map(|_| TextDocument::new_in(&backend)).collect();
    for (i, doc) in shared.iter().enumerate() {
        doc.set_plain_text(&format!("Scene {i}: she called his name into the trees."))
            .expect("set_plain_text");
    }
    let shared_cost = process_thread_count().saturating_sub(before_shared);

    assert_eq!(standalone.len(), N);
    assert_eq!(shared.len(), N);
    assert!(
        shared_cost <= 1,
        "{N} documents in one backend started {shared_cost} thread(s). The backend \
         exists precisely so a document does not pay a thread of its own; something \
         reintroduced EventHubClient::start per document."
    );
    assert!(
        standalone_cost > shared_cost,
        "the comparison is the point: {N} standalone documents cost {standalone_cost} \
         thread(s) and {N} shared ones cost {shared_cost}"
    );
}

/// Sharing a backend must share nothing that belongs to a document. Two
/// documents in one backend hold separate text.
#[test]
fn documents_in_one_backend_keep_separate_text() {
    let _serial = serial();
    let backend = DocumentBackend::new();
    let a = TextDocument::new_in(&backend);
    let b = TextDocument::new_in(&backend);

    a.set_plain_text("The ferry left before the light did.")
        .expect("set a");
    b.set_plain_text("She counted the bells and lost count twice.")
        .expect("set b");

    assert_eq!(
        a.to_plain_text().expect("a"),
        "The ferry left before the light did."
    );
    assert_eq!(
        b.to_plain_text().expect("b"),
        "She counted the bells and lost count twice."
    );
}

/// The one that decides whether the store may be shared at all: undo snapshots
/// and restores a WHOLE store, so a shared store would let one document's undo
/// revert another's edit. Each document keeping its own is what makes this pass.
#[test]
fn undo_in_one_document_leaves_its_sibling_alone() {
    let _serial = serial();
    let backend = DocumentBackend::new();
    let a = TextDocument::new_in(&backend);
    let b = TextDocument::new_in(&backend);

    a.set_plain_text("First.").expect("set a");
    b.set_plain_text("Second.").expect("set b");

    let b_before = b.to_plain_text().expect("b before");
    a.undo().expect("undo a");

    assert_eq!(
        b.to_plain_text().expect("b after"),
        b_before,
        "undoing in one document must not touch a sibling sharing the backend"
    );
}

/// A document dropped out of a backend must not take the pump with it. The
/// survivor still works, which it could not if the shared hub had been shut
/// down by the first drop.
#[test]
fn dropping_one_document_leaves_the_backend_running() {
    let _serial = serial();
    let backend = DocumentBackend::new();
    let doomed = TextDocument::new_in(&backend);
    let survivor = TextDocument::new_in(&backend);
    doomed.set_plain_text("Gone in a moment.").expect("set");
    drop(doomed);

    survivor
        .set_plain_text("Still here, and still edited.")
        .expect("the survivor must still take an edit");
    assert_eq!(
        survivor.to_plain_text().expect("survivor"),
        "Still here, and still edited."
    );
}

/// A document says which of the two ways it was built.
///
/// The host-facing half of the sharing: a document meant to live as long as a
/// project belongs in the backend, and one built standalone by mistake behaves
/// identically while carrying a thread of its own. This is what lets a host assert
/// its own wiring without counting threads, which is a process-global number no
/// test can own.
#[test]
fn a_document_reports_whether_it_shares_a_backend() {
    let backend = DocumentBackend::new();
    let shared = TextDocument::new_in(&backend);
    let alone = TextDocument::new();

    assert!(shared.shares_a_backend());
    assert!(!alone.shares_a_backend());
}
