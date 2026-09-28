//! The number each ordered list item wears, for the writers.

use std::collections::HashMap;

use common::database::Store;
use common::database::rope_helpers::block_document_position;
use common::types::EntityId;

/// The number each list item of a document wears, as an editor shows it (see
/// `TextList::item_marker`): its list's start, 1 unless the list says otherwise, plus its
/// place among the list's items in reading order.
///
/// A writer numbering a list from its start at each run of its items wrote the items after
/// a table or a paragraph splitting the list from the start again: an editor showed 5 and
/// 6, the save said 3 and 4, and the reload read a list starting at 3. A writer asks here
/// for the number a run opens with.
#[derive(Default)]
pub(crate) struct ListNumbers {
    of_block: HashMap<EntityId, i64>,
}

impl ListNumbers {
    /// Every list item's number in the document `store` holds: one walk of its blocks.
    pub(crate) fn new(store: &Store) -> Self {
        let mut items: HashMap<EntityId, Vec<(i64, EntityId)>> = HashMap::new();
        {
            let blocks = store.blocks.read();
            for (id, block) in blocks.iter() {
                if let Some(list_id) = block.list {
                    items
                        .entry(list_id)
                        .or_default()
                        .push((block_document_position(block, store), *id));
                }
            }
        }
        let lists = store.lists.read();
        let mut of_block = HashMap::new();
        for (list_id, mut list_items) in items {
            list_items.sort_unstable();
            let start = lists.get(&list_id).and_then(|list| list.start).unwrap_or(1);
            for (index, (_, block_id)) in list_items.into_iter().enumerate() {
                let index = i64::try_from(index).unwrap_or(i64::MAX);
                of_block.insert(block_id, start.saturating_add(index));
            }
        }
        ListNumbers { of_block }
    }

    /// The number the list item `block_id` wears; `None` for a block in no list.
    pub(crate) fn of(&self, block_id: EntityId) -> Option<i64> {
        self.of_block.get(&block_id).copied()
    }
}
