// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Cyril Jacquet

//! M-T2a: the ODT (OpenDocument Text) writer. Mirrors `export_docx_uc.rs`'s shape — the same
//! `LongOperation`, the same Root→Document→Frame→Block walk, the same `child_order` interleaving,
//! the same footnote pre-render pass — but every "build a DOCX paragraph" step here instead
//! builds one fragment of raw ODF XML, since there is no `docx-rs`-equivalent builder crate for
//! ODF to lean on (see `crate::odt_render`'s module doc for why). Comments in this module
//! describe *this writer's own* decisions; the low-level XML/style plumbing (escaping,
//! automatic-style dedup, the styles.xml skeleton, zip packaging) lives in `crate::odt_render`.
//!
//! ## M-T2b: comment ranges, layered onto the M-T2a walker
//!
//! Skribisto's own `.odt` **reader** (`document_ingest::sources::odt`) already understands
//! `office:annotation` comment threads — this writer's own encoding was measured against it
//! (specifically its `open_annotation`/`close_annotation`, ~odt.rs:793-970), not against the ODF
//! spec, because ODF standardises no reply-threading construct at all. Four things that reading
//! established, which this module's own comment machinery exists to satisfy exactly:
//!
//!  - **Pairing**: an `office:annotation`/`office:annotation-end` pair, matched by a generated
//!    `office:name`, brackets a `[start, end)` range in the document's addressable character
//!    space — the exact space [`common::format_runs_query::addressable_inline_pieces_for_block`]
//!    resolves, and the one `common::parser_tools::DocumentComment::start`/`end` are defined in.
//!    A whole comment's data (author, date, resolved flag, body) lives on the *opening* tag —
//!    unlike DOCX, where `docx-rs`'s own auto-collector moves a comment's body into a separate
//!    `comments.xml` part, ODF's `<office:annotation>` carries its body inline, at the point its
//!    range starts.
//!  - **Threading**: a reply is a *sibling* `office:annotation` carrying `loext:parent-name`
//!    pointing at the thread root's own `office:name` — a LibreOffice extension with no ODF
//!    standard equivalent, confirmed by round-tripping a hand-written file through
//!    `.docx`→`.odt` in LibreOffice 25.8 (see the reader's own module doc). A reply shares its
//!    root's exact `[start, end)` range, never a range of its own — see `DocumentComment`'s doc
//!    comment for why.
//!  - **Resolved**: `loext:resolved="true"`/`"false"`, read by the same attribute name.
//!  - **uid**: carried on a *new* private-namespace attribute, `skrb:uid` (declared in
//!    `odt_render::NAMESPACES`) — never on `office:name`, which is already spoken for as the
//!    annotation-start/-end pairing key. `office:name`'s schema type is a plain string (nothing
//!    about ODF requires it to be an NCName), so nothing stops one attribute serving both roles
//!    — but doing that anyway would mean a value with two independent meanings, which is worse
//!    engineering than a second attribute even where the schema would tolerate it.
//!
//! One field `DocumentComment` carries has no ODF home at all: `author_initials`. Word's own
//! comment UI shows an author's initials as a small badge (`w:initials`, which
//! `export_docx_uc::patch_comment_extras` writes); LibreOffice's comment sidebar has never shown
//! anything but the full `dc:creator` name, and `<office:annotation>`'s schema has no attribute
//! for it. Dropping it here is a genuine format ceiling, not an oversight — there is nowhere in
//! a valid ODF file to put it.
//!
//! **Rich bodies, and the one DOCX constraint that does NOT carry over**: a comment's body is
//! Djot source, rendered to real ODF markup by [`render_comment_body_odt`] — bold, italic,
//! underline, strikethrough, and paragraph/line breaks, the same narrow scope
//! `export_docx_uc::render_comment_body` covers. That function collapses a multi-paragraph body
//! to exactly *one* `docx_rs::Paragraph`, but only because of a `docx-rs` 0.4.22 bug: its
//! auto-collector duplicates the whole `<w:comment>` once per body paragraph it finds. Nothing
//! here goes through an auto-collecting builder — `<office:annotation>` just holds however many
//! `<text:p>` children it is given — so [`render_comment_body_odt`] emits one real `<text:p>`
//! per Djot paragraph, which is what a reader (and a human opening the file in LibreOffice)
//! actually expects to see.
//!
//! **Paragraph-kind comments** (no highlighted text — the marker sits on the paragraph itself)
//! need nothing special here: Skribisto's own export pipeline is what decides a paragraph
//! comment's `[start, end)` (the paragraph's trimmed extent) before this writer ever sees it, so
//! from this module's side every comment is just an ordinary range. The same is true of an
//! unresolvable comment (no anchor a caller could find at all): Skribisto's orphan
//! classification happens upstream too, so an unanchored comment is simply never present in
//! `OdtExportOptions::comments` — this writer never needs to filter one out itself.
//!
//! **Out of scope, deliberately, mirroring `export_docx_uc`'s own boundaries**: a comment whose
//! range falls inside a fenced code block, or on a paragraph the scene-break/rule heuristic
//! replaced with an empty `"Rule"` paragraph (see this module's own section below), is not
//! anchored — neither branch ever reaches [`add_inline_content`], the only place a comment
//! marker is placed. Also out of scope: a footnote body (pre-rendered before the main walk, over
//! content `to_addressable_text` never descends into) and table-cell prose (the addressable text
//! represents a table by one sentinel character, never by its cells' own content). Any comment
//! that never finds a home in the walk is a loud, named [`anyhow::Error`] from
//! [`CommentEmitState::ensure_all_anchored`] — never a silently thinner `.odt` than the caller
//! asked for.
//!
//! ## A structural difference from DOCX that shapes this whole module: lists are real trees
//!
//! OOXML represents list nesting as a flat sequence of paragraphs each carrying an `w:ilvl`
//! (indent level) — `export_docx_uc` never has to build a tree, Word derives the nesting purely
//! from adjacent paragraphs sharing a `w:numId` at different levels. ODF instead nests
//! `<text:list>` elements structurally: a sub-list is XML nested one level down, inside the
//! `<text:list-item>` of whichever item it belongs under. [`ListStack`] is what turns this
//! writer's flat, `List`-entity-tagged block sequence into that nested shape, closing and
//! re-opening `<text:list>` elements exactly when the sequence's `(List entity, indent)` pair
//! changes. Every `<text:list-style>` this writer declares carries the *same* numbering
//! format/glyph at all nine levels (unlike DOCX's `build_level`, which does too, for the same
//! reason) rather than only the one level that entity's own `indent` names: ODF resolves which
//! level applies to a nested `<text:list>` from **how deep it is structurally nested among any
//! ancestor `<text:list>` elements**, not from a level recorded on the entity — see
//! `document_ingest::sources::odt::Walker::walk_list`'s own `depth` parameter, which counts
//! nesting occurrences the identical way. Defining all nine levels identically means whatever
//! structural depth a list ends up at, it still resolves to *this* list's own configured
//! ordered/bulleted-ness — only the per-level indent (`text:space-before`) actually varies, which
//! is what gives a nested list its extra visual indent without needing the "true" depth known
//! ahead of time.
//!
//! ## The scene-break / horizontal-rule heuristic, and its one hard constraint
//!
//! `document_ingest::sources::odt`'s reader treats an **empty** paragraph styled with a
//! bottom-border-only paragraph style as a thematic break (see that module's "A horizontal line
//! *is* a scene break" doc section) — the ODF spelling `document_ingest` measured against real
//! LibreOffice output (and the same construct Pandoc emits for `* * *` under the name
//! "Horizontal Line"). This crate has no dependency on `skribisto_model` and therefore no access
//! to its canonical scene-break glyphs (`"* * *"`, `"# # #"`, …) — and would not want one anyway,
//! since a scene break's actual glyph is a **per-project preset** (`skribisto_compiler::preset`
//! offers a dozen different ones: `"#"`, `"***"`, `". . ."`, `"＊"`, `"◇"`, …), never a fixed set
//! this crate could enumerate. [`looks_like_rule_glyph`] instead recognises the *general shape*
//! every one of those presets shares: a plain, unformatted paragraph whose text, once whitespace
//! is stripped, is one non-alphanumeric character repeated — true for every glyph above, and for
//! the CommonMark thematic-break syntax the same way `document_ingest`'s own doc comment already
//! draws the analogy to.
//!
//! **Constraint that shaped [`OdtStyleSheet::paragraph_style`]'s design in `odt_render.rs`:** a
//! rule paragraph is *always* written referencing the named `"Rule"` style directly, never behind
//! a per-block automatic-style wrapper (not even to add a quote-depth indent). The reader's
//! `StyleTable::is_rule` walks a style's parent chain but **stops at the first style carrying
//! *any* `<style:paragraph-properties>` element**, rule or not — so an automatic style adding,
//! say, `fo:margin-left` on top of `"Rule"` would present its own (border-less)
//! paragraph-properties first and read back as "not a rule" before the walk ever reaches the real
//! definition. Simplest fix, applied here: a rule paragraph never gets a wrapper style at all.

use crate::ExportOdtDto;
use crate::ExportOdtResultDto;
use crate::odt_render::{self, OdtStyleSheet};
use anyhow::{Result, anyhow};
use common::database::QueryUnitOfWork;
use common::database::rope_helpers::{block_content_via_store, block_document_position};
use common::entities::{
    Alignment, Block, Document, Frame, List, ListStyle, MarkerType, Root, SemanticRole, Table,
    TableCell, TextDirection,
};
use common::format_runs::{InlineContent, InlineSegment};
use common::format_runs_query::{addressable_inline_pieces_for_block, inline_segments_for_block};
use common::long_operation::LongOperation;
use common::parser_tools::{DocumentComments, DocumentMarks, ExportImages};
use common::types::{EntityId, ROOT_ENTITY_ID};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub trait ExportOdtUnitOfWorkFactoryTrait: Send + Sync {
    fn create(&self) -> Box<dyn ExportOdtUnitOfWorkTrait>;
}

#[macros::uow_action(entity = "Root", action = "GetRO", thread_safe = true)]
#[macros::uow_action(entity = "Root", action = "GetRelationshipRO", thread_safe = true)]
#[macros::uow_action(entity = "Document", action = "GetRO", thread_safe = true)]
#[macros::uow_action(entity = "Document", action = "GetRelationshipRO", thread_safe = true)]
#[macros::uow_action(entity = "Frame", action = "GetRO", thread_safe = true)]
#[macros::uow_action(entity = "Frame", action = "GetRelationshipRO", thread_safe = true)]
#[macros::uow_action(entity = "Block", action = "GetRO", thread_safe = true)]
#[macros::uow_action(entity = "Block", action = "GetMultiRO", thread_safe = true)]
#[macros::uow_action(entity = "Block", action = "GetRelationshipRO", thread_safe = true)]
#[macros::uow_action(entity = "List", action = "GetRO", thread_safe = true)]
#[macros::uow_action(entity = "Table", action = "GetRO", thread_safe = true)]
#[macros::uow_action(entity = "Table", action = "GetRelationshipRO", thread_safe = true)]
#[macros::uow_action(entity = "TableCell", action = "GetMultiRO", thread_safe = true)]
pub trait ExportOdtUnitOfWorkTrait: QueryUnitOfWork + Send + Sync {}

/// One unit-step of left indentation for blockquote/list nesting, re-exported at the value
/// `crate::odt_render::INDENT_STEP_PT` already fixes (36pt = 0.5in), so every quote-depth
/// computation in this module goes through one constant.
const INDENT_STEP_PT: f64 = odt_render::INDENT_STEP_PT;

/// Each note's body, pre-rendered to ODF XML (the concatenated `<text:p>`/`<text:h>`/… fragments
/// its blocks produce), keyed by label — the ODF analog of `export_docx_uc::NoteParagraphs`.
/// Unlike DOCX (which defers a footnote's body into a separate part), ODF's `<text:note-body>`
/// sits inline at the citation point, but the *ordering* problem is the same one DOCX has: the
/// body has to be in hand by the time a citation is rendered, so it is pre-rendered first — see
/// this module's doc comment for why a footnote reference found *while* pre-rendering another
/// note's body is never allowed to become a second real `<text:note>`.
type NoteBodies = HashMap<String, String>;

/// What a citation of `label` prints and whether this is its first appearance in the whole main
/// walk — the ODF analog of `export_docx_uc::FootnoteRefState`, plus its own monotonic
/// `text:id` counter (ODF numbers notes by an explicit id attribute, not by tree position the
/// way OOXML's separate-part-with-a-collector-pass does).
struct FootnoteRefState<'a> {
    numbers: &'a crate::footnotes::Footnotes,
    emitted: RefCell<HashSet<String>>,
    next_id: Cell<usize>,
}

impl<'a> FootnoteRefState<'a> {
    fn new(numbers: &'a crate::footnotes::Footnotes) -> Self {
        FootnoteRefState {
            numbers,
            emitted: RefCell::new(HashSet::new()),
            next_id: Cell::new(1),
        }
    }

    fn take_id(&self) -> usize {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        id
    }
}

/// Everything the inline-run/block builders need that stays constant across one whole render
/// pass (either the main walk, or one note body's pre-render), bundled so `render_frame_content`/
/// `render_block`/`build_run` don't each carry five more parameters on top of the ones that
/// genuinely vary per call (`styles`, `out`, the recursion-only `quote_depth`/`semantic`).
struct WalkCtx<'a> {
    /// Pre-rendered note bodies. Empty during the note-body pre-render pass itself (see
    /// `NoteBodies`'s doc comment) and populated for the main walk.
    notes: &'a NoteBodies,
    footnote_state: &'a FootnoteRefState<'a>,
    /// `true` while pre-rendering a note's own body — see this module's doc comment. Forces
    /// every footnote reference `build_run` meets to take the bare-superscript-marker branch,
    /// never the real-`<text:note>` branch, so a `<text:note>` can never end up nested inside
    /// another `<text:note-body>`.
    inside_note_body: bool,
    image_hrefs: &'a BTreeMap<String, String>,
    images: &'a ExportImages,
    /// `draw:name` must be unique per frame in the document; the image's own `src` is not
    /// (the same picture can be embedded twice). A shared counter, not a map, because the
    /// value only needs to be unique, never looked up again.
    image_seq: Cell<usize>,
}

// ── Comment ranges (M-T2b) ───────────────────────────────────────────────────────────
//
// See this module's own doc comment for the encoding this section produces. The shape mirrors
// `export_docx_uc`'s three-part machinery (`prepare_comments`, `CommentEmitState`, the run
// splitter), but simplified by one whole layer: DOCX resolves everything into `docx_rs` builder
// calls that only *become* XML once `Docx::build()` runs, so its `PreparedSpan` carries a
// half-built `docx_rs::Comment` and its `InlineHost` trait exists to target either of two
// distinct builder types (`Paragraph`/`Hyperlink`). This writer has no builder at all — every
// other part of it already assembles `content.xml`'s body as one `String` — so a "prepared"
// comment here is simply the *exact* `<office:annotation>…</office:annotation>` XML this thread
// will splice in, computed once, and "applying a marker" is nothing more than
// `String::push_str`. One function (`append_piece`) suffices where DOCX needed a host trait.

/// One anchored span this writer has to place: a comment, a reply, or a round-trip
/// [mark](common::parser_tools::DocumentMark). The ODF analog of `export_docx_uc::PreparedSpan`
/// — but unlike that type, `open_xml` is not assembled later by a builder: it already **is** the
/// finished element (for a comment: author, date, resolved flag, uid, parent link and rendered
/// body all resolved up front by [`prepare_spans`]), spliced in verbatim the instant the span's
/// range starts.
///
/// The three kinds differ only in the two strings they splice and in whether failing to place
/// them is an error, which is why one type covers all of them: the block windowing, the
/// per-piece marker resolution and the run splitting are all pure range arithmetic and could not
/// care what element the boundary eventually spells.
struct PreparedSpan {
    /// Sequential id, scoped to one export. Purely internal bookkeeping for
    /// [`CommentEmitState`]'s started/ended sets — never written to the file itself (`name` is
    /// what the file spells); the ODF analog of `docx-rs`'s numeric comment id, minted the same
    /// way (root, then each reply, in document order).
    id: usize,
    /// The uid this comment/reply carries — `DocumentComment::uid` for the root,
    /// `CommentReply::uid` for a reply, and the bookmark's own name for a mark. Reported by
    /// [`CommentEmitState::ensure_all_anchored`] when a comment never finds a home.
    uid: String,
    /// `[start, end)` in the document's addressable character space. For a comment thread this
    /// is shared by every reply in it (see `DocumentComment`'s own doc comment for why a reply
    /// has no range of its own); for a mark it is the mark's own span, possibly empty.
    start: u32,
    end: u32,
    /// Spliced in where the span starts. A comment's complete
    /// `<office:annotation …>…</office:annotation>`; a range mark's `<text:bookmark-start/>`; a
    /// point mark's entire `<text:bookmark/>`, which has no second half.
    open_xml: String,
    /// Spliced in where the span ends. Empty for a point mark — whose `end` equals its `start`,
    /// so `window_for_block`'s strictly-greater `ends` test never selects it and this is never
    /// reached. Precomputed rather than formatted at the boundary so the emit path is a splice
    /// and nothing else, for every kind alike.
    close_xml: String,
    /// Whether failing to anchor this span fails the export.
    ///
    /// True for comments: a note the caller asked to be written that silently is not in the file
    /// is data loss, and [`CommentEmitState::ensure_all_anchored`] exists to make it loud.
    ///
    /// False for marks: a mark is an aid to reading the file *back*, not content. A row whose
    /// mark could not be placed (its position fell in a footnote body, a fenced code block or
    /// table content — the regions this writer cannot anchor into) is still fully exported, and
    /// re-import falls back to matching it by type and title. Refusing to write the manuscript
    /// over that would trade a complete export for a convenience.
    required: bool,
}

/// FNV-1a (32-bit) is not needed here the way `export_docx_uc::fnv1a32` is (that hash derives a
/// `docx-rs` paragraph id forced into a small, collision-avoidant id space docx-rs itself hands
/// out sequentially) — ODF's `office:name` has no competing allocator to collide with, so a
/// plain, human-legible sequential name is simpler and just as unique. `__Comment__` mirrors the
/// double-underscore convention LibreOffice's own writer uses for its generated range names
/// (e.g. `__Fieldmark__0`), so a name this writer mints reads as what it is to anyone who has
/// looked at real LibreOffice output before.
fn comment_range_name(id: usize) -> String {
    format!("__Comment__{id}")
}

/// Resolve every comment thread in `comments` into its flat [`PreparedSpan`] list, in
/// document order (root immediately followed by its own replies, in the order they were
/// authored) — the ODF analog of `export_docx_uc::prepare_comments`. Takes `styles` because,
/// unlike DOCX's comment body (built directly as `docx_rs::Run`s with no shared style table), a
/// bold or italic word inside a comment body needs the same interned automatic character style
/// every other bold/italic run in this document uses — see [`render_comment_body_odt`].
fn prepare_spans(
    comments: &DocumentComments,
    marks: &DocumentMarks,
    styles: &mut OdtStyleSheet,
) -> Result<Vec<PreparedSpan>> {
    let mut out = prepare_comments(comments, styles);
    // Marks continue the same id space rather than starting a second one: the started/ended
    // sets in `CommentEmitState` are keyed by id alone, and two spans sharing an id would mark
    // each other as placed.
    let first_mark_id = out.len() + 1;
    out.extend(prepare_marks(marks, first_mark_id)?);
    Ok(out)
}

/// Resolve every round-trip mark into a [`PreparedSpan`].
///
/// Validates first, and refuses the whole export on a bad name rather than dropping the offender:
/// these names are minted by the host from its own identifiers, so an invalid one is a bug in the
/// caller, and a silently missing mark would surface much later as a returning file that
/// mysteriously fails to match — the hardest possible place to notice it. See
/// [`DocumentMark::validate`](common::parser_tools::DocumentMark::validate).
fn prepare_marks(marks: &DocumentMarks, first_id: usize) -> Result<Vec<PreparedSpan>> {
    marks
        .validate()
        .map_err(|e| anyhow!("invalid round-trip mark(s): {e}"))?;

    Ok(marks
        .in_document_order()
        .into_iter()
        .enumerate()
        .map(|(i, m)| {
            let escaped = odt_render::xml_escape(&m.name);
            // A point mark is one self-closing `<text:bookmark>`; a range mark is a
            // start/end pair. ODF spells the name on both halves of a pair, unlike OOXML,
            // which names only the start and closes by numeric id.
            let (open_xml, close_xml) = if m.is_point() {
                (
                    format!("<text:bookmark text:name=\"{escaped}\"/>"),
                    String::new(),
                )
            } else {
                (
                    format!("<text:bookmark-start text:name=\"{escaped}\"/>"),
                    format!("<text:bookmark-end text:name=\"{escaped}\"/>"),
                )
            };
            PreparedSpan {
                id: first_id + i,
                uid: m.name.clone(),
                start: m.start,
                end: m.end,
                open_xml,
                close_xml,
                required: false,
            }
        })
        .collect())
}

fn prepare_comments(comments: &DocumentComments, styles: &mut OdtStyleSheet) -> Vec<PreparedSpan> {
    let mut out = Vec::new();
    let mut next_id: usize = 1;

    for c in comments.in_document_order() {
        let root_id = next_id;
        next_id += 1;
        let root_name = comment_range_name(root_id);
        let body_xml = render_comment_body_odt(&c.body, styles);
        let open_xml = annotation_open_xml(
            &root_name, &c.uid, &c.author, &c.date, c.resolved, None, &body_xml,
        );
        out.push(PreparedSpan {
            id: root_id,
            uid: c.uid.clone(),
            start: c.start,
            end: c.end,
            open_xml,
            close_xml: annotation_close_xml(&root_name),
            required: true,
        });

        for reply in &c.replies {
            let reply_id = next_id;
            next_id += 1;
            let reply_name = comment_range_name(reply_id);
            let reply_body_xml = render_comment_body_odt(&reply.body, styles);
            let reply_open_xml = annotation_open_xml(
                &reply_name,
                &reply.uid,
                &reply.author,
                &reply.date,
                // A reply carries no resolved state of its own — only the thread it belongs to
                // (`DocumentComment::resolved`) does, exactly as `export_docx_uc::prepare_comments`
                // documents for its own `PreparedSpan::resolved`.
                false,
                Some(&root_name),
                &reply_body_xml,
            );
            out.push(PreparedSpan {
                id: reply_id,
                uid: reply.uid.clone(),
                start: c.start,
                end: c.end,
                open_xml: reply_open_xml,
                close_xml: annotation_close_xml(&reply_name),
                required: true,
            });
        }
    }
    out
}

/// Build one `<office:annotation>` element: `office:name` (the annotation-start/-end pairing
/// key), `skrb:uid` (this writer's own private uid carrier — see this module's doc comment for
/// why it is not `office:name`), `loext:resolved`, an optional `loext:parent-name` (a reply
/// only), `dc:creator`/`dc:date`, and finally `body_xml` — already-rendered `<text:p>` elements
/// from [`render_comment_body_odt`].
/// The `<office:annotation-end>` that closes an annotation opened under `name`. ODF pairs the two
/// halves by `office:name`, so this is the only thing the close tag needs.
fn annotation_close_xml(name: &str) -> String {
    format!(
        "<office:annotation-end office:name=\"{}\"/>",
        odt_render::xml_escape(name)
    )
}

fn annotation_open_xml(
    name: &str,
    uid: &str,
    author: &str,
    date: &str,
    resolved: bool,
    parent_name: Option<&str>,
    body_xml: &str,
) -> String {
    let mut attrs = format!(
        "office:name=\"{}\" skrb:uid=\"{}\" loext:resolved=\"{}\"",
        odt_render::xml_escape(name),
        odt_render::xml_escape(uid),
        if resolved { "true" } else { "false" },
    );
    if let Some(parent) = parent_name {
        attrs.push_str(&format!(
            " loext:parent-name=\"{}\"",
            odt_render::xml_escape(parent)
        ));
    }
    format!(
        "<office:annotation {attrs}><dc:creator>{}</dc:creator><dc:date>{}</dc:date>{body_xml}</office:annotation>",
        odt_render::xml_escape(author),
        odt_render::xml_escape(date),
    )
}

/// Render a comment or reply body's Djot source into a sequence of real `<text:p>` elements —
/// the ODF analog of `export_docx_uc::render_comment_body`, with one deliberate divergence: that
/// function collapses every body to exactly **one** `docx_rs::Paragraph` because of a `docx-rs`
/// 0.4.22 auto-collector bug (see its own doc comment) that duplicates the whole `<w:comment>`
/// once per body paragraph it finds. Nothing here goes through an auto-collecting builder —
/// `<office:annotation>` simply holds however many `<text:p>` children it is given — so a body
/// with several Djot paragraphs becomes several real `<text:p>` elements, each independently
/// legible to any ODF reader, LibreOffice included, rather than one paragraph full of
/// `<text:line-break/>`s standing in for what should be real paragraph breaks.
///
/// Formatting support is deliberately narrow, matching `render_comment_body`'s own scope: bold,
/// italic, underline, strikethrough, and paragraph/line breaks — plain text for everything else
/// (links, images, lists, tables, footnotes, code spans). A margin note is short annotation
/// prose, not manuscript content; nothing here panics or drops text on an unsupported construct,
/// it just renders as plain text, so an unusual comment body degrades gracefully instead of
/// failing the export.
///
/// A Djot soft line break (an ordinary wrapped line inside one source paragraph) becomes a
/// space, and a hard break (`jotdown::Event::Hardbreak`, Djot's backslash-newline) becomes a
/// real `<text:line-break/>` — `export_docx_uc::render_comment_body` does not distinguish either
/// event at all (both fall through its catch-all `_ => {}` arm), which is a gap that function's
/// own one-paragraph-per-body constraint mostly hides; this writer has no reason to repeat it.
fn render_comment_body_odt(djot: &str, styles: &mut OdtStyleSheet) -> String {
    use jotdown::{Container as C, Event as E, Parser};

    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut underline = false;
    let mut strikeout = false;
    let mut buffer = String::new();
    let mut buf_bold = false;
    let mut buf_italic = false;
    let mut buf_underline = false;
    let mut buf_strikeout = false;

    macro_rules! mark_flags {
        () => {
            if buffer.is_empty() {
                buf_bold = bold;
                buf_italic = italic;
                buf_underline = underline;
                buf_strikeout = strikeout;
            }
        };
    }
    macro_rules! flush {
        () => {
            if !buffer.is_empty() {
                let attrs = character_style_attrs_from_flags(
                    buf_bold,
                    buf_italic,
                    buf_underline,
                    buf_strikeout,
                );
                let encoded = odt_render::encode_run_text(&buffer);
                if attrs.is_empty() {
                    current.push_str(&encoded);
                } else {
                    let style = styles.text_style(attrs.trim());
                    current.push_str(&format!(
                        "<text:span text:style-name=\"{style}\">{encoded}</text:span>"
                    ));
                }
                buffer.clear();
            }
        };
    }

    for event in Parser::new(djot) {
        match event {
            E::Start(C::Paragraph, _) | E::Start(C::Heading { .. }, _) => {}
            E::End(C::Paragraph) | E::End(C::Heading { .. }) => {
                flush!();
                paragraphs.push(std::mem::take(&mut current));
            }
            E::Start(C::Strong, _) => {
                flush!();
                bold = true;
            }
            E::End(C::Strong) => {
                flush!();
                bold = false;
            }
            E::Start(C::Emphasis, _) => {
                flush!();
                italic = true;
            }
            E::End(C::Emphasis) => {
                flush!();
                italic = false;
            }
            E::Start(C::Insert, _) => {
                flush!();
                underline = true;
            }
            E::End(C::Insert) => {
                flush!();
                underline = false;
            }
            E::Start(C::Delete, _) => {
                flush!();
                strikeout = true;
            }
            E::End(C::Delete) => {
                flush!();
                strikeout = false;
            }
            E::Str(s) => {
                mark_flags!();
                buffer.push_str(s.as_ref());
            }
            E::Softbreak => {
                mark_flags!();
                buffer.push(' ');
            }
            E::Hardbreak => {
                flush!();
                current.push_str("<text:line-break/>");
            }
            E::LeftSingleQuote => {
                mark_flags!();
                buffer.push('\u{2018}');
            }
            E::RightSingleQuote => {
                mark_flags!();
                buffer.push('\u{2019}');
            }
            E::LeftDoubleQuote => {
                mark_flags!();
                buffer.push('\u{201C}');
            }
            E::RightDoubleQuote => {
                mark_flags!();
                buffer.push('\u{201D}');
            }
            E::Ellipsis => {
                mark_flags!();
                buffer.push('\u{2026}');
            }
            E::EnDash => {
                mark_flags!();
                buffer.push('\u{2013}');
            }
            E::EmDash => {
                mark_flags!();
                buffer.push('\u{2014}');
            }
            E::NonBreakingSpace => {
                mark_flags!();
                buffer.push('\u{00A0}');
            }
            // Exotic djot constructs (images, tables, footnotes, links, code blocks, ...)
            // contribute no text of their own here — see this function's doc comment.
            _ => {}
        }
    }
    flush!();
    if !current.is_empty() {
        paragraphs.push(std::mem::take(&mut current));
    }

    // An all-blank body (an editor who left a comment box with nothing typed in it, or whose
    // Djot parsed to no visible block at all) becomes zero `<text:p>` children — a valid, if
    // empty, `<office:annotation>` — rather than a stray empty paragraph nobody asked for.
    paragraphs
        .iter()
        .map(|p| format!("<text:p>{p}</text:p>"))
        .collect()
}

/// The `<style:text-properties>` attribute fragment for bold/italic/underline/strikethrough,
/// shared by [`text_run_with_format`] (formatting read off an `InlineSegment`) and
/// [`render_comment_body_odt`] (formatting tracked as plain `bool`s while walking Djot events,
/// which carries no `InlineSegment` to read from) — one definition of what each of the four
/// switches spells in ODF, rather than two copies that could drift.
fn character_style_attrs_from_flags(
    bold: bool,
    italic: bool,
    underline: bool,
    strikeout: bool,
) -> String {
    let mut attrs = String::new();
    if bold {
        attrs.push_str(" fo:font-weight=\"bold\" style:font-weight-complex=\"bold\"");
    }
    if italic {
        attrs.push_str(" fo:font-style=\"italic\" style:font-style-complex=\"italic\"");
    }
    if underline {
        attrs.push_str(
            " style:text-underline-style=\"solid\" style:text-underline-width=\"auto\" \
             style:text-underline-color=\"font-color\"",
        );
    }
    if strikeout {
        attrs.push_str(" style:text-line-through-style=\"solid\"");
    }
    attrs
}

/// One comment boundary event, resolved to a local character index inside the one inline piece
/// it falls in — see `markers_for_piece`. The ODF analog of `export_docx_uc::Marker`.
enum Marker<'a> {
    Start(&'a PreparedSpan),
    End(&'a PreparedSpan),
}

/// The comments open or closing somewhere inside one block — the ODF analog of
/// `export_docx_uc::BlockCommentWindow`.
struct BlockCommentWindow<'a> {
    starts: Vec<&'a PreparedSpan>,
    ends: Vec<&'a PreparedSpan>,
}

/// The comments whose range opens or closes somewhere inside one block, and the bookkeeping
/// needed to prove every prepared comment found a home by the end of the document walk — the
/// ODF analog of `export_docx_uc::CommentEmitState`, built once per export and threaded by
/// shared reference through `render_frame_content`/`render_block`/`add_inline_content`. `None`
/// at call sites outside the document's addressable text — see this module's own doc comment for
/// which those are.
struct CommentEmitState<'a> {
    prepared: &'a [PreparedSpan],
    started: RefCell<HashSet<usize>>,
    ended: RefCell<HashSet<usize>>,
}

impl<'a> CommentEmitState<'a> {
    fn new(prepared: &'a [PreparedSpan]) -> Self {
        Self {
            prepared,
            started: RefCell::new(HashSet::new()),
            ended: RefCell::new(HashSet::new()),
        }
    }

    /// Comments whose start, respectively end, falls inside `[block_start, block_end)` — or,
    /// for a genuinely empty block (`block_start == block_end`, e.g. a blank paragraph),
    /// comments collapsed to exactly that point. Blocks are visited in non-decreasing
    /// document-position order by every call site that passes `Some(state)`, so a thread
    /// spanning several blocks (its start block and end block differ) opens in the first and
    /// closes in the last with nothing to do in between — no extra state needed beyond this
    /// per-block filter. Identical logic to `export_docx_uc::CommentEmitState::window_for_block`.
    fn window_for_block(&self, block_start: u32, block_end: u32) -> BlockCommentWindow<'a> {
        let empty = block_start == block_end;
        let mut starts: Vec<&'a PreparedSpan> = self
            .prepared
            .iter()
            .filter(|c| {
                (block_start <= c.start && c.start < block_end) || (empty && c.start == block_start)
            })
            .collect();
        starts.sort_by_key(|c| (c.start, c.id));
        let mut ends: Vec<&'a PreparedSpan> = self
            .prepared
            .iter()
            .filter(|c| {
                (block_start < c.end && c.end <= block_end) || (empty && c.end == block_end)
            })
            .collect();
        ends.sort_by_key(|c| (c.end, c.id));
        BlockCommentWindow { starts, ends }
    }

    fn mark_started(&self, id: usize) {
        self.started.borrow_mut().insert(id);
    }

    fn mark_ended(&self, id: usize) {
        self.ended.borrow_mut().insert(id);
    }

    /// Every prepared comment must have been both opened and closed exactly once by the time the
    /// whole document walk finishes, or its range never intersected any block this writer
    /// visited — out of bounds, or targeting a fenced code block, a scene-break/rule paragraph, a
    /// footnote body, or table-cell content, all deliberately outside the range this writer can
    /// anchor into (see this module's own doc comment) — and it would otherwise vanish from the
    /// output with no trace at all. Surfaced here as one loud, actionable `Err` naming every
    /// orphan, rather than an `.odt` that silently opens with fewer comments than the caller
    /// asked for. Identical contract to `export_docx_uc::CommentEmitState::ensure_all_anchored`.
    fn ensure_all_anchored(&self) -> Result<()> {
        let started = self.started.borrow();
        let ended = self.ended.borrow();
        // `required` only — a round-trip mark that found no home degrades re-import to matching
        // by type and title, which is a designed fallback, and is not worth refusing to write
        // the manuscript over. See `PreparedSpan::required`.
        let missing: Vec<String> = self
            .prepared
            .iter()
            .filter(|c| c.required && (!started.contains(&c.id) || !ended.contains(&c.id)))
            .map(|c| format!("{} [{}, {})", c.uid, c.start, c.end))
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(
                "{} comment(s) could not be anchored to any exported text (range outside the \
                 document, or targeting a fenced code block, a scene-break/rule paragraph, a \
                 footnote body, or table-cell content, none of which carry comment ranges): {}",
                missing.len(),
                missing.join(", ")
            ))
        }
    }
}

/// Comment markers landing inside one inline piece spanning `[piece_start, piece_end)`,
/// resolved to a local index (`0..=piece_end-piece_start`) into that piece's own content — the
/// same offset a run's text has to be split at (via `.chars()`, never a byte index). Identical
/// logic and identical tie-break to `export_docx_uc::markers_for_piece` — see that function's
/// own doc comment for why `End` sorts before `Start` at an exact tie.
fn markers_for_piece<'a>(
    window: &BlockCommentWindow<'a>,
    piece_start: u32,
    piece_end: u32,
) -> Vec<(u32, Marker<'a>)> {
    let mut out: Vec<(u32, Marker<'a>)> = Vec::new();
    for &c in &window.starts {
        if piece_start <= c.start && c.start < piece_end {
            out.push((c.start - piece_start, Marker::Start(c)));
        }
    }
    for &c in &window.ends {
        if piece_start < c.end && c.end <= piece_end {
            out.push((c.end - piece_start, Marker::End(c)));
        }
    }
    // Three ranks, not two. `End` before `Start` at an exact tie is the original rule (see
    // `export_docx_uc::markers_for_piece`); the split within `Start` is what puts a row's
    // point mark at the very front of its paragraph rather than after an annotation that
    // happens to open on the same character. A mark whose whole purpose is to say "this
    // paragraph begins row X" reads wrong sitting inside a comment's range, and a reader
    // resolving the row it belongs to would have to look past the annotation to find it.
    out.sort_by_key(|(idx, m)| {
        let (kind_rank, id) = match m {
            Marker::End(c) => (0u8, c.id),
            Marker::Start(c) if !c.required => (1u8, c.id),
            Marker::Start(c) => (2u8, c.id),
        };
        (*idx, kind_rank, id)
    });
    out
}

/// Push one comment boundary's XML: a `Start` splices in the comment's whole precomputed
/// `open_xml` (see `PreparedSpan`'s doc comment for why the body travels with the start, not
/// separately); an `End` writes the matching close tag. No `InlineHost`-style trait is needed
/// the way `export_docx_uc::apply_marker` needs one — every call site here is already building
/// into a plain `String`, whether that string is the paragraph's own body or a `<text:a>` group's
/// inner buffer (see `append_piece`).
fn apply_marker(out: &mut String, marker: &Marker<'_>, state: &CommentEmitState<'_>) {
    match marker {
        Marker::Start(c) => {
            state.mark_started(c.id);
            out.push_str(&c.open_xml);
        }
        Marker::End(c) => {
            state.mark_ended(c.id);
            out.push_str(&c.close_xml);
        }
    }
}

pub struct ExportOdtUseCase {
    uow_factory: Box<dyn ExportOdtUnitOfWorkFactoryTrait>,
    dto: ExportOdtDto,
}

impl ExportOdtUseCase {
    pub fn new(uow_factory: Box<dyn ExportOdtUnitOfWorkFactoryTrait>, dto: &ExportOdtDto) -> Self {
        ExportOdtUseCase {
            uow_factory,
            dto: dto.clone(),
        }
    }
}

impl LongOperation for ExportOdtUseCase {
    type Output = ExportOdtResultDto;

    fn execute(
        &self,
        progress_callback: Box<dyn Fn(common::long_operation::OperationProgress) + Send>,
        cancel_flag: Arc<AtomicBool>,
    ) -> Result<Self::Output> {
        let output_path = std::path::Path::new(&self.dto.output_path);
        if let Some(parent) = output_path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            return Err(anyhow!(
                "Output directory does not exist: '{}'",
                parent.display()
            ));
        }

        progress_callback(common::long_operation::OperationProgress::new(
            0.0,
            Some("Starting ODT export...".to_string()),
        ));

        let uow = self.uow_factory.create();
        uow.begin_transaction()?;
        let build_result = self.build_odt(
            &*uow,
            progress_callback.as_ref(),
            Some(cancel_flag.as_ref()),
        );
        uow.end_transaction()?;

        let (bytes, paragraph_count) = build_result?;

        progress_callback(common::long_operation::OperationProgress::new(
            90.0,
            Some("Writing ODT file...".to_string()),
        ));

        std::fs::write(&self.dto.output_path, &bytes).map_err(|e| {
            anyhow!(
                "Failed to write output file '{}': {}",
                self.dto.output_path,
                e
            )
        })?;

        progress_callback(common::long_operation::OperationProgress::new(
            100.0,
            Some("completed".to_string()),
        ));

        Ok(ExportOdtResultDto {
            file_path: self.dto.output_path.clone(),
            paragraph_count,
        })
    }
}

impl ExportOdtUseCase {
    /// Build the packaged `.odt` bytes without any file I/O, using a no-op progress callback and
    /// no cancellation, together with the paragraph count. Intended for callers (notably tests)
    /// that want to inspect the produced package directly — the ODF analog of
    /// `export_docx_uc::build_document`/`export_epub_uc::build_document`.
    pub(crate) fn build_document(&self) -> Result<(Vec<u8>, i64)> {
        let uow = self.uow_factory.create();
        uow.begin_transaction()?;
        let result = self.build_odt(&*uow, &|_progress| {}, None);
        uow.end_transaction()?;
        result
    }

    /// Assemble the complete `.odt` package. Mirrors `export_docx_uc::build_docx`'s walk
    /// (Root→Document→Frame→Block, cell-frame skip set, note-definition skip, footnote
    /// pre-render pass, then the main walk) but accumulates one `String` of `content.xml` body
    /// XML plus an [`OdtStyleSheet`] instead of a `docx_rs::Docx` builder, and packages the
    /// result via [`odt_render::package_odt`] at the end instead of a final `.build()`.
    pub(crate) fn build_odt(
        &self,
        uow: &dyn ExportOdtUnitOfWorkTrait,
        progress_callback: &dyn Fn(common::long_operation::OperationProgress),
        cancel_flag: Option<&AtomicBool>,
    ) -> Result<(Vec<u8>, i64)> {
        let root = uow
            .get_root(&ROOT_ENTITY_ID)?
            .ok_or_else(|| anyhow!("Root entity not found"))?;
        let doc_ids = uow.get_root_relationship(
            &root.id,
            &common::direct_access::root::RootRelationshipField::Document,
        )?;
        let doc_id = *doc_ids
            .first()
            .ok_or_else(|| anyhow!("Root has no associated Document"))?;

        let frame_ids = uow.get_document_relationship(
            &doc_id,
            &common::direct_access::document::DocumentRelationshipField::Frames,
        )?;
        let table_ids = uow.get_document_relationship(
            &doc_id,
            &common::direct_access::document::DocumentRelationshipField::Tables,
        )?;
        let mut cell_frame_ids: HashSet<EntityId> = HashSet::new();
        for tid in &table_ids {
            let cell_ids = uow.get_table_relationship(
                tid,
                &common::direct_access::table::TableRelationshipField::Cells,
            )?;
            let cells_opt = uow.get_table_cell_multi(&cell_ids)?;
            for cell in cells_opt.into_iter().flatten() {
                if let Some(cf_id) = cell.cell_frame {
                    cell_frame_ids.insert(cf_id);
                }
            }
        }

        progress_callback(common::long_operation::OperationProgress::new(
            10.0,
            Some("Walking document tree...".to_string()),
        ));

        let mut styles = OdtStyleSheet::default();
        let notes = crate::footnotes::Footnotes::build(&uow.store());
        let image_hrefs = build_image_href_map(&self.dto.options.images);

        // Resolved once, up front, so every render call below shares the exact same id/name
        // assignment — mirrors `export_docx_uc::build_docx`'s identical `prepare_comments` call.
        // `None` when the caller supplied no comments at all, so a plain export (no `comments`
        // option set) never pays for the per-block window computation. Needs `&mut styles`
        // because a bold/italic run inside a comment body interns the same automatic character
        // style any other bold/italic run in the document does — see
        // `render_comment_body_odt`'s doc comment.
        let prepared_spans = prepare_spans(
            &self.dto.options.comments,
            &self.dto.options.marks,
            &mut styles,
        )?;
        let comment_state: Option<CommentEmitState<'_>> = if prepared_spans.is_empty() {
            None
        } else {
            Some(CommentEmitState::new(&prepared_spans))
        };

        // Render every note's body first, while `note_bodies` is still empty — mirrors
        // `export_docx_uc::build_docx`'s identical pre-pass, and for the identical reason: a
        // note that itself cites another note must not recurse. See this module's doc comment
        // for why the ODF writer additionally forces every reference met here into the
        // bare-marker branch rather than a real nested `<text:note>`.
        //
        // `comments` is passed as `None` here regardless of `comment_state`: a footnote body is
        // pre-rendered before the main walk even starts, over content
        // `to_addressable_text` never descends into, so a `DocumentComment`'s character offset
        // can never legitimately resolve inside one — see this module's own doc comment.
        let note_bodies: NoteBodies = {
            let mut built: NoteBodies = HashMap::new();
            for (_, label, frame_id) in notes.in_print_order() {
                let Some(note_frame) = uow.get_frame(&frame_id)? else {
                    continue;
                };
                let empty_notes = NoteBodies::new();
                let body_footnote_state = FootnoteRefState::new(&notes);
                let ctx = WalkCtx {
                    notes: &empty_notes,
                    footnote_state: &body_footnote_state,
                    inside_note_body: true,
                    image_hrefs: &image_hrefs,
                    images: &self.dto.options.images,
                    image_seq: Cell::new(0),
                };
                let mut body = String::new();
                let mut counter = 0i64;
                self.render_frame_content(
                    uow,
                    &note_frame,
                    &cell_frame_ids,
                    0,
                    None,
                    &mut styles,
                    &ctx,
                    None,
                    &mut body,
                    &mut counter,
                    None,
                )?;
                built.insert(label, body);
            }
            built
        };

        let footnote_state = FootnoteRefState::new(&notes);
        let ctx = WalkCtx {
            notes: &note_bodies,
            footnote_state: &footnote_state,
            inside_note_body: false,
            image_hrefs: &image_hrefs,
            images: &self.dto.options.images,
            image_seq: Cell::new(0),
        };

        let mut body_xml = String::new();
        let mut paragraph_count = 0i64;

        let total_frames = frame_ids.len().max(1);
        for (frame_idx, frame_id) in frame_ids.iter().enumerate() {
            check_cancelled(cancel_flag)?;
            if cell_frame_ids.contains(frame_id) {
                continue;
            }
            let Some(frame) = uow.get_frame(frame_id)? else {
                continue;
            };
            if notes.is_definition(frame.id) {
                continue;
            }
            if frame.parent_frame.is_some() {
                continue;
            }

            if let Some(table_id) = frame.table {
                let table_xml =
                    self.render_table_odt(uow, &table_id, &mut styles, &ctx, &mut paragraph_count)?;
                body_xml.push_str(&table_xml);
                continue;
            }

            self.render_frame_content(
                uow,
                &frame,
                &cell_frame_ids,
                0,
                None,
                &mut styles,
                &ctx,
                cancel_flag,
                &mut body_xml,
                &mut paragraph_count,
                comment_state.as_ref(),
            )?;

            let pct = 10.0 + (frame_idx as f32 / total_frames as f32) * 70.0;
            progress_callback(common::long_operation::OperationProgress::new(
                pct,
                Some(format!(
                    "Processing frame {}/{}",
                    frame_idx + 1,
                    total_frames
                )),
            ));
        }

        // Every prepared comment must have found a home somewhere in the walk just finished —
        // see `CommentEmitState::ensure_all_anchored`'s doc comment for what "must" buys here.
        if let Some(state) = &comment_state {
            state.ensure_all_anchored()?;
        }

        progress_callback(common::long_operation::OperationProgress::new(
            85.0,
            Some("Assembling document...".to_string()),
        ));

        let heading_styles = self.dto.options.resolved_heading_styles();
        let content_xml = odt_render::content_xml(&styles, &body_xml);
        let styles_xml = odt_render::styles_xml(&self.dto.options, &heading_styles);

        let mut packaged_images: Vec<(String, Vec<u8>, String)> = Vec::new();
        for (src, href) in &image_hrefs {
            if let Some(image) = self.dto.options.images.get(src) {
                packaged_images.push((href.clone(), image.bytes.clone(), image.mime_type.clone()));
            }
        }

        let bytes = odt_render::package_odt(&content_xml, &styles_xml, &packaged_images)?;
        Ok((bytes, paragraph_count))
    }

    /// Walk a frame's `child_order` (or, when empty, its blocks sorted by `document_position`),
    /// appending rendered XML to `out`. Mirrors `export_docx_uc::render_frame_content`'s shape,
    /// with one addition this format needs and DOCX doesn't: a [`ListStack`], scoped to this one
    /// call (i.e. to this one frame's flat content run — a sub-frame gets its own via its own
    /// recursive call), which is flushed before any non-list content and unconditionally at the
    /// end, so consecutive listed blocks land inside one real, possibly nested, `<text:list>`
    /// tree instead of a run of unrelated sibling paragraphs.
    ///
    /// `comments` is threaded straight through every recursive call — including into a
    /// blockquote sub-frame, which *is* part of the document's addressable text — but is never
    /// passed down into `render_table_odt`'s own cell walk (that call always hands its cells
    /// `None`; see this module's own doc comment for why table cells are out of scope for
    /// comment ranges). Mirrors `export_docx_uc::render_frame_content`'s identical contract.
    #[allow(clippy::too_many_arguments)]
    fn render_frame_content(
        &self,
        uow: &dyn ExportOdtUnitOfWorkTrait,
        frame: &Frame,
        cell_frame_ids: &HashSet<EntityId>,
        quote_depth: usize,
        semantic: Option<&SemanticRole>,
        styles: &mut OdtStyleSheet,
        ctx: &WalkCtx,
        cancel_flag: Option<&AtomicBool>,
        out: &mut String,
        counter: &mut i64,
        comments: Option<&CommentEmitState<'_>>,
    ) -> Result<()> {
        let mut list_stack = ListStack::default();

        if !frame.child_order.is_empty() {
            for &entry in &frame.child_order {
                check_cancelled(cancel_flag)?;
                if entry == 0 {
                    continue;
                }
                if entry > 0 {
                    let block_id = entry as EntityId;
                    if let Some(block) = uow.get_block(&block_id)? {
                        self.dispatch_block(
                            uow,
                            &block,
                            quote_depth,
                            semantic,
                            styles,
                            ctx,
                            &mut list_stack,
                            out,
                            counter,
                            comments,
                        )?;
                    }
                } else {
                    list_stack.flush(out);
                    let sub_frame_id = (-entry) as EntityId;
                    if cell_frame_ids.contains(&sub_frame_id) {
                        continue;
                    }
                    if let Some(sub_frame) = uow.get_frame(&sub_frame_id)? {
                        if let Some(table_id) = sub_frame.table {
                            let table_xml =
                                self.render_table_odt(uow, &table_id, styles, ctx, counter)?;
                            out.push_str(&table_xml);
                            continue;
                        }
                        let sub_depth = if sub_frame.fmt_is_blockquote == Some(true) {
                            quote_depth + 1
                        } else {
                            quote_depth
                        };
                        let sub_semantic = if sub_frame.fmt_is_blockquote == Some(true) {
                            sub_frame.fmt_semantic_role.as_ref()
                        } else {
                            semantic
                        };
                        self.render_frame_content(
                            uow,
                            &sub_frame,
                            cell_frame_ids,
                            sub_depth,
                            sub_semantic,
                            styles,
                            ctx,
                            cancel_flag,
                            out,
                            counter,
                            comments,
                        )?;
                    }
                }
            }
            list_stack.flush(out);
        } else {
            let block_ids = uow.get_frame_relationship(
                &frame.id,
                &common::direct_access::frame::FrameRelationshipField::Blocks,
            )?;
            if block_ids.is_empty() {
                return Ok(());
            }
            let blocks_opt = uow.get_block_multi(&block_ids)?;
            let mut blocks: Vec<Block> = blocks_opt.into_iter().flatten().collect();
            blocks.sort_by_key(|b| b.document_position);
            for block in &blocks {
                check_cancelled(cancel_flag)?;
                self.dispatch_block(
                    uow,
                    block,
                    quote_depth,
                    semantic,
                    styles,
                    ctx,
                    &mut list_stack,
                    out,
                    counter,
                    comments,
                )?;
            }
            list_stack.flush(out);
        }
        Ok(())
    }

    /// Render one block and route it: a standalone element flushes any open list and is appended
    /// directly; a list item is handed to `list_stack`, which decides on its own whether that
    /// continues the currently-open `<text:list>` or opens/closes one.
    #[allow(clippy::too_many_arguments)]
    fn dispatch_block(
        &self,
        uow: &dyn ExportOdtUnitOfWorkTrait,
        block: &Block,
        quote_depth: usize,
        semantic: Option<&SemanticRole>,
        styles: &mut OdtStyleSheet,
        ctx: &WalkCtx,
        list_stack: &mut ListStack,
        out: &mut String,
        counter: &mut i64,
        comments: Option<&CommentEmitState<'_>>,
    ) -> Result<()> {
        match self.render_block(uow, block, quote_depth, semantic, styles, ctx, comments)? {
            RenderedBlock::Standalone(xml) => {
                list_stack.flush(out);
                out.push_str(&xml);
            }
            RenderedBlock::ListItem {
                list_id,
                depth,
                style_name,
                inner,
            } => {
                list_stack.push(out, list_id, depth, style_name, inner);
            }
        }
        *counter += 1;
        Ok(())
    }

    /// Render a single block. Dispatch priority mirrors `export_docx_uc::render_block`: code
    /// block, then heading (which takes priority over list membership — a heading-tagged list
    /// item still renders as a heading, never as a list item), then list item, then plain
    /// paragraph (with the scene-break heuristic as one further branch inside "plain paragraph").
    ///
    /// `comments` is `Some` only from call sites reached while walking the document's
    /// addressable text — the main body walk and, recursively, its blockquote sub-frames. It is
    /// always `None` for a footnote body (rendered in a separate pre-pass, before the main walk
    /// starts, over content `to_addressable_text` never descends into) and for table-cell
    /// content (represented in the addressable text by the table's one sentinel character, never
    /// by the cells' own prose) — a `DocumentComment`'s character offset can therefore never
    /// legitimately resolve inside either, and this writer does not pretend otherwise. A code
    /// block and a scene-break/rule paragraph are also out of scope: neither one ever reaches
    /// `add_inline_content`, the only place a comment marker is placed — see this module's own
    /// doc comment. Mirrors `export_docx_uc::render_block`'s identical `comments` contract.
    #[allow(clippy::too_many_arguments)]
    fn render_block(
        &self,
        uow: &dyn ExportOdtUnitOfWorkTrait,
        block: &Block,
        quote_depth: usize,
        semantic: Option<&SemanticRole>,
        styles: &mut OdtStyleSheet,
        ctx: &WalkCtx,
        comments: Option<&CommentEmitState<'_>>,
    ) -> Result<RenderedBlock> {
        let block_text = block_content_via_store(block, &uow.store());
        let elements = inline_segments_for_block(&uow.store(), block.id, &block_text);
        // `elements` and `addressable` are built from the exact same `merge_runs_and_anchors`
        // pieces, one InlineSegment/AddressableInlinePiece per piece, in the same order — see
        // `addressable_inline_pieces_for_block`'s own doc comment. Zipping them is what lets
        // every downstream call reach a piece's char-space `[start, end)` without re-deriving it
        // from format-run byte offsets — exactly the bug class that accessor exists to close, and
        // the same zip `export_docx_uc::render_block` performs for the identical reason.
        let addressable = addressable_inline_pieces_for_block(&uow.store(), block, &block_text);
        debug_assert_eq!(
            elements.len(),
            addressable.len(),
            "inline_segments_for_block and addressable_inline_pieces_for_block must stay in \
             lockstep — both are views over the same merge_runs_and_anchors() pieces"
        );
        let pieces: Vec<(InlineSegment, u32, u32)> = elements
            .into_iter()
            .zip(addressable.iter())
            .map(|(elem, piece)| (elem, piece.start, piece.end))
            .collect();

        let quote_indent_pt = quote_depth as f64 * INDENT_STEP_PT;

        // --- Code block ------------------------------------------------------
        if block.fmt_is_code_block == Some(true) {
            let mut raw = String::new();
            for (elem, _, _) in &pieces {
                if let InlineContent::Text(t) = &elem.content {
                    raw.push_str(t);
                }
            }
            let attrs = if quote_indent_pt > 0.0 {
                format!("fo:margin-left=\"{}\"", odt_render::fmt_pt(quote_indent_pt))
            } else {
                String::new()
            };
            let style = styles.paragraph_style("Code_Block", &attrs, "");
            return Ok(RenderedBlock::Standalone(format!(
                "<text:p text:style-name=\"{style}\">{}</text:p>",
                odt_render::encode_run_text(&raw)
            )));
        }

        // The comments whose range opens or closes somewhere inside this one block — computed
        // once per block, then narrowed further per inline piece inside `add_inline_content`.
        // `None` when this call site is out of scope (see this function's doc comment) or the
        // export carries no comments at all.
        let comment_window = comments.map(|state| {
            // Same cast `addressable_inline_pieces_for_block` makes on this exact value, and for
            // the same reason: a document large enough to overflow `u32` chars would already
            // have overflowed the rope it lives in.
            let block_start = block_document_position(block, &uow.store()) as u32;
            let block_end = block_start + block_text.chars().count() as u32;
            (state, state.window_for_block(block_start, block_end))
        });

        // --- Resolve list membership ----------------------------------------
        let list_ids = uow.get_block_relationship(
            &block.id,
            &common::direct_access::block::BlockRelationshipField::List,
        )?;
        let list = match list_ids.first() {
            Some(list_id) => uow.get_list(list_id)?.map(|l| (*list_id, l)),
            None => None,
        };
        let is_task = matches!(
            block.fmt_marker,
            Some(MarkerType::Checked) | Some(MarkerType::Unchecked)
        );

        let common_attrs = common_para_attrs(block, quote_indent_pt);

        // --- Heading -----------------------------------------------------------
        if let Some(level) = block.fmt_heading_level {
            let level = level.clamp(1, 6);
            let mut attrs = common_attrs;
            if let Some(before) = block.fmt_top_margin.filter(|&t| t > 0) {
                attrs.push_str(&format!(
                    " fo:margin-top=\"{}\"",
                    odt_render::fmt_pt(odt_render::px_to_pt(before))
                ));
            }
            let style = styles.paragraph_style(&format!("Heading_{level}"), attrs.trim(), "");
            let inner = add_inline_content(
                &pieces,
                ctx,
                styles,
                comment_window
                    .as_ref()
                    .map(|(state, window)| (*state, window)),
            );
            return Ok(RenderedBlock::Standalone(format!(
                "<text:h text:style-name=\"{style}\" text:outline-level=\"{level}\">{inner}</text:h>"
            )));
        }

        // --- List item -----------------------------------------------------
        if let Some((list_id, list_entity)) = &list {
            let depth = list_entity.indent.clamp(0, 8);
            if is_task {
                // Task items carry a checkbox glyph instead of an auto-number, exactly mirroring
                // `export_docx_uc`: they never enter the `<text:list>` numbering system at all.
                let style = styles.paragraph_style("Standard", common_attrs.trim(), "");
                let glyph = if block.fmt_marker == Some(MarkerType::Checked) {
                    "\u{2612} " // ☒
                } else {
                    "\u{2610} " // ☐
                };
                let inner = add_inline_content(
                    &pieces,
                    ctx,
                    styles,
                    comment_window
                        .as_ref()
                        .map(|(state, window)| (*state, window)),
                );
                return Ok(RenderedBlock::Standalone(format!(
                    "<text:p text:style-name=\"{style}\">{}{inner}</text:p>",
                    odt_render::xml_escape(glyph)
                )));
            }
            let list_style_name =
                styles.list_style(*list_id, |name| odt_list_style_xml(name, list_entity));
            let para_style = styles.paragraph_style("Standard", common_attrs.trim(), "");
            let inner = add_inline_content(
                &pieces,
                ctx,
                styles,
                comment_window
                    .as_ref()
                    .map(|(state, window)| (*state, window)),
            );
            let item_body = format!("<text:p text:style-name=\"{para_style}\">{inner}</text:p>");
            return Ok(RenderedBlock::ListItem {
                list_id: *list_id,
                depth,
                style_name: list_style_name,
                inner: item_body,
            });
        }

        // --- Plain paragraph, including the scene-break/rule heuristic ---------
        //
        // Applied only outside a blockquote's semantic role (an epigraph's attribution line can
        // legitimately be short, symbol-heavy prose — e.g. a single em dash — and must never be
        // mistaken for a thematic break) and, structurally, only ever produces the bare `"Rule"`
        // reference — see this module's doc comment for why nothing may be layered on top of it.
        if semantic.is_none() && looks_like_rule_glyph(&pieces) {
            return Ok(RenderedBlock::Standalone(
                "<text:p text:style-name=\"Rule\"/>".to_string(),
            ));
        }

        let mut attrs = common_attrs;
        if let Some(top) = block.fmt_top_margin.filter(|&t| t > 0) {
            attrs.push_str(&format!(
                " fo:margin-top=\"{}\"",
                odt_render::fmt_pt(odt_render::px_to_pt(top))
            ));
        }
        let first_line = match block.fmt_text_indent {
            Some(ti) if ti > 0 => Some(odt_render::px_to_pt(ti)),
            Some(_) => None,
            None => self
                .dto
                .options
                .first_line_indent_twips
                .filter(|&f| f > 0)
                .map(odt_render::twips_to_pt),
        };
        if let Some(fl) = first_line {
            attrs.push_str(&format!(" fo:text-indent=\"{}\"", odt_render::fmt_pt(fl)));
        }
        if let Some(after) = self
            .dto
            .options
            .paragraph_spacing_after_twips
            .filter(|&a| a > 0)
        {
            attrs.push_str(&format!(
                " fo:margin-bottom=\"{}\"",
                odt_render::fmt_pt(odt_render::twips_to_pt(after))
            ));
        }
        if block.fmt_line_height.is_none()
            && let Some(ls) = self.dto.options.line_spacing_twips
        {
            let percent = (odt_render::twips_to_pt(ls) / 12.0 * 100.0).round() as i64;
            attrs.push_str(&format!(" fo:line-height=\"{percent}%\""));
        }
        let rtl = block.fmt_direction == Some(TextDirection::RightToLeft);
        if block.fmt_alignment.is_none() {
            let align = if self.dto.options.justify {
                Some("justify")
            } else if rtl {
                Some("right")
            } else {
                None
            };
            if let Some(a) = align {
                attrs.push_str(&format!(" fo:text-align=\"{a}\""));
            }
        }

        let parent = match semantic {
            Some(SemanticRole::Epigraph) => {
                if block.fmt_alignment == Some(Alignment::Right) {
                    "EpigraphAttribution"
                } else {
                    "Epigraph"
                }
            }
            None => "Standard",
        };
        let style = styles.paragraph_style(parent, attrs.trim(), "");
        let inner = add_inline_content(
            &pieces,
            ctx,
            styles,
            comment_window
                .as_ref()
                .map(|(state, window)| (*state, window)),
        );
        Ok(RenderedBlock::Standalone(format!(
            "<text:p text:style-name=\"{style}\">{inner}</text:p>"
        )))
    }

    /// Render `table_id` as a `<table:table>`. ODF's grid model, unlike OOXML's, requires an
    /// explicit element at **every** row/column position (a column-span continuation cannot
    /// simply be omitted the way `export_docx_uc::render_table_docx` omits it) — so every covered
    /// position, whether covered by a row-span *or* a column-span, gets a
    /// `<table:covered-table-cell/>`, and only the anchor (top-left) cell of a span is a real
    /// `<table:table-cell>`.
    fn render_table_odt(
        &self,
        uow: &dyn ExportOdtUnitOfWorkTrait,
        table_id: &EntityId,
        styles: &mut OdtStyleSheet,
        ctx: &WalkCtx,
        counter: &mut i64,
    ) -> Result<String> {
        let table = uow
            .get_table(table_id)?
            .ok_or_else(|| anyhow!("Table not found"))?;

        let cell_ids = uow.get_table_relationship(
            table_id,
            &common::direct_access::table::TableRelationshipField::Cells,
        )?;
        let cells_opt = uow.get_table_cell_multi(&cell_ids)?;
        let mut cells: Vec<TableCell> = cells_opt.into_iter().flatten().collect();
        cells.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));

        let rows = table.rows.max(0) as usize;
        let cols = table.columns.max(0) as usize;
        let mut covered = vec![vec![false; cols]; rows];

        // A table-level automatic style: an explicit width when the model gives one (converted
        // from logical pixels like every other length here), else `style:rel-width="100%"` so
        // the table fills the text area the way a reader expects rather than shrinking to its
        // narrowest possible content width (ODF's own default when no width is declared at
        // all). Alignment maps to `table:align`'s ODF values (`"left"`/`"right"`/`"center"`;
        // `Alignment::Justify` has no table analog and falls back to `"margins"`, ODF's own "as
        // wide as the page margins allow" default) — the model's own `fmt_alignment`, when set,
        // wins over the width-based default.
        let width_attr = match table.fmt_width.filter(|&w| w > 0) {
            Some(width) => format!(
                "style:width=\"{}\"",
                odt_render::fmt_pt(odt_render::px_to_pt(width))
            ),
            None => "style:rel-width=\"100%\"".to_string(),
        };
        let align = match &table.fmt_alignment {
            Some(Alignment::Left) => "left",
            Some(Alignment::Right) => "right",
            Some(Alignment::Center) => "center",
            Some(Alignment::Justify) => "margins",
            None if table.fmt_width.is_some_and(|w| w > 0) => "left",
            None => "margins",
        };
        let table_style = styles.table_style(&format!("{width_attr} table:align=\"{align}\""));

        let mut cols_xml = String::new();
        if table.column_widths.is_empty() {
            if cols > 0 {
                cols_xml.push_str(&format!(
                    "<table:table-column table:number-columns-repeated=\"{cols}\"/>"
                ));
            }
        } else {
            for w in &table.column_widths {
                let attrs = format!(
                    "style:column-width=\"{}\"",
                    odt_render::fmt_pt(odt_render::px_to_pt(*w))
                );
                let col_style = styles.table_column_style(&attrs);
                cols_xml.push_str(&format!(
                    "<table:table-column table:style-name=\"{col_style}\"/>"
                ));
            }
        }

        // Borders are per-cell in ODF (there is no single "the table has a border" switch the
        // way `html_render`'s `<table border>` is) — every cell shares one automatic style, kept
        // to the two properties that actually matter: whether it draws a border, and a small
        // fixed padding so the border (or the lack of one) isn't flush against the text either
        // way.
        let cell_attrs = if table.fmt_border.is_some_and(|b| b > 0) {
            "fo:border=\"0.5pt solid #000000\" fo:padding=\"0.1cm\""
        } else {
            "fo:padding=\"0.1cm\""
        };
        let cell_style = styles.table_cell_style(cell_attrs);

        let mut rows_xml = String::new();
        for r in 0..rows {
            let mut row_xml = String::new();
            for c in 0..cols {
                if covered[r][c] {
                    row_xml.push_str("<table:covered-table-cell/>");
                    continue;
                }
                let cell = cells
                    .iter()
                    .find(|cell| cell.row == r as i64 && cell.column == c as i64);
                let Some(cell) = cell else {
                    row_xml.push_str(&format!(
                        "<table:table-cell table:style-name=\"{cell_style}\"><text:p/></table:table-cell>"
                    ));
                    continue;
                };

                // Spans are `i64`; clamp to >= 1 before any `as usize` so a malformed (0 or
                // negative) span can never wrap to a huge `usize` and blow up the coverage loop —
                // the same guard `export_docx_uc::render_table_docx` applies.
                let row_span = cell.row_span.max(1) as usize;
                let col_span = cell.column_span.max(1) as usize;
                let mut span_attrs = String::new();
                if col_span > 1 {
                    span_attrs.push_str(&format!(" table:number-columns-spanned=\"{col_span}\""));
                }
                if row_span > 1 {
                    span_attrs.push_str(&format!(" table:number-rows-spanned=\"{row_span}\""));
                }

                let mut inner = String::new();
                if let Some(cf_id) = cell.cell_frame
                    && let Some(cell_frame) = uow.get_frame(&cf_id)?
                {
                    // The document-level cell-frame skip set does not apply inside a cell (an
                    // ordinary sub-frame here is real content, not another table's cell), so an
                    // empty set is passed — mirrors `export_docx_uc::render_table_docx`. `None`
                    // for comments: table-cell prose is out of scope for comment ranges — see
                    // this module's own doc comment and `render_block`'s.
                    self.render_frame_content(
                        uow,
                        &cell_frame,
                        &HashSet::new(),
                        0,
                        None,
                        styles,
                        ctx,
                        None,
                        &mut inner,
                        counter,
                        None,
                    )?;
                }
                if inner.is_empty() {
                    // ODF's schema permits an empty `table:table-cell`, but LibreOffice's own
                    // writer always gives one at least one empty paragraph, and matching that is
                    // simpler than finding out the hard way whether some other reader assumes it.
                    inner.push_str("<text:p/>");
                }
                row_xml.push_str(&format!(
                    "<table:table-cell table:style-name=\"{cell_style}\"{span_attrs}>{inner}</table:table-cell>"
                ));

                for sr in 0..row_span {
                    for sc in 0..col_span {
                        if sr == 0 && sc == 0 {
                            continue;
                        }
                        if r + sr < rows && c + sc < cols {
                            covered[r + sr][c + sc] = true;
                        }
                    }
                }
            }
            rows_xml.push_str(&format!("<table:table-row>{row_xml}</table:table-row>"));
        }

        *counter += 1;
        Ok(format!(
            "<table:table table:name=\"Table{table_id}\" table:style-name=\"{table_style}\">\
             {cols_xml}{rows_xml}</table:table>"
        ))
    }
}

/// A rendered block, on its way into `content.xml`'s body.
enum RenderedBlock {
    /// Goes straight into the body (or into the currently-open list-item's content, when a
    /// caller wants that — see `ListStack::push`'s own callers): a heading, code block, task
    /// item, or plain paragraph. Any open `<text:list>` must be flushed before this is appended,
    /// which `dispatch_block` does uniformly rather than trusting each producer to remember.
    Standalone(String),
    /// Belongs inside a `<text:list>`/`<text:list-item>` tree — see this module's doc comment for
    /// why ODF needs a real nested structure here and DOCX does not.
    ListItem {
        list_id: EntityId,
        depth: i64,
        style_name: String,
        inner: String,
    },
}

/// The paragraph-property attributes every block kind applies identically, computed once and
/// layered with kind-specific extras by each `render_block` branch. Mirrors the "common
/// formatting" section at the top of `export_docx_uc::render_block`'s dispatch, applied before
/// the heading/list/plain branch — everything here except quote indentation is skipped for a
/// code block, exactly as `export_docx_uc::render_code_block` skips it by returning early.
fn common_para_attrs(block: &Block, quote_indent_pt: f64) -> String {
    let mut attrs = String::new();
    if let Some(lh) = block.fmt_line_height {
        // thousandths → percent: 1000 = 100% (single spacing), 1500 = 150%.
        let percent = (lh as f64 / 10.0).round() as i64;
        attrs.push_str(&format!(" fo:line-height=\"{percent}%\""));
    }
    if block.fmt_non_breakable_lines == Some(true) {
        attrs.push_str(" fo:keep-together=\"always\"");
    }
    if block.fmt_page_break_before == Some(true) {
        attrs.push_str(" fo:break-before=\"page\"");
    }
    if let Some(alignment) = &block.fmt_alignment {
        attrs.push_str(&format!(
            " fo:text-align=\"{}\"",
            odt_render::odf_align(alignment)
        ));
    }
    if block.fmt_direction == Some(TextDirection::RightToLeft) {
        attrs.push_str(" style:writing-mode=\"rl-tb\"");
    }
    if quote_indent_pt > 0.0 {
        attrs.push_str(&format!(
            " fo:margin-left=\"{}\"",
            odt_render::fmt_pt(quote_indent_pt)
        ));
    }
    attrs
}

/// Whether `pieces` is one plain, unformatted text run whose content — once whitespace is
/// stripped — is a single non-alphanumeric character repeated one or more times: `"* * *"`,
/// `"###"`, `". . ."`, `"＊"`, `"-"`, and every other scene-break glyph
/// `skribisto_compiler::preset` offers share this shape. See this module's doc comment for why
/// this crate cannot simply match a fixed glyph list. An image, footnote reference, hyperlink, or
/// any character-level formatting disqualifies a paragraph immediately — those mean a writer
/// composed real content here, not a typed thematic-break marker.
///
/// Takes the same `(InlineSegment, u32, u32)` pieces `render_block` already built for comment
/// placement (ignoring the char-space `start`/`end` — the rule check only ever cares about the
/// text) rather than a bare `&[InlineSegment]`, so `render_block` does not need to keep two
/// parallel views of the same block alive.
fn looks_like_rule_glyph(pieces: &[(InlineSegment, u32, u32)]) -> bool {
    let mut text = String::new();
    for (elem, _, _) in pieces {
        match &elem.content {
            InlineContent::Text(t) => {
                if elem.fmt_font_bold == Some(true)
                    || elem.fmt_font_italic == Some(true)
                    || elem.fmt_font_underline == Some(true)
                    || elem.fmt_font_strikeout == Some(true)
                    || elem.fmt_anchor_href.is_some()
                {
                    return false;
                }
                text.push_str(t);
            }
            InlineContent::Empty => {}
            InlineContent::Image { .. } | InlineContent::FootnoteRef { .. } => return false,
        }
    }
    let stripped: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    let Some(&first) = stripped.first() else {
        return false;
    };
    if first.is_alphanumeric() {
        return false;
    }
    // A **lone** mark of sentence punctuation is prose, not furniture.
    //
    // Found on a real manuscript: a paragraph containing only `!` — a beat of dialogue — was
    // being replaced by a horizontal rule, and the character was simply gone from the exported
    // file. The same would happen to a lone `…`, which is if anything more common: an ellipsis
    // alone on a line is an ordinary silence in fiction, and the host this crate was built for
    // says so in as many words, deliberately refusing to treat one as a scene break.
    //
    // Every glyph a break is actually typed with is either repeated (`* * *`, `###`, `. . .`)
    // or is a symbol no one ends a sentence with (`＊`, `⁂`, `◇`, `§`, `—`). Requiring one of
    // those two keeps every real break working and stops a writer's punctuation from
    // evaporating.
    const SENTENCE_PUNCTUATION: &[char] = &['!', '?', '.', '\u{2026}', ',', ';', ':'];
    if stripped.len() == 1 && SENTENCE_PUNCTUATION.contains(&first) {
        return false;
    }
    stripped[1..].iter().all(|&c| c == first)
}

/// Build a `<text:list-style>` for `list`, with all nine levels sharing the same
/// ordered/bulleted-ness and format — see this module's doc comment for why uniform levels are
/// the correct choice here, not a shortcut.
fn odt_list_style_xml(name: &str, list: &List) -> String {
    /// A list's ordered/bulleted-ness and glyph never vary by nesting level (see this
    /// function's own doc comment on `odt_list_style_xml`'s caller) — computed once, outside
    /// the per-level loop below, rather than re-matched nine times.
    enum Marker {
        Number { format: &'static str },
        Bullet { glyph: &'static str },
    }
    let marker = match list.style {
        ListStyle::Decimal => Marker::Number { format: "1" },
        ListStyle::LowerAlpha => Marker::Number { format: "a" },
        ListStyle::UpperAlpha => Marker::Number { format: "A" },
        ListStyle::LowerRoman => Marker::Number { format: "i" },
        ListStyle::UpperRoman => Marker::Number { format: "I" },
        ListStyle::Disc => Marker::Bullet { glyph: "\u{2022}" },
        ListStyle::Circle => Marker::Bullet { glyph: "\u{25CB}" },
        ListStyle::Square => Marker::Bullet { glyph: "\u{25AA}" },
    };
    let suffix = if list.suffix.is_empty() {
        "."
    } else {
        list.suffix.as_str()
    };

    let mut levels = String::new();
    for level in 1..=9i64 {
        let space_before = odt_render::fmt_pt(INDENT_STEP_PT * level as f64);
        let props = format!(
            "<style:list-level-properties text:space-before=\"{space_before}\" text:min-label-width=\"0.5cm\"/>"
        );
        match &marker {
            Marker::Number { format } => levels.push_str(&format!(
                "<text:list-level-style-number text:level=\"{level}\" style:num-format=\"{format}\" \
                 style:num-prefix=\"{prefix}\" style:num-suffix=\"{suffix}\">{props}</text:list-level-style-number>",
                prefix = odt_render::xml_escape(&list.prefix),
                suffix = odt_render::xml_escape(suffix),
            )),
            Marker::Bullet { glyph } => levels.push_str(&format!(
                "<text:list-level-style-bullet text:level=\"{level}\" text:bullet-char=\"{glyph}\">{props}</text:list-level-style-bullet>"
            )),
        }
    }
    format!("<text:list-style style:name=\"{name}\">{levels}</text:list-style>")
}

/// One depth level of an in-progress nested `<text:list>` tree — see this module's doc comment
/// for why ODF needs this and DOCX does not.
struct ListFrame {
    list_id: EntityId,
    depth: i64,
    style_name: String,
    /// Already-closed `<text:list-item>…</text:list-item>` strings, in order.
    finished_items: Vec<String>,
    /// The inner XML of the item currently being built. A deeper sub-list appends into this
    /// (via `ListStack::close_last`) *before* the item is finalized, which is what keeps a
    /// sub-list nested inside its parent item rather than becoming the parent's sibling.
    current_item_inner: String,
    has_open_item: bool,
}

/// Turns a flat sequence of `(List entity, indent)`-tagged blocks into a properly nested
/// `<text:list>` tree. One instance is scoped to one `render_frame_content` call (one frame's
/// flat content run) — see that function's own doc comment.
#[derive(Default)]
struct ListStack {
    frames: Vec<ListFrame>,
}

impl ListStack {
    /// Add one list item at `(list_id, depth)`. Continues the currently-open list when it is the
    /// same `(list_id, depth)` pair as the last item pushed; otherwise closes whatever needs
    /// closing (deeper frames, and a same-depth-but-different-list frame) and opens a new one.
    /// `item_body` is that item's own rendered content (a `<text:p>` or `<text:h>` — ODF permits
    /// either directly inside a `<text:list-item>`).
    fn push(
        &mut self,
        out: &mut String,
        list_id: EntityId,
        depth: i64,
        style_name: String,
        item_body: String,
    ) {
        while self.frames.last().is_some_and(|f| f.depth > depth) {
            self.close_last(out);
        }
        let continues = self
            .frames
            .last()
            .is_some_and(|f| f.depth == depth && f.list_id == list_id);
        if !continues && self.frames.last().is_some_and(|f| f.depth == depth) {
            self.close_last(out);
        }
        if !continues {
            self.frames.push(ListFrame {
                list_id,
                depth,
                style_name,
                finished_items: Vec::new(),
                current_item_inner: String::new(),
                has_open_item: false,
            });
        }
        // Reaches a real frame either way: `continues` means the frame checked above is still
        // the top of the stack (nothing was popped or pushed since), and the `!continues` arm
        // above just pushed one.
        if let Some(f) = self.frames.last_mut() {
            if continues && f.has_open_item {
                let inner = std::mem::take(&mut f.current_item_inner);
                f.finished_items
                    .push(format!("<text:list-item>{inner}</text:list-item>"));
            }
            f.current_item_inner.push_str(&item_body);
            f.has_open_item = true;
        }
    }

    /// Close the deepest open frame, finalizing its last item and wrapping every finished item
    /// into a `<text:list>`. That XML either extends the new deepest frame's still-open item
    /// (this was a sub-list) or, once the stack is empty, is appended to `out` directly (this was
    /// a top-level list).
    fn close_last(&mut self, out: &mut String) {
        let Some(mut frame) = self.frames.pop() else {
            return;
        };
        if frame.has_open_item {
            let inner = std::mem::take(&mut frame.current_item_inner);
            frame
                .finished_items
                .push(format!("<text:list-item>{inner}</text:list-item>"));
        }
        let list_xml = format!(
            "<text:list text:style-name=\"{}\">{}</text:list>",
            frame.style_name,
            frame.finished_items.concat()
        );
        if let Some(parent) = self.frames.last_mut() {
            parent.current_item_inner.push_str(&list_xml);
        } else {
            out.push_str(&list_xml);
        }
    }

    /// Close every remaining open frame. Called before any standalone (non-list) content and
    /// unconditionally at the end of the block sequence this stack was scoped to.
    fn flush(&mut self, out: &mut String) {
        while !self.frames.is_empty() {
            self.close_last(out);
        }
    }
}

/// Build one inline run's XML. Returns `None` for a segment that contributes nothing (an empty
/// image fallback, an empty text run). Mirrors `export_docx_uc::build_run`'s dispatch, including
/// the identical footnote-repeat-citation workaround (see `FootnoteRefState`'s doc comment there,
/// and this module's for the ODF-specific "never nest a real note" addition).
fn build_run(elem: &InlineSegment, ctx: &WalkCtx, styles: &mut OdtStyleSheet) -> Option<String> {
    if let InlineContent::FootnoteRef { label } = &elem.content {
        let is_first = ctx
            .footnote_state
            .emitted
            .borrow_mut()
            .insert(label.clone());
        if is_first && !ctx.inside_note_body {
            let id = ctx.footnote_state.take_id();
            let body = ctx.notes.get(label).cloned();
            let body = match body {
                Some(b) if !b.is_empty() => b,
                _ => "<text:p/>".to_string(),
            };
            return Some(format!(
                "<text:note text:id=\"ftn{id}\" text:note-class=\"footnote\">\
                 <text:note-citation>{marker}</text:note-citation>\
                 <text:note-body>{body}</text:note-body></text:note>",
                marker = odt_render::xml_escape(&ctx.footnote_state.numbers.marker(label))
            ));
        }
        let marker = odt_render::xml_escape(&ctx.footnote_state.numbers.marker(label));
        let style = styles.text_style("style:text-position=\"super 58%\"");
        return Some(format!(
            "<text:span text:style-name=\"{style}\">{marker}</text:span>"
        ));
    }

    let text = match &elem.content {
        InlineContent::FootnoteRef { .. } => return None,
        InlineContent::Text(t) => t.clone(),
        InlineContent::Image {
            name,
            alt,
            width,
            height,
            ..
        } => {
            if let Some(xml) = build_image_frame(name, alt, *width, *height, ctx) {
                return Some(xml);
            }
            if alt.is_empty() {
                return None;
            }
            alt.clone()
        }
        InlineContent::Empty => return None,
    };
    if text.is_empty() {
        return None;
    }
    Some(text_run_with_format(&text, elem, styles))
}

/// Build a run carrying `text`, formatted per `elem`'s `fmt_*` fields. Split out of
/// [`build_run`]'s tail so `append_piece` can call it directly for a *substring* of a text
/// segment — the piece straddling a comment boundary — without routing back through
/// `build_run`'s footnote/image handling, neither of which a plain text segment ever touches
/// anyway. Mirrors `export_docx_uc::text_run_with_format`'s identical role.
fn text_run_with_format(text: &str, elem: &InlineSegment, styles: &mut OdtStyleSheet) -> String {
    let mut attrs = character_style_attrs_from_flags(
        elem.fmt_font_bold == Some(true),
        elem.fmt_font_italic == Some(true),
        elem.fmt_font_underline == Some(true),
        elem.fmt_font_strikeout == Some(true),
    );
    if elem.fmt_font_family.as_deref() == Some("monospace") {
        attrs.push_str(" style:font-name=\"Courier New\"");
    }
    let encoded = odt_render::encode_run_text(text);
    if attrs.is_empty() {
        encoded
    } else {
        let style = styles.text_style(attrs.trim());
        format!("<text:span text:style-name=\"{style}\">{encoded}</text:span>")
    }
}

/// One inline piece, resolved to whether/how it contributes a run — the ODF analog of
/// `export_docx_uc::RenderedPiece`.
struct RenderedPiece<'p> {
    elem: &'p InlineSegment,
    start: u32,
    end: u32,
    /// The run `build_run` would emit for the whole piece — `None` for content it drops
    /// entirely (e.g. an inline image with neither embeddable bytes nor alt text). Kept even
    /// when `None`: a comment boundary sitting exactly at a run-less piece's position still
    /// needs somewhere to attach its marker, and dropping the piece outright would silently
    /// lose that comment instead of anchoring it (`CommentEmitState::ensure_all_anchored` exists
    /// specifically to catch the alternative — a comment that reaches no piece at all).
    run: Option<String>,
}

/// Append the inline content of a block: build one [`RenderedPiece`] per source piece (applying
/// `build_run`'s footnote/image side effects exactly once each, same as before comment support
/// existed), group consecutive same-`href` pieces under one `<text:a>`, and — when `comments` is
/// `Some` — split whichever run straddles a comment boundary, interleaving each comment's
/// `<office:annotation>`/`<office:annotation-end>` at the right point. Mirrors
/// `export_docx_uc::add_inline_content`'s shape; see that function's own doc comment for the
/// three cases this covers (mid-run split, split inside a hyperlink's own children, two
/// overlapping comments in one block) — identical here, just built as `String` concatenation
/// rather than `docx_rs` builder calls.
fn add_inline_content(
    pieces: &[(InlineSegment, u32, u32)],
    ctx: &WalkCtx,
    styles: &mut OdtStyleSheet,
    comments: Option<(&CommentEmitState<'_>, &BlockCommentWindow<'_>)>,
) -> String {
    let rendered: Vec<RenderedPiece<'_>> = pieces
        .iter()
        .map(|(elem, start, end)| RenderedPiece {
            elem,
            start: *start,
            end: *end,
            run: build_run(elem, ctx, styles),
        })
        .collect();

    let mut out = String::new();

    if rendered.is_empty() {
        // A genuinely empty block (e.g. a blank paragraph) has no piece to wrap a marker
        // around, but a comment whose range touches this point still needs to be anchored, or
        // `ensure_all_anchored` fails the export for a thread that had every right to exist.
        //
        // This used to claim an empty block's `starts` and `ends` hold the same comments, and
        // read `starts` for both. They do not: `window_for_block`'s two empty-block arms test
        // `c.start == block_start` and `c.end == block_end` independently, so they coincide
        // only for a comment that both begins and ends exactly here. See the closing loop.
        if let Some((state, window)) = comments {
            for &c in &window.starts {
                state.mark_started(c.id);
                out.push_str(&c.open_xml);
            }
            // Closed from `ends`, NOT from `starts`. `window_for_block`'s empty-block arms are
            // `c.start == block_start` for one and `c.end == block_end` for the other — which
            // select the *same* comment only when it both begins and ends at this exact point.
            // A comment that starts at an empty block and ends further on is in `starts` alone,
            // and closing it here would emit `annotation-end` for a range still open; one that
            // ends at an empty block having begun earlier is in `ends` alone, and reading
            // `starts` would never close it at all.
            for &c in window.ends.iter().rev() {
                state.mark_ended(c.id);
                out.push_str(&c.close_xml);
            }
        }
        return out;
    }

    // Coalesce consecutive pieces sharing the same href into one hyperlink group — mirrors the
    // grouping `add_inline_content` always did, before comment support existed; a run-less
    // piece (see `RenderedPiece::run`) still participates by its own `href`, same as any other.
    enum Group {
        Plain(usize),
        Link(String, std::ops::Range<usize>),
    }
    let mut groups: Vec<Group> = Vec::new();
    for (i, piece) in rendered.iter().enumerate() {
        match &piece.elem.fmt_anchor_href {
            Some(href) if !href.is_empty() => {
                if let Some(Group::Link(open_href, range)) = groups.last_mut()
                    && open_href == href
                {
                    range.end = i + 1;
                    continue;
                }
                groups.push(Group::Link(href.clone(), i..i + 1));
            }
            _ => groups.push(Group::Plain(i)),
        }
    }

    for group in groups {
        match group {
            Group::Plain(i) => append_piece(&mut out, &rendered[i], styles, comments),
            Group::Link(href, range) => {
                out.push_str(&format!(
                    "<text:a xlink:type=\"simple\" xlink:href=\"{}\">",
                    odt_render::xml_escape(&href)
                ));
                for i in range {
                    append_piece(&mut out, &rendered[i], styles, comments);
                }
                out.push_str("</text:a>");
            }
        }
    }

    out
}

/// Append one [`RenderedPiece`] to `out` (the paragraph's own body, or a `<text:a>` group's
/// inner buffer being built up inside it — see `add_inline_content`'s `Group::Link` arm),
/// splitting its run at any comment boundary `markers_for_piece` finds inside it. Mirrors
/// `export_docx_uc::append_piece`'s split logic exactly; see that function's doc comment for why
/// atomic content (an image or footnote reference) is only ever placed *around*, never split.
fn append_piece(
    out: &mut String,
    piece: &RenderedPiece<'_>,
    styles: &mut OdtStyleSheet,
    comments: Option<(&CommentEmitState<'_>, &BlockCommentWindow<'_>)>,
) {
    let Some((state, window)) = comments else {
        if let Some(run) = &piece.run {
            out.push_str(run);
        }
        return;
    };
    let markers = markers_for_piece(window, piece.start, piece.end);
    if markers.is_empty() {
        if let Some(run) = &piece.run {
            out.push_str(run);
        }
        return;
    }

    if let InlineContent::Text(text) = &piece.elem.content {
        // The general, mid-run case: slice the text at each marker's local char index —
        // `.chars()`, never a byte index, since `markers_for_piece`'s indices are character
        // offsets and this text can hold any UTF-8.
        let chars: Vec<char> = text.chars().collect();
        let mut cursor = 0usize;
        for (idx, marker) in &markers {
            let local = (*idx as usize).min(chars.len());
            if local > cursor {
                let slice: String = chars[cursor..local].iter().collect();
                out.push_str(&text_run_with_format(&slice, piece.elem, styles));
                cursor = local;
            }
            apply_marker(out, marker, state);
        }
        if cursor < chars.len() {
            let slice: String = chars[cursor..].iter().collect();
            out.push_str(&text_run_with_format(&slice, piece.elem, styles));
        }
    } else {
        // Atomic content (image, footnote reference, or a run-less piece): every marker sits at
        // local index `0` (before) or the piece's own length (after) — see
        // `markers_for_piece`'s doc comment — so there is only ever placement around the one
        // run, never a true split.
        for (idx, marker) in &markers {
            if *idx == 0 {
                apply_marker(out, marker, state);
            }
        }
        if let Some(run) = &piece.run {
            out.push_str(run);
        }
        for (idx, marker) in &markers {
            if *idx != 0 {
                apply_marker(out, marker, state);
            }
        }
    }
}

/// Embed an inline image as a `<draw:frame>`/`<draw:image>` pair, anchored `as-char` so it flows
/// inline with the surrounding text exactly like a DOCX/HTML inline image. Returns `None` when
/// the caller supplied no bytes for this `src`, or when those bytes are not a decodable image and
/// the model gave no explicit display size to fall back on — the same "an unreadable picture must
/// never fail the whole manuscript export" contract `export_docx_uc::build_image_run` documents.
///
/// Unlike DOCX (which re-encodes every image to PNG because `docx-rs` always names embedded media
/// parts `.png` regardless of content — see that function's own doc comment for the trap), ODF
/// carries an explicit `manifest:media-type` per part, so the original bytes are embedded
/// unmodified; no transcoding step exists to fail.
fn build_image_frame(
    name: &str,
    alt: &str,
    width: i64,
    height: i64,
    ctx: &WalkCtx,
) -> Option<String> {
    let href = ctx.image_hrefs.get(name)?;
    let image = ctx.images.get(name)?;

    let (natural_w, natural_h) = if width > 0 && height > 0 {
        (width, height)
    } else {
        use image::GenericImageView;
        let decoded = image::load_from_memory(&image.bytes).ok()?;
        let (w, h) = decoded.dimensions();
        (
            if width > 0 { width } else { w as i64 },
            if height > 0 { height } else { h as i64 },
        )
    };

    let seq = ctx.image_seq.get() + 1;
    ctx.image_seq.set(seq);
    let frame_name = format!("Image{seq}");
    let title = if alt.is_empty() {
        String::new()
    } else {
        format!("<svg:title>{}</svg:title>", odt_render::xml_escape(alt))
    };
    Some(format!(
        "<draw:frame draw:name=\"{frame_name}\" svg:width=\"{}\" svg:height=\"{}\" \
         text:anchor-type=\"as-char\">\
         <draw:image xlink:href=\"{}\" xlink:type=\"simple\" xlink:show=\"embed\" \
         xlink:actuate=\"onLoad\"/>{title}</draw:frame>",
        odt_render::fmt_pt(odt_render::px_to_pt(natural_w)),
        odt_render::fmt_pt(odt_render::px_to_pt(natural_h)),
        odt_render::xml_escape(href),
    ))
}

/// Assign every supplied image a stable in-package href (`Pictures/img_NNN.ext`), independent of
/// the document's own `src` strings the same way `export_epub_uc::image_packaging_map` is — a
/// `src` may be an absolute path, may repeat, and may contain characters that are legal in a
/// filesystem but not in an ODF href. `ExportImages` iterates in `src` order (a `BTreeMap`), so
/// this is deterministic across two exports of the same document.
fn build_image_href_map(images: &ExportImages) -> BTreeMap<String, String> {
    images
        .iter()
        .enumerate()
        .map(|(i, (src, image))| {
            (
                src.clone(),
                format!("Pictures/img_{:03}.{}", i + 1, image.extension()),
            )
        })
        .collect()
}

/// Return `Err` if a cancellation flag is present and set.
fn check_cancelled(cancel_flag: Option<&AtomicBool>) -> Result<()> {
    if let Some(flag) = cancel_flag
        && flag.load(Ordering::Relaxed)
    {
        return Err(anyhow!("Operation was cancelled"));
    }
    Ok(())
}
