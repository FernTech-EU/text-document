//! `BatchDocument` — the headless, thread-free document a backend batch uses.
//!
//! The point of this type is that it is *cheap enough to make hundreds of*. So these
//! tests check two things: that it round-trips Djot faithfully (a replace that mangles
//! markup is worse than no replace at all), and that it genuinely costs no thread —
//! because if it did, a project-wide rename would spawn one per touched scene.

use text_document::{BatchDocument, DjotExportOptions, DjotImportOptions, FindOptions};

fn round_trip(djot: &str) -> String {
    let batch = BatchDocument::new().expect("BatchDocument::new");
    batch
        .set_djot(djot, &DjotImportOptions::default())
        .expect("set_djot");
    batch
        .to_djot(&DjotExportOptions::default())
        .expect("to_djot")
}

/// Prose survives the trip. This is the load-bearing property: a replace parses the
/// Djot, rewrites text, and serialises it back — so anything the round trip damages,
/// a replace would damage across the whole manuscript.
#[test]
fn a_batch_document_round_trips_djot() {
    for source in [
        "Just a plain paragraph.",
        "A paragraph with *emphasis* and _more_ of it.",
        "# A heading\n\nAnd a paragraph beneath it.",
        "One paragraph.\n\nAnd a second one.",
    ] {
        let out = round_trip(source);
        assert_eq!(
            out.trim(),
            source.trim(),
            "round-tripping this Djot changed it:\n  in:  {source:?}\n  out: {out:?}"
        );
    }
}

/// A **tight** list comes back **loose** — the one place the round trip is not an
/// identity. Pinned rather than wished away, because a `BatchDocument` is what a
/// project-wide replace runs through, and this is the shape of the only reformatting
/// it can cause.
///
/// This is a documented limitation of the model, not a bug in this type: the entity
/// model has no tight/loose distinction, and the exporter's blank line between blocks
/// is what lets an indented sub-list *nest* instead of folding into its parent item's
/// paragraph (see `export_djot_uc`'s own comment). Removing it would trade this for a
/// worse bug.
///
/// It is also **not new to replace**: the editor round-trips a scene's Djot through a
/// document on every flush, so opening and editing a scene already loosens its tight
/// lists. Replace adds no damage that saving does not already do. Fixing it properly
/// means teaching the entity model about tight lists — a separate job.
#[test]
fn a_tight_list_comes_back_loose_and_that_is_known() {
    let out = round_trip("- a list item\n- another one");
    assert_eq!(
        out.trim(),
        "- a list item\n\n- another one",
        "if this now round-trips tightly, the model learned tight/loose — delete this \
         test and fold the case back into `a_batch_document_round_trips_djot`"
    );
}

/// The whole reason this type exists. `TextDocument` starts an `EventHubClient` thread
/// per document; `BatchDocument` must not — otherwise a rename touching 120 scenes
/// spawns 120 threads.
///
/// Counted from the process's own thread count rather than mocked, so it fails if a
/// future refactor quietly reintroduces the spawn. Platform-specific: `/proc` on Linux,
/// `task_threads` on macOS, Toolhelp32 on Windows (CI runs all three).
#[test]
fn a_batch_document_costs_no_thread() {
    let before = process_thread_count();
    let docs: Vec<BatchDocument> = (0..32)
        .map(|i| {
            let b = BatchDocument::new().expect("BatchDocument::new");
            b.set_djot(
                &format!("Scene {i}: she called his name into the trees."),
                &DjotImportOptions::default(),
            )
            .expect("set_djot");
            b
        })
        .collect();
    let after = process_thread_count();

    assert_eq!(docs.len(), 32);
    assert!(
        after <= before + 2,
        "32 BatchDocuments started {} extra thread(s) ({before} -> {after}). \
         BatchDocument exists precisely so a batch does not pay a thread per document; \
         something reintroduced EventHubClient::start.",
        after.saturating_sub(before)
    );
}

/// Live thread count of this process. Used only by `a_batch_document_costs_no_thread`.
fn process_thread_count() -> usize {
    #[cfg(target_os = "linux")]
    {
        // Linux exposes the live thread count of the process directly.
        std::fs::read_to_string("/proc/self/status")
            .expect("/proc/self/status")
            .lines()
            .find_map(|l| l.strip_prefix("Threads:"))
            .and_then(|v| v.trim().parse().ok())
            .expect("Threads: line")
    }

    #[cfg(target_os = "macos")]
    {
        // `task_threads` lists every live thread of the current Mach task.
        type MachPort = u32;
        type KernReturn = i32;
        type MachMsgTypeNumber = u32;

        // Edition 2024: FFI declarations live in `unsafe extern`.
        unsafe extern "C" {
            static mut mach_task_self_: MachPort;
            fn task_threads(
                target_task: MachPort,
                act_list: *mut *mut MachPort,
                act_list_cnt: *mut MachMsgTypeNumber,
            ) -> KernReturn;
            fn vm_deallocate(target_task: MachPort, address: usize, size: usize) -> KernReturn;
            fn mach_port_deallocate(task: MachPort, name: MachPort) -> KernReturn;
        }

        // SAFETY: task_threads fills thread_list; we free each port and the array.
        unsafe {
            let task = mach_task_self_;
            let mut thread_list: *mut MachPort = std::ptr::null_mut();
            let mut thread_count: MachMsgTypeNumber = 0;
            let kr = task_threads(task, &mut thread_list, &mut thread_count);
            assert_eq!(kr, 0, "task_threads failed with kern_return {kr}");
            let count = thread_count as usize;
            for i in 0..count {
                let _ = mach_port_deallocate(task, *thread_list.add(i));
            }
            let _ = vm_deallocate(
                task,
                thread_list as usize,
                count * std::mem::size_of::<MachPort>(),
            );
            count
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Toolhelp32 snapshot of every thread, filtered to this process.
        #[repr(C)]
        struct ThreadEntry32 {
            dw_size: u32,
            cnt_usage: u32,
            th32_thread_id: u32,
            th32_owner_process_id: u32,
            tp_base_pri: i32,
            tp_delta_pri: i32,
            dw_flags: u32,
        }

        const TH32CS_SNAPTHREAD: u32 = 0x0000_0004;
        const INVALID_HANDLE_VALUE: isize = -1;

        // Edition 2024: FFI declarations live in `unsafe extern`.
        unsafe extern "system" {
            fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> isize;
            fn Thread32First(snapshot: isize, entry: *mut ThreadEntry32) -> i32;
            fn Thread32Next(snapshot: isize, entry: *mut ThreadEntry32) -> i32;
            fn CloseHandle(handle: isize) -> i32;
            fn GetCurrentProcessId() -> u32;
        }

        // SAFETY: Win32 Toolhelp32 snapshot walk; snapshot is closed before return.
        unsafe {
            let pid = GetCurrentProcessId();
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            assert!(
                snap != 0 && snap != INVALID_HANDLE_VALUE,
                "CreateToolhelp32Snapshot failed"
            );
            let mut entry = std::mem::zeroed::<ThreadEntry32>();
            entry.dw_size = std::mem::size_of::<ThreadEntry32>() as u32;
            let mut count = 0usize;
            if Thread32First(snap, &mut entry) != 0 {
                loop {
                    if entry.th32_owner_process_id == pid {
                        count += 1;
                    }
                    if Thread32Next(snap, &mut entry) == 0 {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);
            count
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        compile_error!(
            "process_thread_count is only implemented for linux, macos, and windows (CI targets)"
        );
    }
}

/// Search runs against the *parsed text*, not the Djot source — so markup never matches
/// and never shifts an offset. `*Aurélien*` must be found at the position of the name,
/// not the position of the asterisk.
#[test]
fn find_all_searches_the_prose_not_the_markup() {
    let batch = BatchDocument::new().expect("BatchDocument::new");
    batch
        .set_djot(
            "She called *Aurélien* into the trees, and Aurélien did not answer.",
            &DjotImportOptions::default(),
        )
        .expect("set_djot");

    let hits = batch
        .find_all("Aurélien", &FindOptions::default())
        .expect("find_all");
    assert_eq!(
        hits.len(),
        2,
        "both occurrences, including the emphasised one"
    );

    // The first hit must sit where the NAME is in the prose ("She called " = 11 chars),
    // not where it is in the source (which the `*` would push along by one).
    assert_eq!(
        hits[0].position, 11,
        "the offset must be into the parsed text; a Djot-source offset would be 12 \
         (the emphasis marker) and would corrupt any replace built on it"
    );
    assert_eq!(hits[0].length, "Aurélien".chars().count());
}

/// Reusing one document across several imports must not accumulate the old contents —
/// a batch loop refills the same document for every scene it touches.
#[test]
fn set_djot_replaces_rather_than_appends() {
    let batch = BatchDocument::new().expect("BatchDocument::new");
    batch
        .set_djot("the first scene", &DjotImportOptions::default())
        .expect("first");
    batch
        .set_djot("the second scene", &DjotImportOptions::default())
        .expect("second");

    let out = batch
        .to_djot(&DjotExportOptions::default())
        .expect("to_djot");
    assert!(out.contains("second"), "the newest import must be present");
    assert!(
        !out.contains("first"),
        "the previous import must be gone, not appended: {out:?}"
    );
}
