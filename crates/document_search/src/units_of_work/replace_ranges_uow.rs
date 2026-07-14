// Hand-ported from `qleany generate --temp` into this tree's shape (the installed
// generator is v1.8 and its template has drifted +130/-101 lines from the v1.4.8 code
// this crate is built on, so a straight copy would not compile). Mirrors
// `replace_text_uow.rs` exactly: same entities, same actions.

use crate::use_cases::replace_ranges_uc::{
    ReplaceRangesUnitOfWorkFactoryTrait, ReplaceRangesUnitOfWorkTrait,
};
use anyhow::{Ok, Result};
use common::database::CommandUnitOfWork;
use common::database::{db_context::DbContext, transactions::Transaction};
#[allow(unused_imports)]
use common::entities::{Block, Document, Frame, Root};
use common::event::{AllEvent, DirectAccessEntity, Event, EventBuffer, EventHub, Origin};
#[allow(unused_imports)]
use common::types;
#[allow(unused_imports)]
use common::types::EntityId;
use std::cell::RefCell;
use std::sync::Arc;

// Unit of work for ReplaceRanges

pub struct ReplaceRangesUnitOfWork {
    context: DbContext,
    transaction: Option<Transaction>,
    event_hub: Arc<EventHub>,
    event_buffer: RefCell<EventBuffer>,
}

impl ReplaceRangesUnitOfWork {
    pub fn new(db_context: &DbContext, event_hub: &Arc<EventHub>) -> Self {
        ReplaceRangesUnitOfWork {
            context: db_context.clone(),
            transaction: None,
            event_hub: event_hub.clone(),
            event_buffer: RefCell::new(EventBuffer::new()),
        }
    }
}

impl CommandUnitOfWork for ReplaceRangesUnitOfWork {
    fn begin_transaction(&mut self) -> Result<()> {
        self.transaction = Some(Transaction::begin_write_transaction(&self.context)?);
        self.event_buffer.get_mut().begin_buffering();
        Ok(())
    }

    fn commit(&mut self) -> Result<()> {
        self.transaction.take().unwrap().commit()?;
        for event in self.event_buffer.get_mut().flush() {
            self.event_hub.send_event(event);
        }
        Ok(())
    }

    fn rollback(&mut self) -> Result<()> {
        self.transaction.take().unwrap().rollback()?;
        self.event_buffer.get_mut().discard();
        Ok(())
    }

    fn create_savepoint(&self) -> Result<types::Savepoint> {
        self.transaction.as_ref().unwrap().create_savepoint()
    }

    fn restore_to_savepoint(&mut self, savepoint: types::Savepoint) -> Result<()> {
        let mut transaction = self.transaction.take().unwrap();
        transaction.restore_to_savepoint(savepoint)?;

        // Discard buffered events — savepoint restore invalidated them
        self.event_buffer.get_mut().discard();

        // Send Reset immediately (not buffered — UI must refresh now)
        self.event_hub.send_event(Event {
            origin: Origin::DirectAccess(DirectAccessEntity::All(AllEvent::Reset)),
            ids: vec![],
            data: None,
        });

        // Recreate the transaction after restoring to savepoint
        self.transaction = Some(transaction);

        Ok(())
    }

    fn store(&self) -> std::sync::Arc<common::database::Store> {
        self.context.get_store().clone()
    }
}

#[macros::uow_action(entity = "Root", action = "Get")]
#[macros::uow_action(entity = "Root", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Get")]
#[macros::uow_action(entity = "Document", action = "Update")]
#[macros::uow_action(entity = "Document", action = "GetRelationship")]
#[macros::uow_action(entity = "Document", action = "Snapshot")]
#[macros::uow_action(entity = "Document", action = "Restore")]
#[macros::uow_action(entity = "Frame", action = "Get")]
#[macros::uow_action(entity = "Frame", action = "GetRelationship")]
#[macros::uow_action(entity = "Block", action = "Get")]
#[macros::uow_action(entity = "Block", action = "GetMulti")]
#[macros::uow_action(entity = "Block", action = "Update")]
#[macros::uow_action(entity = "Block", action = "UpdateMulti")]
#[macros::uow_action(entity = "Block", action = "GetRelationship")]
impl ReplaceRangesUnitOfWorkTrait for ReplaceRangesUnitOfWork {}

pub struct ReplaceRangesUnitOfWorkFactory {
    context: DbContext,
    event_hub: Arc<EventHub>,
}

impl ReplaceRangesUnitOfWorkFactory {
    pub fn new(db_context: &DbContext, event_hub: &Arc<EventHub>) -> Self {
        ReplaceRangesUnitOfWorkFactory {
            context: db_context.clone(),
            event_hub: event_hub.clone(),
        }
    }
}

impl ReplaceRangesUnitOfWorkFactoryTrait for ReplaceRangesUnitOfWorkFactory {
    fn create(&self) -> Box<dyn ReplaceRangesUnitOfWorkTrait> {
        Box::new(ReplaceRangesUnitOfWork::new(&self.context, &self.event_hub))
    }
}
