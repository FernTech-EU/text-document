// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2026 FernTech

//! The store's side of transient edits (`RopeStore::begin_transient_write`): the recorded
//! content held while they are outstanding, given back by a revert, and the write
//! transactions refused over them.

use common::database::db_context::DbContext;
use common::database::rope_store::{TransientEditsOutstanding, TransientWriteRefused};
use common::database::transactions::Transaction;

fn text(db: &DbContext) -> String {
    db.get_store().rope.read().to_string()
}

fn write_text(db: &DbContext, text: &str) {
    *db.get_store().rope.write() = ropey::Rope::from_str(text);
}

fn is_refusal(error: &anyhow::Error) -> bool {
    error.downcast_ref::<TransientEditsOutstanding>().is_some()
}

#[test]
fn a_write_transaction_is_refused_over_outstanding_transient_edits() {
    let db = DbContext::new().unwrap();
    let store = db.get_store();
    store.begin_transient_write().unwrap().keep();
    assert!(store.has_transient_edits());

    let refused = Transaction::begin_write_transaction(&db).err().unwrap();
    assert!(is_refusal(&refused));
    // Reads are not refused: a view reads the transient text.
    assert!(Transaction::begin_read_transaction(&db).is_ok());

    assert!(store.revert_transient_edits().unwrap());
    assert!(Transaction::begin_write_transaction(&db).is_ok());
}

#[test]
fn the_transient_write_itself_may_open_transactions() {
    let db = DbContext::new().unwrap();
    let write = db.get_store().begin_transient_write().unwrap();
    let mut transaction = Transaction::begin_write_transaction(&db).unwrap();
    transaction.commit().unwrap();
    write.keep();
}

#[test]
fn another_thread_is_refused_while_a_transient_write_runs() {
    let db = DbContext::new().unwrap();
    let write = db.get_store().begin_transient_write().unwrap();
    let elsewhere = db.clone();
    let refused = std::thread::spawn(move || {
        Transaction::begin_write_transaction(&elsewhere)
            .err()
            .is_some_and(|error| is_refusal(&error))
    })
    .join()
    .unwrap();
    assert!(refused);
    write.keep();
}

#[test]
fn a_revert_gives_back_the_content_the_first_transient_write_found() {
    let db = DbContext::new().unwrap();
    write_text(&db, "abc");
    let store = db.get_store();

    let write = store.begin_transient_write().unwrap();
    assert!(write.opens());
    write_text(&db, "abcn");
    write.keep();

    let write = store.begin_transient_write().unwrap();
    assert!(!write.opens());
    write_text(&db, "abcnihao");
    write.keep();

    assert!(store.revert_transient_edits().unwrap());
    assert_eq!(text(&db), "abc");
    assert!(!store.has_transient_edits());
    assert!(!store.revert_transient_edits().unwrap());
}

#[test]
fn a_transient_write_not_kept_is_undone() {
    let db = DbContext::new().unwrap();
    write_text(&db, "abc");
    let store = db.get_store();

    // The first one: undone, and nothing is outstanding.
    {
        let _write = store.begin_transient_write().unwrap();
        write_text(&db, "abcX");
    }
    assert_eq!(text(&db), "abc");
    assert!(!store.has_transient_edits());

    // A later one: undone, and the earlier ones stay outstanding.
    let write = store.begin_transient_write().unwrap();
    write_text(&db, "abcn");
    write.keep();
    {
        let _write = store.begin_transient_write().unwrap();
        write_text(&db, "abcnX");
    }
    assert_eq!(text(&db), "abcn");
    assert!(store.has_transient_edits());
    assert!(store.revert_transient_edits().unwrap());
    assert_eq!(text(&db), "abc");
}

#[test]
fn a_revert_keeps_the_host_footnote_markers() {
    let db = DbContext::new().unwrap();
    let store = db.get_store();
    store.begin_transient_write().unwrap().keep();
    store
        .footnote_markers
        .write()
        .insert("note".into(), "7".into());

    store.revert_transient_edits().unwrap();
    assert_eq!(
        store
            .footnote_markers
            .read()
            .get("note")
            .map(String::as_str),
        Some("7")
    );
}

// ── What the store refuses ──────────────────────────────────────

#[test]
fn a_revert_inside_a_transient_write_is_refused() {
    let db = DbContext::new().unwrap();
    write_text(&db, "abc");
    let store = db.get_store();
    let write = store.begin_transient_write().unwrap();
    write_text(&db, "abcX");
    assert_eq!(
        store.revert_transient_edits(),
        Err(TransientWriteRefused::TransientWriteInProgress)
    );
    assert_eq!(text(&db), "abcX", "nothing was reverted");
    write.keep();
    assert_eq!(store.revert_transient_edits(), Ok(true));
    assert_eq!(text(&db), "abc");
}

#[test]
fn a_revert_over_an_open_write_is_refused() {
    let db = DbContext::new().unwrap();
    write_text(&db, "abc");
    let store = db.get_store();
    let write = store.begin_transient_write().unwrap();
    write_text(&db, "abcX");
    // A write the transient write began, still open once it is kept.
    let transaction = Transaction::begin_write_transaction(&db).unwrap();
    write.keep();
    assert_eq!(
        store.revert_transient_edits(),
        Err(TransientWriteRefused::WriteInProgress)
    );
    assert_eq!(text(&db), "abcX", "nothing was reverted");
    drop(transaction);
    assert_eq!(store.revert_transient_edits(), Ok(true));
    assert_eq!(text(&db), "abc");
}

/// A transient write begun while another thread reverts finds the recorded content,
/// never the transient content the revert is putting back.
#[test]
fn nothing_begins_while_a_revert_puts_the_content_back() {
    for _ in 0..200 {
        let db = DbContext::new().unwrap();
        write_text(&db, "abc");
        let write = db.get_store().begin_transient_write().unwrap();
        write_text(&db, "abcX");
        write.keep();
        let elsewhere = db.clone();
        // Begins the next layer as soon as the store says the first one has ended.
        let other = std::thread::spawn(move || {
            loop {
                let store = elsewhere.get_store();
                if !store.has_transient_edits()
                    && let Ok(write) = store.begin_transient_write()
                {
                    let found = text(&elsewhere);
                    drop(write);
                    return found;
                }
            }
        });
        db.get_store().revert_transient_edits().unwrap();
        let found = other.join().unwrap();
        assert_eq!(found, "abc");
        assert_eq!(text(&db), "abc");
    }
}

#[test]
fn asking_begins_no_write() {
    let db = DbContext::new().unwrap();
    let elsewhere = db.clone();
    std::thread::spawn(move || assert!(elsewhere.get_store().is_write_allowed()))
        .join()
        .unwrap();
    assert!(db.get_store().begin_transient_write().is_ok());

    // A check begins a write: until that thread takes the write's savepoint, a transient
    // write on another thread waits for it.
    let elsewhere = db.clone();
    let (checked, check) = std::sync::mpsc::channel();
    let (go_on, go) = std::sync::mpsc::channel::<()>();
    let writer = std::thread::spawn(move || {
        elsewhere.get_store().check_write_allowed().unwrap();
        checked.send(()).unwrap();
        go.recv().unwrap();
        let savepoint = elsewhere.get_store().create_savepoint();
        elsewhere.get_store().discard_savepoint(savepoint);
    });
    check.recv().unwrap();
    assert_eq!(
        db.get_store().begin_transient_write().err(),
        Some(TransientWriteRefused::WriteInProgress)
    );
    go_on.send(()).unwrap();
    writer.join().unwrap();
    assert!(db.get_store().begin_transient_write().is_ok());
}

#[test]
fn a_check_with_no_write_after_it_holds_nothing_up() {
    let db = DbContext::new().unwrap();
    let store = db.get_store();
    store.check_write_allowed().unwrap();
    assert!(store.begin_transient_write().is_ok());
}

#[test]
fn transient_writes_do_not_nest() {
    let db = DbContext::new().unwrap();
    let store = db.get_store();
    let outer = store.begin_transient_write().unwrap();
    assert_eq!(
        store.begin_transient_write().err(),
        Some(TransientWriteRefused::TransientWriteInProgress)
    );
    // Nor on another thread.
    let elsewhere = db.clone();
    let refused = std::thread::spawn(move || elsewhere.get_store().begin_transient_write().err())
        .join()
        .unwrap();
    assert_eq!(
        refused,
        Some(TransientWriteRefused::TransientWriteInProgress)
    );
    drop(outer);
    assert!(store.begin_transient_write().is_ok());
}

/// A transient write refused while a write transaction is open, whichever way the transaction
/// then ends, and allowed again once it has.
#[test]
fn a_transient_write_waits_for_every_open_write() {
    let db = DbContext::new().unwrap();
    write_text(&db, "abc");
    let store = db.get_store();
    type End = fn(Transaction);
    let ends: [(&str, End); 3] = [
        ("commit", |mut t| t.commit().unwrap()),
        ("rollback", |mut t| t.rollback().unwrap()),
        ("drop", drop),
    ];
    for (end, finish) in ends {
        let transaction = Transaction::begin_write_transaction(&db).unwrap();
        assert_eq!(
            store.begin_transient_write().err(),
            Some(TransientWriteRefused::WriteInProgress),
            "{end}"
        );
        assert!(!store.has_transient_edits(), "{end}: no base was taken");
        finish(transaction);
        assert!(store.begin_transient_write().is_ok(), "{end}");
    }
    // A savepoint a transaction makes for itself is not a write of its own.
    let transaction = Transaction::begin_write_transaction(&db).unwrap();
    let savepoint = transaction.create_savepoint().unwrap();
    drop(transaction);
    assert!(store.begin_transient_write().is_ok());
    store.discard_savepoint(savepoint);
}

/// A write on another thread and the first transient write never both go ahead over each
/// other: one of them is refused, and the revert never takes the recorded write away.
#[test]
fn a_write_on_another_thread_never_lands_inside_the_base() {
    for _ in 0..200 {
        let db = DbContext::new().unwrap();
        write_text(&db, "abc");
        let elsewhere = db.clone();
        let writer =
            std::thread::spawn(
                move || match Transaction::begin_write_transaction(&elsewhere) {
                    Ok(mut transaction) => {
                        write_text(&elsewhere, "recorded");
                        transaction.commit().unwrap();
                        true
                    }
                    Err(error) => {
                        assert!(is_refusal(&error));
                        false
                    }
                },
            );
        let transient = db.get_store().begin_transient_write();
        let wrote = match transient {
            Ok(write) => {
                write_text(&db, "transient");
                write.keep();
                let wrote = writer.join().unwrap();
                db.get_store().revert_transient_edits().unwrap();
                wrote
            }
            Err(refused) => {
                assert_eq!(refused, TransientWriteRefused::WriteInProgress);
                writer.join().unwrap()
            }
        };
        // Whatever the timing, the recorded write is never taken away by the revert.
        assert_eq!(text(&db), if wrote { "recorded" } else { "abc" });
    }
}
