//! Property tests for the DOCX exporter.
//!
//! There is no DOCX *importer*, so this cannot be a model→docx→model fixpoint
//! the way the djot tests are. Instead we generate a random document (as djot,
//! imported through the real importer), export it via the file-less builder, and
//! assert structural invariants that any correct export must satisfy:
//!
//!  1. **Totality** — the build never errors or panics.
//!  2. **No content loss** — every source word appears in the output text.
//!  3. **No block dropped** — at least one paragraph per source block.
//!  4. **Referential integrity** — every paragraph that references a numbering
//!     id resolves to a registered numbering and abstract numbering.

extern crate text_document_io as document_io;

use common::long_operation::{LongOperationManager, OperationStatus};
use document_io::docx_rs::{DocumentChild, Docx, Paragraph, ParagraphChild, RunChild};
use document_io::{ExportDocxDto, ImportDjotDto, document_io_controller};
use proptest::prelude::*;
use test_harness::setup;

// --- document model under test --------------------------------------------

/// One generated block. `String` payloads are always `[a-z]{3,8}` words so they
/// survive djot round-tripping unescaped and never collide with block markers.
#[derive(Debug, Clone)]
enum GenBlock {
    Plain(String),
    Heading(u8, String),
    Aligned(&'static str, String),
    Bullet(String),
    Ordered(String),
    Code(String),
    Task(bool, String),
}

impl GenBlock {
    /// The visible words this block contributes to the output.
    fn words(&self) -> Vec<&str> {
        match self {
            GenBlock::Plain(w)
            | GenBlock::Heading(_, w)
            | GenBlock::Aligned(_, w)
            | GenBlock::Bullet(w)
            | GenBlock::Ordered(w)
            | GenBlock::Code(w)
            | GenBlock::Task(_, w) => vec![w.as_str()],
        }
    }

    fn emit(&self) -> String {
        match self {
            GenBlock::Plain(w) => w.clone(),
            GenBlock::Heading(level, w) => format!("{} {}", "#".repeat(*level as usize), w),
            GenBlock::Aligned(a, w) => format!("{{alignment={a}}}\n{w}"),
            GenBlock::Bullet(w) => format!("- {w}"),
            GenBlock::Ordered(w) => format!("1. {w}"),
            GenBlock::Code(w) => format!("```\n{w}\n```"),
            GenBlock::Task(true, w) => format!("- [x] {w}"),
            GenBlock::Task(false, w) => format!("- [ ] {w}"),
        }
    }
}

fn word() -> impl Strategy<Value = String> {
    "[a-z]{3,8}"
}

fn gen_block() -> impl Strategy<Value = GenBlock> {
    prop_oneof![
        word().prop_map(GenBlock::Plain),
        (1u8..=6, word()).prop_map(|(l, w)| GenBlock::Heading(l, w)),
        (
            prop_oneof![Just("left"), Just("right"), Just("center"), Just("justify")],
            word()
        )
            .prop_map(|(a, w)| GenBlock::Aligned(a, w)),
        word().prop_map(GenBlock::Bullet),
        word().prop_map(GenBlock::Ordered),
        word().prop_map(GenBlock::Code),
        (any::<bool>(), word()).prop_map(|(c, w)| GenBlock::Task(c, w)),
    ]
}

fn emit_doc(blocks: &[GenBlock]) -> String {
    blocks
        .iter()
        .map(GenBlock::emit)
        .collect::<Vec<_>>()
        .join("\n\n")
}

// --- harness ---------------------------------------------------------------

fn build_docx(djot: &str) -> Docx {
    let (db, ev, _) = setup().expect("setup");
    let mut mgr = LongOperationManager::new();
    let op = document_io_controller::import_djot(
        &db,
        &ev,
        &mut mgr,
        &ImportDjotDto {
            djot_text: djot.to_string(),
            options: Default::default(),
        },
    )
    .expect("import_djot");
    while let Some(OperationStatus::Running) = mgr.get_operation_status(&op) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(
        mgr.get_operation_status(&op),
        Some(OperationStatus::Completed),
        "import did not complete for {djot:?}"
    );
    document_io_controller::build_docx_document(&db, &ExportDocxDto::default())
        .expect("build_docx_document must not fail")
}

fn paragraphs(docx: &Docx) -> Vec<&Paragraph> {
    docx.document
        .children
        .iter()
        .filter_map(|c| match c {
            DocumentChild::Paragraph(p) => Some(&**p),
            _ => None,
        })
        .collect()
}

fn collect_text(children: &[ParagraphChild], out: &mut String) {
    for child in children {
        match child {
            ParagraphChild::Run(run) => {
                for rc in &run.children {
                    if let RunChild::Text(t) = rc {
                        out.push_str(&t.text);
                    }
                }
            }
            ParagraphChild::Hyperlink(h) => collect_text(&h.children, out),
            _ => {}
        }
    }
}

fn all_text(docx: &Docx) -> String {
    let mut s = String::new();
    for p in paragraphs(docx) {
        collect_text(&p.children, &mut s);
        s.push('\n');
    }
    s
}

// --- the properties --------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn export_is_total_and_lossless(blocks in prop::collection::vec(gen_block(), 1..8)) {
        let djot = emit_doc(&blocks);
        let docx = build_docx(&djot);

        let paras = paragraphs(&docx);

        // (3) No source block is dropped.
        prop_assert!(
            paras.len() >= blocks.len(),
            "expected >= {} paragraphs, got {}\n--- djot ---\n{}",
            blocks.len(),
            paras.len(),
            djot
        );

        // (2) Every source word survives into the output.
        let text = all_text(&docx);
        for block in &blocks {
            for w in block.words() {
                prop_assert!(
                    text.contains(w),
                    "word {:?} missing from export\n--- djot ---\n{}\n--- text ---\n{}",
                    w, djot, text
                );
            }
        }

        // (4) Referential integrity of numbering references.
        for p in &paras {
            if let Some(np) = &p.property.numbering_property
                && let Some(id) = &np.id
            {
                let num = docx.numberings.numberings.iter().find(|n| n.id == id.id);
                prop_assert!(
                    num.is_some(),
                    "paragraph references numbering id {} with no definition",
                    id.id
                );
                let abstract_id = num.unwrap().abstract_num_id;
                prop_assert!(
                    docx.numberings.abstract_nums.iter().any(|a| a.id == abstract_id),
                    "numbering {} references missing abstract numbering {}",
                    id.id, abstract_id
                );
            }
        }
    }
}
