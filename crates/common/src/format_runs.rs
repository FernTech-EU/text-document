//! Per-block character formatting as sorted, non-overlapping byte spans.
//!
//! Each block carries a `Vec<FormatRun>` (formatting) and a
//! `Vec<ImageAnchor>` (image positions). The block's `plain_text` is
//! the authoritative character source for byte offsets used by both.
//! Replaces the pre-Phase-1 model where every formatted run and every
//! inline image was a row in the now-deleted `inline_elements` entity
//! table; the [`InlineSegment`] type in this module is a transient
//! view synthesized from `(plain_text, format_runs, block_images)`
//! for readers (export, fragments, cursor) that still consume a
//! per-segment shape.
//!
//! Invariants are documented on [`FormatRun`] and enforced by
//! [`debug_assert_well_formed`] and by [`splice_range`] / [`shift_after`]
//! which rebuild the run list while preserving them.
//!
//! **In a release build a `debug_assert!` is not enforcement.** It is compiled out, so
//! a contract violation there produced a malformed run list *silently* — and autosave
//! wrote that corruption to the writer's file seconds later. The bulk-edit path
//! therefore uses the checked siblings, which report instead of assert:
//! [`check_well_formed`], [`try_splice_range`] and [`shift_runs_for_replace`], all
//! returning [`FormatRunError`].

use crate::entities::{CharVerticalAlignment, UnderlineStyle};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A violation of the format-run invariants, reported instead of asserted.
///
/// The invariants used to be guarded only by `debug_assert!`, which is **compiled out
/// of release builds** — so a contract violation silently produced a malformed run list
/// in a shipped binary, and (with autosave) that corruption became the file's new truth
/// within seconds. Anything on the *replace* path returns this instead, so the use case
/// can refuse the edit and say why rather than quietly mangle a writer's formatting.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FormatRunError {
    #[error("byte range {start}..{end} is reversed")]
    ReversedRange { start: u32, end: u32 },

    #[error(
        "replacement run {run_start}..{run_end} falls outside the spliced range \
         {range_start}..{range_end}"
    )]
    ReplacementOutsideRange {
        run_start: u32,
        run_end: u32,
        range_start: u32,
        range_end: u32,
    },

    #[error("run {start}..{end} is empty or reversed")]
    EmptyRun { start: u32, end: u32 },

    #[error("runs overlap or are out of order at index {index}: {left:?} then {right:?}")]
    RunsOverlap {
        index: usize,
        left: Box<FormatRun>,
        right: Box<FormatRun>,
    },

    #[error("adjacent runs with identical formatting were left uncoalesced at index {index}")]
    RunsNotCoalesced { index: usize },

    #[error("run {start}..{end} runs past the end of the block's {text_len} bytes")]
    RunPastEndOfBlock {
        start: u32,
        end: u32,
        text_len: usize,
    },
}

/// What the replacement text wears when it overwrites formatted text.
///
/// Before this existed the behaviour was **emergent, not chosen**: a replace was a
/// delete followed by an insert, and `shift_runs_for_insert` hardcodes "the inserted
/// text inherits whatever run ends at the insertion point". That silently destroys
/// formatting — renaming a character whose name reads `Auré**lien**` dropped the bold
/// entirely, and not one test in the repo covered it.
///
/// The old behaviour is still the default (and is byte-for-byte identical); it is now
/// simply one option among four, and the caller has to look at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReplaceFormatPolicy {
    /// The replacement inherits the format of the run that straddles the start of the
    /// replaced range or ends exactly on it (precisely: `byte_start < start &&
    /// byte_end >= start`); otherwise it is unformatted. A run beginning *exactly* at
    /// the start never contributes.
    ///
    /// This is the Qt / ProseMirror insertion convention, and it is what
    /// delete-then-insert has always produced — byte for byte, which
    /// `inherit_preceding_matches_the_historical_delete_then_insert` pins *differentially*
    /// (it runs the historical primitives and compares, rather than trusting a
    /// transcription of them).
    #[default]
    InheritPreceding,

    /// If a **single run covers the whole replaced range**, the replacement keeps that
    /// run's format; otherwise fall back to [`Self::InheritPreceding`]. "Rename a name
    /// that was entirely bold and it stays bold" — without guessing when the range is
    /// formatted unevenly.
    PreserveIfFullyCovered,

    /// The replacement takes the format that covered the **most bytes** of the replaced
    /// range. Unformatted gaps count as a candidate, so a mostly-plain range stays
    /// plain; ties go to the *formatted* run, because a tie means the writer's
    /// formatting covered at least as much as its absence and dropping it is the
    /// destructive answer.
    KeepDominantRun,

    /// The replacement carries no formatting at all.
    PreserveNothing,
}

/// Content type for an inline segment: text, image, or empty.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub enum InlineContent {
    #[default]
    Empty,
    Text(String),
    Image {
        name: String,
        width: i64,
        height: i64,
        quality: i64,
    },
}

/// A lean view type representing one inline segment (text or image) with its
/// associated formatting. Used by readers (export, fragments, cursor) to
/// consume per-segment data synthesized from `(plain_text, format_runs,
/// block_images)` via [`crate::format_runs_query::inline_segments_for_block`].
/// Never stored — synthesized on demand.
///
/// The `fmt_*` field names match those on `Block` and on `FragmentElement`
/// so readers can copy fields verbatim across the three types.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct InlineSegment {
    pub content: InlineContent,
    pub fmt_font_family: Option<String>,
    pub fmt_font_point_size: Option<i64>,
    pub fmt_font_weight: Option<i64>,
    pub fmt_font_bold: Option<bool>,
    pub fmt_font_italic: Option<bool>,
    pub fmt_font_underline: Option<bool>,
    pub fmt_font_overline: Option<bool>,
    pub fmt_font_strikeout: Option<bool>,
    pub fmt_letter_spacing: Option<i64>,
    pub fmt_word_spacing: Option<i64>,
    pub fmt_anchor_href: Option<String>,
    pub fmt_anchor_names: Vec<String>,
    pub fmt_is_anchor: Option<bool>,
    pub fmt_tooltip: Option<String>,
    pub fmt_underline_style: Option<UnderlineStyle>,
    pub fmt_vertical_alignment: Option<CharVerticalAlignment>,
}

/// Character-level formatting for a contiguous byte span. One per
/// [`FormatRun`]; one per [`ImageAnchor`]. Fields mirror the `fmt_*`
/// set on [`InlineSegment`] and on `FragmentElement` so values copy
/// across types verbatim.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterFormat {
    pub font_family: Option<String>,
    pub font_point_size: Option<i64>,
    pub font_weight: Option<i64>,
    pub font_bold: Option<bool>,
    pub font_italic: Option<bool>,
    pub font_underline: Option<bool>,
    pub font_overline: Option<bool>,
    pub font_strikeout: Option<bool>,
    pub letter_spacing: Option<i64>,
    pub word_spacing: Option<i64>,
    pub anchor_href: Option<String>,
    pub anchor_names: Vec<String>,
    pub is_anchor: Option<bool>,
    pub tooltip: Option<String>,
    pub underline_style: Option<UnderlineStyle>,
    pub vertical_alignment: Option<CharVerticalAlignment>,
}

/// One run of identical character formatting inside a block. Byte offsets
/// are relative to the block's `plain_text` (Phase 1) or to the block's
/// rope range (Phase 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatRun {
    pub byte_start: u32,
    pub byte_end: u32,
    pub format: CharacterFormat,
}

/// An image embedded at a specific byte position inside a block. In
/// Phase 1 the byte position is an index into the block's `plain_text`;
/// in Phase 2 it points at the U+FFFC sentinel character in the rope.
///
/// Images carry their own [`CharacterFormat`] because vertical alignment
/// and anchor metadata apply per inline run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAnchor {
    pub byte_offset: u32,
    pub name: String,
    pub width: i64,
    pub height: i64,
    pub quality: i64,
    pub format: CharacterFormat,
}

/// Debug-only invariant check. Run from `debug_assert!` callsites in
/// the use cases that mutate format runs. Cheap: O(n) where n is the
/// run count (typically < 100 per block in real prose).
///
/// # Invariants
/// 1. Runs are sorted by `byte_start` ascending.
/// 2. Each run has `byte_start < byte_end`.
/// 3. Runs are non-overlapping: `runs[i].byte_end <= runs[i+1].byte_start`.
/// 4. The last run's `byte_end` does not exceed `block_text_len`.
/// 5. Adjacent runs with identical format are coalesced (no two
///    consecutive runs satisfy `byte_end == next.byte_start &&
///    format == next.format`).
pub fn debug_assert_well_formed(runs: &[FormatRun], block_text_len: usize) {
    // Only pay the O(n) walk where the assertion would actually fire.
    if cfg!(debug_assertions)
        && let Err(e) = check_well_formed(runs, block_text_len)
    {
        debug_assert!(false, "format runs are malformed: {e}");
    }
}

/// The same invariants as [`debug_assert_well_formed`], but **reported rather than
/// asserted** — so a caller that must not corrupt a block can refuse the edit.
///
/// This exists because `debug_assert!` is compiled out of release builds: a malformed
/// run list produced in a shipped binary went completely undetected, and autosave wrote
/// it to disk seconds later. The replace path calls this and propagates the error.
pub fn check_well_formed(runs: &[FormatRun], block_text_len: usize) -> Result<(), FormatRunError> {
    if runs.is_empty() {
        return Ok(());
    }
    for run in runs {
        if run.byte_start >= run.byte_end {
            return Err(FormatRunError::EmptyRun {
                start: run.byte_start,
                end: run.byte_end,
            });
        }
    }
    for i in 0..runs.len() - 1 {
        if runs[i].byte_end > runs[i + 1].byte_start {
            return Err(FormatRunError::RunsOverlap {
                index: i,
                left: Box::new(runs[i].clone()),
                right: Box::new(runs[i + 1].clone()),
            });
        }
        if runs[i].byte_end == runs[i + 1].byte_start && runs[i].format == runs[i + 1].format {
            return Err(FormatRunError::RunsNotCoalesced { index: i });
        }
    }
    let last = runs.last().expect("non-empty");
    if last.byte_end as usize > block_text_len {
        return Err(FormatRunError::RunPastEndOfBlock {
            start: last.byte_start,
            end: last.byte_end,
            text_len: block_text_len,
        });
    }
    Ok(())
}

/// Merge adjacent runs that have identical formatting. O(n).
pub fn coalesce_in_place(runs: &mut Vec<FormatRun>) {
    if runs.len() < 2 {
        return;
    }
    let mut write = 0usize;
    for read in 1..runs.len() {
        if runs[write].byte_end == runs[read].byte_start && runs[write].format == runs[read].format
        {
            runs[write].byte_end = runs[read].byte_end;
        } else {
            write += 1;
            if write != read {
                runs[write] = runs[read].clone();
            }
        }
    }
    runs.truncate(write + 1);
}

/// Replace the runs covering `range` with `replacement`, preserving the
/// invariants. Runs that straddle the range boundary are clipped on
/// either side; runs fully contained are removed.
///
/// The replacement byte ranges must lie within `range` and themselves
/// be well-formed (sorted, non-overlapping). The function does NOT
/// shift bytes after `range.end` — callers wanting to splice in a
/// different-length text must call [`shift_after`] first or after,
/// depending on whether the text length is changing.
pub fn splice_range(
    runs: &mut Vec<FormatRun>,
    range: std::ops::Range<u32>,
    replacement: Vec<FormatRun>,
) {
    if let Err(e) = try_splice_range(runs, range, replacement) {
        // Previously this was a bare `debug_assert!` — compiled OUT of release, where a
        // contract violation therefore went on to build a malformed run list and corrupt
        // the block's formatting silently. Keep the loud failure in debug; in release,
        // refuse the splice and leave `runs` untouched rather than mangle it.
        //
        // No existing caller can reach this: every one either builds a replacement that
        // spans exactly the range, or goes through `shift_runs_for_delete`, which
        // early-returns on an inverted range. It is a net, not a behaviour change.
        debug_assert!(false, "splice_range contract violated: {e}");
    }
}

/// [`splice_range`], but the contract is **checked and reported** instead of asserted.
///
/// `runs` is left completely untouched when this returns `Err` — validation happens
/// before any mutation, so a rejected splice cannot half-apply.
pub fn try_splice_range(
    runs: &mut Vec<FormatRun>,
    range: std::ops::Range<u32>,
    replacement: Vec<FormatRun>,
) -> Result<(), FormatRunError> {
    if range.start > range.end {
        return Err(FormatRunError::ReversedRange {
            start: range.start,
            end: range.end,
        });
    }
    for r in &replacement {
        if r.byte_start >= r.byte_end {
            return Err(FormatRunError::EmptyRun {
                start: r.byte_start,
                end: r.byte_end,
            });
        }
        if r.byte_start < range.start || r.byte_end > range.end {
            return Err(FormatRunError::ReplacementOutsideRange {
                run_start: r.byte_start,
                run_end: r.byte_end,
                range_start: range.start,
                range_end: range.end,
            });
        }
    }
    for i in 1..replacement.len() {
        if replacement[i - 1].byte_end > replacement[i].byte_start {
            return Err(FormatRunError::RunsOverlap {
                index: i - 1,
                left: Box::new(replacement[i - 1].clone()),
                right: Box::new(replacement[i].clone()),
            });
        }
    }

    let mut result: Vec<FormatRun> = Vec::with_capacity(runs.len() + replacement.len());

    // Keep / clip everything strictly before range.start.
    for run in runs.iter() {
        if run.byte_end <= range.start {
            result.push(run.clone());
        } else if run.byte_start < range.start {
            // Run straddles range.start: keep the left part.
            result.push(FormatRun {
                byte_start: run.byte_start,
                byte_end: range.start,
                format: run.format.clone(),
            });
        }
    }

    // Insert the replacement runs.
    result.extend(replacement);

    // Keep / clip everything starting at or after range.end.
    for run in runs.iter() {
        if run.byte_start >= range.end {
            result.push(run.clone());
        } else if run.byte_end > range.end {
            // Run straddles range.end: keep the right part.
            result.push(FormatRun {
                byte_start: range.end,
                byte_end: run.byte_end,
                format: run.format.clone(),
            });
        }
    }

    coalesce_in_place(&mut result);
    *runs = result;
    Ok(())
}

/// Capture the slice of `runs` that intersects `[start..end)`, clipped
/// to those bounds. Used by hand-rolled-inverse undo for format-only
/// edits: callers capture this BEFORE calling [`splice_range`], and
/// on undo splice the captured runs back into the same byte range to
/// restore the prior state without paying the cost of a full
/// `RopeStoreSnapshot`.
///
/// Gaps in the original runs (positions inside `[start..end)` with no
/// formatting) become gaps in the captured output too — the undo
/// splice preserves them faithfully.
pub fn capture_runs_in_range(runs: &[FormatRun], start: u32, end: u32) -> Vec<FormatRun> {
    let mut out = Vec::new();
    for run in runs {
        if run.byte_end <= start || run.byte_start >= end {
            continue;
        }
        let clipped_start = std::cmp::max(run.byte_start, start);
        let clipped_end = std::cmp::min(run.byte_end, end);
        if clipped_start < clipped_end {
            out.push(FormatRun {
                byte_start: clipped_start,
                byte_end: clipped_end,
                format: run.format.clone(),
            });
        }
    }
    out
}

/// Capture the `(byte_offset, format)` pairs for every image anchor
/// inside `[start..end)`. Used together with [`capture_runs_in_range`]
/// by hand-rolled-inverse undo for format-only edits.
pub fn capture_image_formats_in_range(
    images: &[ImageAnchor],
    start: u32,
    end: u32,
) -> Vec<(u32, CharacterFormat)> {
    let mut out = Vec::new();
    for img in images {
        if img.byte_offset >= start && img.byte_offset < end {
            out.push((img.byte_offset, img.format.clone()));
        }
    }
    out
}

/// Shift the byte offsets of every run whose `byte_start >= threshold`
/// by `delta`. Used after a text insert/delete to keep downstream runs
/// in sync with the new block text. Runs strictly before the threshold
/// are unaffected; runs that straddle the threshold are left alone
/// (the caller should have spliced them first).
///
/// Panics in debug mode if `delta` would underflow a run's offset.
pub fn shift_after(runs: &mut [FormatRun], threshold: u32, delta: i32) {
    for run in runs.iter_mut() {
        if run.byte_start >= threshold {
            let new_start = (run.byte_start as i64) + (delta as i64);
            let new_end = (run.byte_end as i64) + (delta as i64);
            debug_assert!(new_start >= 0 && new_end >= new_start);
            run.byte_start = new_start as u32;
            run.byte_end = new_end as u32;
        }
    }
}

/// Synthesize a stable per-fragment id from a block id and byte offset
/// within that block. Populates the `element_id` field in
/// `FragmentContent::{Text, Image}` (the public layout-engine type),
/// giving callers a stable handle across renders even though the
/// underlying [`InlineSegment`]s are never stored. Two segments at the
/// same `(block_id, byte_start)` always produce the same id; a segment
/// that moves to a new byte_start (e.g. due to an insert upstream)
/// gets a new id.
///
/// Bit layout (u64): bit 62 = synth tag (so synthesized ids never
/// collide with real entity ids issued by the store's counter, which
/// start at 1 and grow upward). Bits 32..62 = block id (1 billion
/// blocks per document, 30 bits). Bottom 32 bits = byte offset (4 GB
/// per block). The top bit stays zero so the value fits in positive
/// i64 range — public DTOs expose element_id as i64.
pub fn synth_element_id(block_id: u64, byte_start: u32) -> u64 {
    const SYNTH_TAG: u64 = 0x4000_0000_0000_0000;
    SYNTH_TAG | ((block_id & 0x3FFF_FFFF) << 32) | (byte_start as u64)
}

/// Same as `shift_after` for image anchors. Anchors AT the threshold are
/// shifted (treated as part of the inserted region's right side).
pub fn shift_images_after(images: &mut [ImageAnchor], threshold: u32, delta: i32) {
    for img in images.iter_mut() {
        if img.byte_offset >= threshold {
            let new_off = (img.byte_offset as i64) + (delta as i64);
            debug_assert!(new_off >= 0);
            img.byte_offset = new_off as u32;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Composite helpers used by writer use cases. These keep the per-block
// run / image vectors well-formed under insert / delete / split.
// ─────────────────────────────────────────────────────────────────────

/// Apply an "insert `inserted_bytes` of text at `byte_offset`" mutation
/// to a block's runs in place. Runs strictly before the offset are
/// unchanged; runs strictly after are shifted by +inserted_bytes; runs
/// that straddle the offset are extended (the inserted text inherits
/// the surrounding run's format — Qt / ProseMirror convention).
pub fn shift_runs_for_insert(runs: &mut [FormatRun], byte_offset: u32, inserted_bytes: u32) {
    if inserted_bytes == 0 {
        return;
    }
    for run in runs.iter_mut() {
        if run.byte_start >= byte_offset {
            run.byte_start += inserted_bytes;
            run.byte_end += inserted_bytes;
        } else if run.byte_end >= byte_offset {
            // Run straddles the insertion point, or its right edge sits
            // exactly on it. In both cases the inserted text inherits
            // this run's format (Qt convention).
            run.byte_end += inserted_bytes;
        }
    }
}

/// Apply a "delete byte range `[byte_start..byte_end)`" mutation to a
/// block's runs. Splices the range with empty replacement (clipping
/// straddling runs) and shifts everything past `byte_end` back by the
/// deleted length. Adjacent runs that end up equal-format are coalesced.
pub fn shift_runs_for_delete(runs: &mut Vec<FormatRun>, byte_start: u32, byte_end: u32) {
    if byte_end <= byte_start {
        return;
    }
    splice_range(runs, byte_start..byte_end, Vec::new());
    let delta = (byte_end - byte_start) as i32;
    shift_after(runs, byte_end, -delta);
    // The shift can make a left-clipped run abut a shifted trailing run
    // with identical format; coalesce once more to restore the invariant.
    coalesce_in_place(runs);
}

/// Apply a "replace byte range `[byte_start..byte_end)` with `replacement_bytes` bytes"
/// mutation to a block's runs, choosing explicitly what the replacement wears.
///
/// This is the one edit where the old delete-then-insert composition quietly lost a
/// writer's formatting: replacing `Auré**lien**` deletes both runs under the range and
/// then lets the replacement inherit whatever preceded it, so the bold is gone. That
/// behaviour is still available — and is still the default — but it is now a *decision*
/// ([`ReplaceFormatPolicy`]) rather than an accident of two functions being called in
/// sequence.
///
/// Additive by design: `shift_runs_for_delete` / `shift_runs_for_insert` are untouched,
/// because 13 other call sites across delete/insert/paste depend on their exact
/// behaviour. [`ReplaceFormatPolicy::InheritPreceding`] is implemented *by calling that
/// very composition*, so the default cannot drift away from what shipped.
///
/// Returns `Err` rather than corrupting the block if the range is inverted or the
/// re-splice would violate the run invariants — see [`FormatRunError`].
pub fn shift_runs_for_replace(
    runs: &mut Vec<FormatRun>,
    byte_start: u32,
    byte_end: u32,
    replacement_bytes: u32,
    policy: ReplaceFormatPolicy,
) -> Result<(), FormatRunError> {
    if byte_end < byte_start {
        return Err(FormatRunError::ReversedRange {
            start: byte_start,
            end: byte_end,
        });
    }

    // Decide what the replacement wears from the runs as they stand BEFORE the edit —
    // afterwards the runs under the range are gone and the evidence with them.
    //
    // `None` here means "do not override": let the composition below decide, which is
    // exactly `InheritPreceding`.
    //
    // An empty range replaces nothing, so it is an *insertion*, and the two
    // coverage-based policies have nothing to reason about — they must defer to the
    // insert convention rather than strip the format the typed text would have
    // inherited. Only `PreserveNothing`, which is an explicit request for unformatted
    // text, still applies.
    let destroys_formatting = byte_end > byte_start;
    let override_format: Option<Option<CharacterFormat>> = match policy {
        ReplaceFormatPolicy::InheritPreceding => None,
        ReplaceFormatPolicy::PreserveNothing => Some(None),
        ReplaceFormatPolicy::PreserveIfFullyCovered => {
            covering_format(runs, byte_start, byte_end).map(Some)
        }
        ReplaceFormatPolicy::KeepDominantRun if destroys_formatting => {
            Some(dominant_format(runs, byte_start, byte_end))
        }
        ReplaceFormatPolicy::KeepDominantRun => None,
    };

    // The historical composition. This IS `InheritPreceding`, byte for byte.
    shift_runs_for_delete(runs, byte_start, byte_end);
    shift_runs_for_insert(runs, byte_start, replacement_bytes);

    // Every other policy is a targeted re-splice of exactly the replacement's span.
    if let Some(format) = override_format
        && replacement_bytes > 0
    {
        let span = byte_start..byte_start + replacement_bytes;
        let replacement = match format {
            Some(format) => vec![FormatRun {
                byte_start: span.start,
                byte_end: span.end,
                format,
            }],
            None => Vec::new(),
        };
        try_splice_range(runs, span, replacement)?;
    }
    Ok(())
}

/// The format of the single run covering **all** of `[start..end)`, if one does.
///
/// An empty range is covered by nothing: a pure insertion has no formatting of its own
/// to preserve, so every policy falls back to inheritance there rather than silently
/// formatting typed text from a run it merely sits next to.
fn covering_format(runs: &[FormatRun], start: u32, end: u32) -> Option<CharacterFormat> {
    if end <= start {
        return None;
    }
    runs.iter()
        .find(|r| r.byte_start <= start && r.byte_end >= end)
        .map(|r| r.format.clone())
}

/// The format covering the most bytes of `[start..end)`, or `None` if unformatted text
/// covers more than any single run.
///
/// Gaps count as a candidate, so renaming inside a mostly-plain range stays plain. Ties
/// go to the formatted run: a tie means the formatting covered at least as much of the
/// range as its absence did, and dropping it is the destructive outcome.
fn dominant_format(runs: &[FormatRun], start: u32, end: u32) -> Option<CharacterFormat> {
    if end <= start {
        return None;
    }
    let span = u64::from(end - start);
    let mut covered = 0u64;
    let mut best: Option<(u64, &FormatRun)> = None;

    for r in runs {
        let lo = r.byte_start.max(start);
        let hi = r.byte_end.min(end);
        if hi <= lo {
            continue;
        }
        let overlap = u64::from(hi - lo);
        covered += overlap;
        // `>` keeps the EARLIEST run on a tie between two runs, so the result does not
        // depend on iteration order beyond the list's own sortedness.
        if best.is_none_or(|(best_overlap, _)| overlap > best_overlap) {
            best = Some((overlap, r));
        }
    }

    let plain = span - covered;
    match best {
        Some((overlap, run)) if overlap >= plain => Some(run.format.clone()),
        _ => None,
    }
}

/// Apply an "insert" shift to a block's image anchors. Anchors at or
/// past the offset move forward by `inserted_bytes`.
pub fn shift_images_for_insert(images: &mut [ImageAnchor], byte_offset: u32, inserted_bytes: u32) {
    if inserted_bytes == 0 {
        return;
    }
    for img in images.iter_mut() {
        if img.byte_offset >= byte_offset {
            img.byte_offset += inserted_bytes;
        }
    }
}

/// Apply a "delete" mutation to a block's image anchors. Anchors whose
/// `byte_offset` falls inside `[byte_start..byte_end)` are removed;
/// anchors at or past `byte_end` shift back by the deleted length.
/// Returns the number of anchors removed.
pub fn shift_images_for_delete(
    images: &mut Vec<ImageAnchor>,
    byte_start: u32,
    byte_end: u32,
) -> usize {
    if byte_end <= byte_start {
        return 0;
    }
    let before = images.len();
    images.retain(|i| !(i.byte_offset >= byte_start && i.byte_offset < byte_end));
    let removed = before - images.len();
    let delta = (byte_end - byte_start) as i32;
    shift_images_after(images, byte_end, -delta);
    removed
}

/// Translate a logical character offset (counting text characters AND
/// image positions interleaved by their `byte_offset`) into a UTF-8
/// byte offset within `plain_text`. Used by writer use cases to map a
/// document-space char position to the byte position where text edits
/// should land in `block.plain_text`.
///
/// Images contribute 1 logical character but 0 bytes in `plain_text`.
/// Images at the same byte_offset are visited in their stored order.
pub fn logical_offset_to_byte(plain_text: &str, images: &[ImageAnchor], char_offset: i64) -> u32 {
    if char_offset <= 0 {
        return 0;
    }
    let mut logical: i64 = 0;
    let mut images_consumed = 0usize;
    for (b, _) in plain_text.char_indices() {
        while images_consumed < images.len() && images[images_consumed].byte_offset <= b as u32 {
            if logical == char_offset {
                return b as u32;
            }
            logical += 1;
            images_consumed += 1;
        }
        if logical == char_offset {
            return b as u32;
        }
        logical += 1;
    }
    let plain_len = plain_text.len() as u32;
    while images_consumed < images.len() {
        if logical == char_offset {
            return plain_len;
        }
        logical += 1;
        images_consumed += 1;
    }
    plain_len
}

/// Split a block's format runs at `byte_offset`. The returned right-hand
/// vector has its run offsets re-based so they start at byte 0 of the
/// new (right) block. Straddling runs are split with their `format`
/// cloned to both halves.
pub fn split_runs_at(runs: &[FormatRun], byte_offset: u32) -> (Vec<FormatRun>, Vec<FormatRun>) {
    let mut left = Vec::new();
    let mut right = Vec::new();
    for run in runs {
        if run.byte_end <= byte_offset {
            left.push(run.clone());
        } else if run.byte_start >= byte_offset {
            right.push(FormatRun {
                byte_start: run.byte_start - byte_offset,
                byte_end: run.byte_end - byte_offset,
                format: run.format.clone(),
            });
        } else {
            left.push(FormatRun {
                byte_start: run.byte_start,
                byte_end: byte_offset,
                format: run.format.clone(),
            });
            right.push(FormatRun {
                byte_start: 0,
                byte_end: run.byte_end - byte_offset,
                format: run.format.clone(),
            });
        }
    }
    (left, right)
}

/// Split block image anchors at `byte_offset`. Anchors at exactly
/// `byte_offset` go to the right half (rebased to offset 0).
pub fn split_images_at(
    images: &[ImageAnchor],
    byte_offset: u32,
) -> (Vec<ImageAnchor>, Vec<ImageAnchor>) {
    let mut left = Vec::new();
    let mut right = Vec::new();
    for img in images {
        if img.byte_offset < byte_offset {
            left.push(img.clone());
        } else {
            let mut new = img.clone();
            new.byte_offset -= byte_offset;
            right.push(new);
        }
    }
    (left, right)
}

// ─────────────────────────────────────────────────────────────────────
// View synthesis: build a Vec<InlineSegment> from format_runs + images.
// ─────────────────────────────────────────────────────────────────────

/// Copy the `fmt_*` fields of an `InlineSegment` into a `CharacterFormat`.
pub fn character_format_from_segment(seg: &InlineSegment) -> CharacterFormat {
    CharacterFormat {
        font_family: seg.fmt_font_family.clone(),
        font_point_size: seg.fmt_font_point_size,
        font_weight: seg.fmt_font_weight,
        font_bold: seg.fmt_font_bold,
        font_italic: seg.fmt_font_italic,
        font_underline: seg.fmt_font_underline,
        font_overline: seg.fmt_font_overline,
        font_strikeout: seg.fmt_font_strikeout,
        letter_spacing: seg.fmt_letter_spacing,
        word_spacing: seg.fmt_word_spacing,
        anchor_href: seg.fmt_anchor_href.clone(),
        anchor_names: seg.fmt_anchor_names.clone(),
        is_anchor: seg.fmt_is_anchor,
        tooltip: seg.fmt_tooltip.clone(),
        underline_style: seg.fmt_underline_style.clone(),
        vertical_alignment: seg.fmt_vertical_alignment.clone(),
    }
}

/// Apply a `CharacterFormat` onto an `InlineSegment`'s fmt_* fields.
pub fn apply_character_format_to_segment(seg: &mut InlineSegment, fmt: &CharacterFormat) {
    seg.fmt_font_family = fmt.font_family.clone();
    seg.fmt_font_point_size = fmt.font_point_size;
    seg.fmt_font_weight = fmt.font_weight;
    seg.fmt_font_bold = fmt.font_bold;
    seg.fmt_font_italic = fmt.font_italic;
    seg.fmt_font_underline = fmt.font_underline;
    seg.fmt_font_overline = fmt.font_overline;
    seg.fmt_font_strikeout = fmt.font_strikeout;
    seg.fmt_letter_spacing = fmt.letter_spacing;
    seg.fmt_word_spacing = fmt.word_spacing;
    seg.fmt_anchor_href = fmt.anchor_href.clone();
    seg.fmt_anchor_names = fmt.anchor_names.clone();
    seg.fmt_is_anchor = fmt.is_anchor;
    seg.fmt_tooltip = fmt.tooltip.clone();
    seg.fmt_underline_style = fmt.underline_style.clone();
    seg.fmt_vertical_alignment = fmt.vertical_alignment.clone();
}

/// Synthesize a `Vec<InlineSegment>` view of a block from its
/// `plain_text`, `format_runs`, and `block_images`. Returns segments
/// in document order: a Text segment per format run (with a fallback
/// default-format segment for any uncovered bytes), and an Image
/// segment per anchor at its byte offset.
///
/// The canonical reader-side accessor for per-segment data — there is
/// no persistent inline-element table; this view is computed fresh
/// each call.
pub fn inline_segments_view(
    plain_text: &str,
    runs: &[FormatRun],
    images: &[ImageAnchor],
) -> Vec<InlineSegment> {
    let mut out: Vec<InlineSegment> = Vec::new();
    let bytes = plain_text.as_bytes();

    let mut img_iter = images.iter().peekable();
    let mut cursor: u32 = 0;

    let emit_text =
        |out: &mut Vec<InlineSegment>, bytes: &[u8], start: u32, end: u32, fmt: CharacterFormat| {
            if start >= end {
                return;
            }
            let slice = &bytes[start as usize..end as usize];
            let s = std::str::from_utf8(slice)
                .expect("block plain_text must be valid UTF-8")
                .to_string();
            let mut seg = InlineSegment {
                content: InlineContent::Text(s),
                ..Default::default()
            };
            apply_character_format_to_segment(&mut seg, &fmt);
            out.push(seg);
        };

    let emit_image = |out: &mut Vec<InlineSegment>, anchor: &ImageAnchor| {
        let mut seg = InlineSegment {
            content: InlineContent::Image {
                name: anchor.name.clone(),
                width: anchor.width,
                height: anchor.height,
                quality: anchor.quality,
            },
            ..Default::default()
        };
        apply_character_format_to_segment(&mut seg, &anchor.format);
        out.push(seg);
    };

    for run in runs {
        while let Some(img) = img_iter.peek() {
            if img.byte_offset < run.byte_start {
                emit_text(
                    &mut out,
                    bytes,
                    cursor,
                    img.byte_offset,
                    CharacterFormat::default(),
                );
                emit_image(&mut out, img);
                cursor = img.byte_offset;
                img_iter.next();
            } else {
                break;
            }
        }

        if cursor < run.byte_start {
            emit_text(
                &mut out,
                bytes,
                cursor,
                run.byte_start,
                CharacterFormat::default(),
            );
        }

        emit_text(
            &mut out,
            bytes,
            run.byte_start,
            run.byte_end,
            run.format.clone(),
        );
        cursor = run.byte_end;
    }

    for img in img_iter {
        if img.byte_offset > cursor {
            emit_text(
                &mut out,
                bytes,
                cursor,
                img.byte_offset,
                CharacterFormat::default(),
            );
            cursor = img.byte_offset;
        }
        emit_image(&mut out, img);
    }

    if (cursor as usize) < bytes.len() {
        emit_text(
            &mut out,
            bytes,
            cursor,
            bytes.len() as u32,
            CharacterFormat::default(),
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: u32, e: u32, bold: bool) -> FormatRun {
        FormatRun {
            byte_start: s,
            byte_end: e,
            format: CharacterFormat {
                font_bold: Some(bold),
                ..Default::default()
            },
        }
    }

    #[test]
    fn empty_runs_are_well_formed() {
        debug_assert_well_formed(&[], 0);
        debug_assert_well_formed(&[], 100);
    }

    #[test]
    fn coalesce_merges_adjacent_equal_runs() {
        let mut rs = vec![run(0, 5, true), run(5, 10, true), run(10, 15, false)];
        coalesce_in_place(&mut rs);
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0].byte_end, 10);
    }

    #[test]
    fn coalesce_leaves_disjoint_runs_alone() {
        let mut rs = vec![run(0, 5, true), run(7, 10, true)];
        coalesce_in_place(&mut rs);
        assert_eq!(rs.len(), 2);
    }

    #[test]
    fn splice_range_clips_straddling_runs() {
        let mut rs = vec![run(0, 20, true)];
        splice_range(&mut rs, 5..15, vec![run(5, 15, false)]);
        assert_eq!(rs.len(), 3);
        assert_eq!(rs[0].byte_end, 5);
        assert_eq!(rs[1].format.font_bold, Some(false));
        assert_eq!(rs[2].byte_start, 15);
    }

    #[test]
    fn splice_range_empty_replacement_removes_inner_runs() {
        let mut rs = vec![run(0, 5, true), run(5, 10, false), run(10, 15, true)];
        splice_range(&mut rs, 5..10, vec![]);
        // 0..5 bold, then 10..15 bold — after coalesce these are NOT adjacent
        // (there's a gap from 5..10 in the run table, meaning "no format").
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0].byte_end, 5);
        assert_eq!(rs[1].byte_start, 10);
    }

    #[test]
    fn shift_after_moves_downstream() {
        let mut rs = vec![run(0, 5, true), run(10, 15, false)];
        shift_after(&mut rs, 5, 3);
        assert_eq!(rs[0].byte_start, 0); // unchanged
        assert_eq!(rs[1].byte_start, 13);
        assert_eq!(rs[1].byte_end, 18);
    }
}

/// The adversarial corpus for [`shift_runs_for_replace`].
///
/// Replace is the one edit that rewrites a writer's prose *in bulk*, so its formatting
/// behaviour has to be pinned rather than inherited by accident. Before this module
/// there were 11 replace tests in the repo and **not one** of them mentioned
/// `FormatRun`: the behaviour was emergent, undecided, and untested.
#[cfg(test)]
mod replace_policy_tests {
    use super::*;

    fn fmt(tag: &str) -> CharacterFormat {
        CharacterFormat {
            font_bold: Some(tag == "B"),
            font_italic: Some(tag == "I"),
            ..Default::default()
        }
    }
    fn r(start: u32, end: u32, tag: &str) -> FormatRun {
        FormatRun {
            byte_start: start,
            byte_end: end,
            format: fmt(tag),
        }
    }
    /// Compact "0..5=B 8..12=I" rendering, so a failure shows the whole run list.
    fn show(runs: &[FormatRun]) -> String {
        if runs.is_empty() {
            return "[]".to_string();
        }
        runs.iter()
            .map(|x| {
                let tag = if x.format.font_bold == Some(true) {
                    "B"
                } else if x.format.font_italic == Some(true) {
                    "I"
                } else {
                    "p"
                };
                format!("{}..{}={tag}", x.byte_start, x.byte_end)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
    fn replace(
        runs: &[FormatRun],
        start: u32,
        end: u32,
        n: u32,
        policy: ReplaceFormatPolicy,
    ) -> Vec<FormatRun> {
        let mut runs = runs.to_vec();
        shift_runs_for_replace(&mut runs, start, end, n, policy).expect("valid replace");
        runs
    }

    /// **The spec-conformance test.** `InheritPreceding` must be byte-for-byte what
    /// today's `shift_runs_for_delete` + `shift_runs_for_insert` produces — anything
    /// else silently rewrites the formatting of every replace that ever shipped.
    ///
    /// Differential, not hand-transcribed: it runs the historical primitives directly
    /// and compares, so it stays honest even if their behaviour is ever changed.
    #[test]
    fn inherit_preceding_matches_the_historical_delete_then_insert() {
        let corpus: Vec<(&str, Vec<FormatRun>, u32, u32, u32)> = vec![
            ("run ends exactly at start", vec![r(0, 5, "B")], 5, 10, 3),
            ("run begins exactly at start", vec![r(5, 8, "B")], 5, 10, 3),
            (
                "run begins at start, outlives end",
                vec![r(5, 20, "B")],
                5,
                10,
                3,
            ),
            (
                "run straddles the whole range",
                vec![r(0, 20, "B")],
                5,
                10,
                3,
            ),
            ("no run touches the start", vec![r(12, 20, "B")], 5, 10, 3),
            ("bold tail inside the range", vec![r(9, 13, "B")], 5, 13, 4),
            ("pure delete", vec![r(0, 20, "B")], 5, 10, 0),
            ("pure insert", vec![r(0, 20, "B")], 5, 5, 3),
            (
                "same format either side coalesces",
                vec![r(0, 5, "B"), r(10, 15, "B")],
                5,
                10,
                3,
            ),
            (
                "different formats either side",
                vec![r(0, 5, "B"), r(10, 15, "I")],
                5,
                10,
                3,
            ),
            ("empty run list", vec![], 5, 10, 3),
            ("the only run is consumed", vec![r(5, 10, "B")], 5, 10, 3),
            (
                "replacement longer than the range",
                vec![r(0, 5, "B")],
                5,
                10,
                20,
            ),
            (
                "three runs straddled",
                vec![r(0, 3, "B"), r(3, 6, "I"), r(6, 9, "B")],
                2,
                7,
                4,
            ),
            (
                "gap between two same-format runs is deleted",
                vec![r(0, 5, "B"), r(8, 13, "B")],
                5,
                8,
                0,
            ),
        ];

        for (name, runs, start, end, n) in corpus {
            // The historical composition, run for real.
            let mut expected = runs.clone();
            shift_runs_for_delete(&mut expected, start, end);
            shift_runs_for_insert(&mut expected, start, n);

            let got = replace(&runs, start, end, n, ReplaceFormatPolicy::InheritPreceding);

            assert_eq!(
                show(&got),
                show(&expected),
                "InheritPreceding diverged from delete+insert for {name:?} \
                 (replace {start}..{end}, n={n})\n  before:   {}\n  historical: {}\n  got:        {}",
                show(&runs),
                show(&expected),
                show(&got),
            );
        }
    }

    /// The motivating data loss: renaming a character whose name reads `Auré**lien**`.
    /// The default drops the bold — that is what shipped, and it is now visible and
    /// chosen rather than emergent. Every other policy is a way to not lose it.
    #[test]
    fn the_four_policies_diverge_on_a_partly_bold_name() {
        // "Auré" plain (bytes 0..5, é is two bytes), "lien" bold (5..9).
        let runs = vec![r(5, 9, "B")];
        let (start, end, n) = (0, 9, 9); // rename the whole name, same length

        use ReplaceFormatPolicy::*;
        assert_eq!(
            show(&replace(&runs, start, end, n, InheritPreceding)),
            "[]",
            "the historical default destroys the bold — pinned, not endorsed"
        );
        assert_eq!(
            show(&replace(&runs, start, end, n, PreserveNothing)),
            "[]",
            "explicitly unformatted"
        );
        assert_eq!(
            show(&replace(&runs, start, end, n, PreserveIfFullyCovered)),
            "[]",
            "no SINGLE run covers 0..9 — it must fall back to inheritance, not guess"
        );
        // Bold covers 4 of the 9 bytes, plain covers 5 → plain dominates.
        assert_eq!(
            show(&replace(&runs, start, end, n, KeepDominantRun)),
            "[]",
            "plain covers more of the name than the bold does"
        );

        // …but when the bold covers MOST of the name, KeepDominantRun keeps it.
        let mostly_bold = vec![r(1, 9, "B")];
        assert_eq!(
            show(&replace(&mostly_bold, 0, 9, 9, KeepDominantRun)),
            "0..9=B",
            "bold covers 8 of 9 bytes — the rename must keep it"
        );
    }

    /// "Fully covered" means ONE run covers the range — not "the runs jointly span it".
    /// A gapless Italic+Bold union spanning the range exactly must NOT be treated as
    /// covered, or the replacement silently inherits whichever run was looked at first.
    #[test]
    fn fully_covered_means_a_single_run_not_a_gapless_union() {
        let two = vec![r(0, 3, "I"), r(3, 10, "B")];
        assert_eq!(
            show(&replace(
                &two,
                0,
                10,
                4,
                ReplaceFormatPolicy::PreserveIfFullyCovered
            )),
            "[]",
            "two different-format runs jointly spanning the range are not 'covered'; \
             with no run preceding the start, the fallback is unformatted"
        );

        // One run that really does cover it, and begins exactly at the start — the case
        // InheritPreceding cannot see (a run at `start` never inherits).
        let one = vec![r(5, 20, "B")];
        assert_eq!(
            show(&replace(
                &one,
                5,
                10,
                3,
                ReplaceFormatPolicy::PreserveIfFullyCovered
            )),
            "5..18=B",
            "a single covering run keeps its format across the rename"
        );
        assert_eq!(
            show(&replace(
                &one,
                5,
                10,
                3,
                ReplaceFormatPolicy::InheritPreceding
            )),
            "8..18=B",
            "…which the default would have lost: the replacement lands unformatted"
        );
    }

    /// A run that merely *touches* the range must not leak its format to the whole
    /// replacement.
    #[test]
    fn a_partially_overlapping_run_does_not_count_as_covering() {
        let runs = vec![r(0, 8, "B")]; // covers only 5..8 of the range 5..12
        assert_eq!(
            show(&replace(
                &runs,
                5,
                12,
                4,
                ReplaceFormatPolicy::PreserveIfFullyCovered
            )),
            "0..9=B",
            "not covered → falls back to inheritance, which extends the preceding bold; \
             it must NOT format the whole replacement as though bold had covered it"
        );
    }

    /// Ties between two runs resolve to the EARLIEST, deterministically.
    ///
    /// `Iterator::max_by_key` returns the LAST maximum, so the obvious one-liner would
    /// have silently picked the other run here.
    #[test]
    fn a_dominance_tie_between_two_runs_goes_to_the_earlier() {
        let runs = vec![r(0, 3, "B"), r(3, 6, "I")]; // 3 bytes each
        assert_eq!(
            show(&replace(
                &runs,
                0,
                6,
                4,
                ReplaceFormatPolicy::KeepDominantRun
            )),
            "0..4=B",
            "a true tie must resolve to the earlier run, not to whichever the iterator \
             happened to visit last"
        );
    }

    /// A tie between a run and the unformatted gap goes to the run: losing formatting is
    /// the destructive outcome, so it needs a strict majority of *plain* to win.
    #[test]
    fn a_dominance_tie_against_plain_text_keeps_the_formatting() {
        let runs = vec![r(4, 8, "B")]; // 4 bold bytes, 4 plain bytes in 0..8
        assert_eq!(
            show(&replace(
                &runs,
                0,
                8,
                5,
                ReplaceFormatPolicy::KeepDominantRun
            )),
            "0..5=B",
            "an even split must keep the formatting rather than silently drop it"
        );
    }

    /// An empty range replaces nothing, so it is an insertion — and every policy that
    /// reasons about "what was covered" must defer to the insert convention instead of
    /// stripping the format the typed text would have inherited.
    #[test]
    fn an_empty_range_is_an_insert_and_no_coverage_policy_overrides_it() {
        let runs = vec![r(0, 5, "B"), r(5, 10, "I")];
        use ReplaceFormatPolicy::*;
        for policy in [InheritPreceding, PreserveIfFullyCovered, KeepDominantRun] {
            assert_eq!(
                show(&replace(&runs, 5, 5, 2, policy)),
                "0..7=B 7..12=I",
                "{policy:?}: typing at a boundary must inherit the run to the LEFT (Qt \
                 convention) — an empty range destroyed no formatting, so there is \
                 nothing for a coverage policy to override"
            );
        }
        // The one policy that is an explicit request, not an inference, still applies.
        assert_eq!(
            show(&replace(&runs, 5, 5, 2, PreserveNothing)),
            "0..5=B 7..12=I",
            "PreserveNothing asks for unformatted text, and means it even on an insert"
        );
    }

    /// Nothing to do must mean nothing done — no fabricated runs, no lost ones.
    #[test]
    fn a_zero_width_zero_length_replace_is_the_identity() {
        let runs = vec![r(0, 5, "B"), r(7, 12, "I")];
        for policy in [
            ReplaceFormatPolicy::InheritPreceding,
            ReplaceFormatPolicy::PreserveIfFullyCovered,
            ReplaceFormatPolicy::KeepDominantRun,
            ReplaceFormatPolicy::PreserveNothing,
        ] {
            assert_eq!(
                show(&replace(&runs, 6, 6, 0, policy)),
                "0..5=B 7..12=I",
                "{policy:?} changed a no-op edit"
            );
        }
    }

    /// `PreserveNothing` must leave the region genuinely *unformatted* — not covered by
    /// a fabricated run carrying `CharacterFormat::default()`, which is a different
    /// thing and would defeat coalescing forever after.
    #[test]
    fn preserve_nothing_fabricates_no_default_run() {
        let runs = vec![r(0, 5, "B")];
        let got = replace(&runs, 7, 9, 2, ReplaceFormatPolicy::PreserveNothing);
        assert_eq!(show(&got), "0..5=B", "no run may be invented for the gap");
        assert!(
            got.iter().all(|x| x.byte_start < 7 || x.byte_end > 9),
            "the replaced span must carry no run at all"
        );
    }

    /// Offsets are BYTES, not chars. A 4-byte emoji replaced by 2 ASCII bytes must
    /// shift downstream runs by -2, not by -1 (chars) or 0.
    #[test]
    fn offsets_are_bytes_not_characters() {
        // "🎉" (4 bytes, bold) + " " + "abcd" (italic).
        let runs = vec![r(0, 4, "B"), r(5, 9, "I")];
        let got = replace(&runs, 0, 4, 2, ReplaceFormatPolicy::KeepDominantRun);
        assert_eq!(
            show(&got),
            "0..2=B 3..7=I",
            "the trailing italic must shift back by the BYTE delta (4 -> 2 = -2)"
        );
    }

    /// Every policy must leave the run list well-formed — sorted, non-overlapping,
    /// coalesced, inside the block. This is the invariant a release build no longer
    /// merely asserts.
    #[test]
    fn every_policy_leaves_the_runs_well_formed() {
        let setups: Vec<(Vec<FormatRun>, u32, u32, u32, usize)> = vec![
            (vec![r(0, 3, "B"), r(3, 6, "I"), r(6, 9, "B")], 2, 7, 4, 8),
            (vec![r(0, 5, "B"), r(8, 13, "B")], 5, 8, 0, 10),
            (vec![r(0, 5, "B"), r(7, 12, "B")], 8, 10, 2, 12),
            (vec![r(3, 7, "B")], 3, 7, 0, 6),
            (vec![], 2, 6, 3, 9),
        ];
        for (runs, start, end, n, text_len) in setups {
            for policy in [
                ReplaceFormatPolicy::InheritPreceding,
                ReplaceFormatPolicy::PreserveIfFullyCovered,
                ReplaceFormatPolicy::KeepDominantRun,
                ReplaceFormatPolicy::PreserveNothing,
            ] {
                let got = replace(&runs, start, end, n, policy);
                check_well_formed(&got, text_len).unwrap_or_else(|e| {
                    panic!(
                        "{policy:?} produced malformed runs from {} (replace {start}..{end}, \
                         n={n}): {} — {e}",
                        show(&runs),
                        show(&got)
                    )
                });
            }
        }
    }

    /// Two same-format runs separated by an untouched gap must stay separate — coalescing
    /// is for *adjacent* runs, and over-eager merging would swallow the plain text between
    /// them.
    #[test]
    fn an_untouched_gap_between_equal_runs_survives() {
        let runs = vec![r(0, 5, "B"), r(7, 12, "B")];
        assert_eq!(
            show(&replace(
                &runs,
                8,
                10,
                2,
                ReplaceFormatPolicy::InheritPreceding
            )),
            "0..5=B 7..12=B",
            "the plain gap at 5..7 must not be swallowed"
        );
    }

    /// An inverted range is refused, not asserted away.
    #[test]
    fn an_inverted_range_is_an_error_not_a_panic() {
        let mut runs = vec![r(0, 5, "B")];
        let err =
            shift_runs_for_replace(&mut runs, 10, 5, 3, ReplaceFormatPolicy::InheritPreceding)
                .expect_err("an inverted range must be rejected");
        assert!(matches!(
            err,
            FormatRunError::ReversedRange { start: 10, end: 5 }
        ));
        assert_eq!(show(&runs), "0..5=B", "a refused edit must change nothing");
    }
}

/// The runtime guards that replaced the release-mode-invisible `debug_assert!`s.
#[cfg(test)]
mod invariant_check_tests {
    use super::*;

    fn run(start: u32, end: u32, bold: bool) -> FormatRun {
        FormatRun {
            byte_start: start,
            byte_end: end,
            format: CharacterFormat {
                font_bold: Some(bold),
                ..Default::default()
            },
        }
    }

    /// The whole point: in a **release** build `debug_assert!` is gone, so a violating
    /// splice used to sail straight through and build a malformed run list. Now it is
    /// reported, and the caller's runs are left exactly as they were.
    #[test]
    fn a_replacement_outside_the_range_is_rejected_without_mutating() {
        let mut runs = vec![run(0, 20, true)];
        let before = runs.clone();

        let err = try_splice_range(&mut runs, 5..10, vec![run(5, 15, false)])
            .expect_err("a replacement run reaching past range.end must be rejected");

        assert!(matches!(
            err,
            FormatRunError::ReplacementOutsideRange {
                run_end: 15,
                range_end: 10,
                ..
            }
        ));
        assert_eq!(
            runs, before,
            "a rejected splice must not half-apply — validation happens before mutation"
        );
    }

    #[test]
    // The inverted range is the whole point of the test — it is what a caller must not
    // be able to sneak past a release build.
    #[allow(clippy::reversed_empty_ranges)]
    fn a_reversed_range_is_rejected() {
        let mut runs = vec![run(0, 20, true)];
        assert!(matches!(
            try_splice_range(&mut runs, 10..5, vec![]),
            Err(FormatRunError::ReversedRange { start: 10, end: 5 })
        ));
    }

    #[test]
    fn a_legal_splice_still_works_through_the_checked_path() {
        let mut runs = vec![run(0, 20, true)];
        try_splice_range(&mut runs, 5..15, vec![run(5, 15, false)]).expect("legal");
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[1].format.font_bold, Some(false));
    }

    #[test]
    fn check_well_formed_catches_what_debug_assert_used_to() {
        assert!(check_well_formed(&[], 0).is_ok());
        assert!(check_well_formed(&[run(0, 5, true)], 5).is_ok());

        assert!(matches!(
            check_well_formed(&[run(5, 5, true)], 10),
            Err(FormatRunError::EmptyRun { .. })
        ));
        assert!(matches!(
            check_well_formed(&[run(0, 8, true), run(5, 10, false)], 10),
            Err(FormatRunError::RunsOverlap { .. })
        ));
        assert!(matches!(
            check_well_formed(&[run(0, 5, true), run(5, 10, true)], 10),
            Err(FormatRunError::RunsNotCoalesced { .. })
        ));
        assert!(matches!(
            check_well_formed(&[run(0, 20, true)], 10),
            Err(FormatRunError::RunPastEndOfBlock { text_len: 10, .. })
        ));
    }
}
