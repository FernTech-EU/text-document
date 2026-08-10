// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Cyril Jacquet

// Hand-written, mirroring the Qleany-generated shape of `export_docx_uow.rs`/
// `export_epub_uow.rs` exactly (same `#[macros::uow_action]` list, same read-only transaction
// wiring) — there is no manifest entity for "ODT export", so this was never generated and never
// will be; it is written once, by hand, and kept in lockstep with `export_odt_uc.rs`'s trait the
// same way every other export UoW in this crate is (see this crate's `CLAUDE.md`/workspace notes
// on why `units_of_work/*_uow.rs` is hand-maintained, not "still generated").

use crate::use_cases::export_odt_uc::{ExportOdtUnitOfWorkFactoryTrait, ExportOdtUnitOfWorkTrait};
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

pub struct ExportOdtUnitOfWork {
    context: DbContext,
    transaction: Mutex<Option<Transaction>>,
}

impl ExportOdtUnitOfWork {
    pub fn new(db_context: &DbContext) -> Self {
        ExportOdtUnitOfWork {
            context: db_context.clone(),
            transaction: Mutex::new(None),
        }
    }
}

impl QueryUnitOfWork for ExportOdtUnitOfWork {
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
impl ExportOdtUnitOfWorkTrait for ExportOdtUnitOfWork {}

pub struct ExportOdtUnitOfWorkFactory {
    context: DbContext,
}

impl ExportOdtUnitOfWorkFactory {
    pub fn new(db_context: &DbContext) -> Self {
        ExportOdtUnitOfWorkFactory {
            context: db_context.clone(),
        }
    }
}

impl ExportOdtUnitOfWorkFactoryTrait for ExportOdtUnitOfWorkFactory {
    fn create(&self) -> Box<dyn ExportOdtUnitOfWorkTrait> {
        Box::new(ExportOdtUnitOfWork::new(&self.context))
    }
}
