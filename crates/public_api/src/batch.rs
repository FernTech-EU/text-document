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
use frontend::document::dtos::CreateDocumentDto;
use frontend::root::dtos::CreateRootDto;
use frontend::common::parser_tools::{DjotExportOptions, DjotImportOptions};
use frontend::document_io::ImportDjotDto;

use crate::error::Result;
use crate::{FindMatch, FindOptions};

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
                options: options.clone(),
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
        let result = document_search_commands::find_all(&self.ctx, &options.to_find_all_dto(query))?;
        Ok(result
            .positions
            .into_iter()
            .zip(result.lengths)
            .map(|(position, length)| FindMatch {
                position: position as usize,
                length: length as usize,
            })
            .collect())
    }
}
