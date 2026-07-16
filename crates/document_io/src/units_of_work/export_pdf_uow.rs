// Custom implementation — hand-maintained, mirrors `export_epub_uow.rs`'s shape exactly (same
// `Mutex`-guarded transaction, same `thread_safe = true` uow-action set, since PDF export is a
// `LongOperation` like EPUB/DOCX, not a synchronous use case like LaTeX/HTML). Kept hand-written
// rather than Qleany-generated in this session, per this repo's regen discipline (targeted regen
// only, never a blanket `-M` sweep) — if this feature is later added to `qleany.yaml` and
// regenerated targeted, this file's shape should come out identical.

use crate::use_cases::export_pdf_uc::{ExportPdfUnitOfWorkFactoryTrait, ExportPdfUnitOfWorkTrait};
use anyhow::{Ok, Result};
use common::database::QueryUnitOfWork;
use common::database::{db_context::DbContext, transactions::Transaction};
#[allow(unused_imports)]
use common::entities::{Block, Document, Frame, List, Resource, Root, Table, TableCell};
#[allow(unused_imports)]
use common::types;
#[allow(unused_imports)]
use common::types::EntityId;
use parking_lot::Mutex;

pub struct ExportPdfUnitOfWork {
    context: DbContext,
    transaction: Mutex<Option<Transaction>>,
}

impl ExportPdfUnitOfWork {
    pub fn new(db_context: &DbContext) -> Self {
        ExportPdfUnitOfWork {
            context: db_context.clone(),
            transaction: Mutex::new(None),
        }
    }
}

impl QueryUnitOfWork for ExportPdfUnitOfWork {
    fn begin_transaction(&self) -> Result<()> {
        let mut transaction = self.transaction.lock();
        *transaction = Some(Transaction::begin_read_transaction(&self.context)?);
        Ok(())
    }

    fn end_transaction(&self) -> Result<()> {
        let mut transaction = self.transaction.lock();
        transaction.take().unwrap().end_read_transaction()?;
        Ok(())
    }

    fn store(&self) -> std::sync::Arc<common::database::Store> {
        self.context.get_store().clone()
    }
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
impl ExportPdfUnitOfWorkTrait for ExportPdfUnitOfWork {}

pub struct ExportPdfUnitOfWorkFactory {
    context: DbContext,
}

impl ExportPdfUnitOfWorkFactory {
    pub fn new(db_context: &DbContext) -> Self {
        ExportPdfUnitOfWorkFactory {
            context: db_context.clone(),
        }
    }
}

impl ExportPdfUnitOfWorkFactoryTrait for ExportPdfUnitOfWorkFactory {
    fn create(&self) -> Box<dyn ExportPdfUnitOfWorkTrait> {
        Box::new(ExportPdfUnitOfWork::new(&self.context))
    }
}
