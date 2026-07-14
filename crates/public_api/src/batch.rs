//! [`BatchDocument`] — a headless, thread-free document for backend batch work.
//!
//! [`TextDocument`](crate::TextDocument) is built for a *view*. Constructing one spins
//! up a dedicated OS thread (`EventHubClient::start`, the only `thread::spawn` in the
//! crate) so a widget can be notified of changes asynchronously. That is exactly right
//! for an editor and exactly wrong for a batch: a backend rewriting a character's name
//! across a manuscript would spawn one thread per touched scene to do work it wants to
//! do inline anyway.
//!
//! `BatchDocument` is the same document machinery without the view's apparatus. It
//! holds an [`AppContext`] — which is itself thread-free — and drives the same
//! `frontend::commands` entry points `TextDocument` drives, so there is no parallel
//! implementation to drift out of step. What it never does is start an
//! `EventHubClient`, so it costs no thread.
//!
//! It is deliberately *not* a `TextDocument`: no cursor, no layout, no highlighting,
//! no view. A caller who wants those wants the real thing. What it has is the parser,
//! the format runs, the search and the exporter — which is all a search-and-replace
//! needs, and all it should be able to reach.
//!
//! ```ignore
//! let batch = BatchDocument::new();
//! batch.set_djot(&content.data, &DjotImportOptions::default())?;
//! let hits = batch.find_all("Aurélien", &FindOptions::default())?;
//! let djot = batch.to_djot(&DjotExportOptions::default())?;
//! ```

use frontend::AppContext;
use frontend::commands::{
    document_commands, document_io_commands, document_search_commands, root_commands,
};
use frontend::common::parser_tools::{DjotExportOptions, DjotImportOptions};
use frontend::document::dtos::CreateDocumentDto;
use frontend::document_io::ImportDjotDto;
use frontend::root::dtos::CreateRootDto;

use crate::error::Result;
use crate::{FindMatch, FindOptions, ReplaceOptions, ReplaceRange};

/// A headless document: no thread, no view, no cursor. See the module docs.
pub struct BatchDocument {
    ctx: AppContext,
}

impl BatchDocument {
    /// Bootstrap an empty document. No I/O, no thread.
    ///
    /// `AppContext::new()` gives the machinery (store, event hub, undo manager) but no
    /// entities — so, like `TextDocument`, this seeds the `Root → Document` the
    /// importer needs to hang its frames from. It stops there: `TextDocument` also
    /// seeds a Frame and a Block for an empty editor to show a caret in, but an import
    /// rebuilds the document's frames anyway, and a batch has no caret to place.
    ///
    /// `stack_id: None` — a batch has no undo of its own. Its caller's transaction is
    /// the unit that gets rolled back.
    pub fn new() -> Result<Self> {
        let ctx = AppContext::new();
        let root = root_commands::create_orphan_root(&ctx, &CreateRootDto::default())?;
        document_commands::create_document(&ctx, None, &CreateDocumentDto::default(), root.id, -1)?;
        Ok(Self { ctx })
    }

    /// Replace the document's contents by parsing `djot`.
    ///
    /// Goes through the **synchronous** import path. The public `import_djot` hands
    /// the use case to a `LongOperationManager`, which spawns a thread for it — the
    /// very thing this type exists to avoid.
    pub fn set_djot(&self, djot: &str, options: &DjotImportOptions) -> Result<()> {
        document_io_commands::import_djot_sync(
            &self.ctx,
            &ImportDjotDto {
                djot_text: djot.to_string(),
                options: *options,
            },
        )?;
        Ok(())
    }

    /// Serialise the document back to Djot.
    pub fn to_djot(&self, options: &DjotExportOptions) -> Result<String> {
        Ok(document_io_commands::export_djot(&self.ctx, options)?.djot_text)
    }

    /// Every match of `query` in the document's text.
    ///
    /// Positions are **char indices into the document's own text** — the same space
    /// every other offset in this crate lives in. They are emphatically *not* byte
    /// offsets into the Djot source the document was parsed from: markup means the two
    /// disagree on almost any real paragraph, and mixing them corrupts a replace.
    pub fn find_all(&self, query: &str, options: &FindOptions) -> Result<Vec<FindMatch>> {
        let result =
            document_search_commands::find_all(&self.ctx, &options.to_find_all_dto(query))?;
        // The same conversion `TextDocument::find_all` uses. Written twice, the two would
        // drift — and a batch search that disagreed with the live one about what it matched
        // is precisely the bug this crate keeps re-learning.
        Ok(crate::convert::find_all_to_matches(&result))
    }

    /// Find every match of `query` and let `decide` choose what each becomes — the whole
    /// thing in one shot.
    ///
    /// `decide` is handed the matched text and the index of the match, and returns the
    /// replacement, or `None` to leave that occurrence alone. That is exactly what a reviewed
    /// bulk rename needs: skip the occurrences the writer unticked, and preserve the case of
    /// the ones that stay (`AURÉLIEN` → `AURÉLIAN`, not `aurélian`).
    ///
    /// **This is why a backend batch can stop doing string surgery on markup.** The
    /// alternative — export the Djot and rewrite it as a string — rewrites a query that also
    /// occurs inside a link's URL, and drops the character formatting under every match. Here
    /// the edit happens *inside the document*, at the offsets the parser itself reports, and
    /// `ReplaceOptions::format_policy` decides what the replacement wears where it overwrites
    /// formatted prose.
    ///
    /// The matched text comes back from the scan itself, sliced from the very text that was
    /// searched — never from a plain-text export, which is the human-readable view and does
    /// not carry the `U+FFFC` anchor an embedded table occupies.
    pub fn find_and_replace(
        &self,
        query: &str,
        options: &ReplaceOptions,
        mut decide: impl FnMut(&str, usize) -> Option<String>,
    ) -> Result<usize> {
        let found =
            document_search_commands::find_all(&self.ctx, &options.find.to_find_all_dto(query))?;

        let mut ranges: Vec<ReplaceRange> = Vec::new();
        for (i, ((&position, &length), matched)) in found
            .positions
            .iter()
            .zip(found.lengths.iter())
            .zip(found.matched_texts.iter())
            .enumerate()
        {
            if let Some(replacement) = decide(matched, i) {
                ranges.push(ReplaceRange {
                    position: position as usize,
                    length: length as usize,
                    replacement,
                });
            }
        }

        if ranges.is_empty() {
            return Ok(0);
        }
        self.replace_ranges(&ranges, options)
    }

    /// Replace an explicit set of ranges, each with its own replacement text.
    ///
    /// Ranges that straddle a block boundary, or that overlap one another, are skipped; the
    /// returned count is what was actually applied. Prefer [`Self::find_and_replace`], which
    /// derives the ranges from a scan of the document as it stands, so they cannot address
    /// text that has since moved.
    pub fn replace_ranges(
        &self,
        ranges: &[ReplaceRange],
        options: &ReplaceOptions,
    ) -> Result<usize> {
        // `stack_id: None` — a batch has no undo of its own; its caller owns the undo entry.
        let result = document_search_commands::replace_ranges(
            &self.ctx,
            None,
            &options.to_replace_ranges_dto(ranges),
        )?;
        Ok(result.replacements_count.max(0) as usize)
    }
}
