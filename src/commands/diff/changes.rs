//! What changed, item by item: each paired item that moved to another block,
//! changed size or is edited, and each item that is new or gone.
//!
//! The unit test of [`Items::was`] is in the tests of `rows.rs`, which
//! build `Items` for the rows and for `was` alike.

use std::collections::HashMap;

use super::Build;
use super::blocks::Block;
use super::edited::Edits;
use super::pairing::{self, PairedBy, Pairing};
use crate::binary::{Binary, Item, ItemKind};
use crate::commands::symbol::Names;

/// What happened to an item. The first line of the text counts the items
/// of each status in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Status {
    /// It is in another block of memory.
    Moved,
    /// Only the new build has it.
    New,
    /// Only the old build has it.
    Gone,
    /// It is larger, in the same block.
    Grew,
    /// It is smaller, in the same block.
    Shrank,
    /// It has the same size, in the same block, but its bytes changed other
    /// than where they say where something is, or its code uses different
    /// items in the new build than in the old (see `edited.rs`).
    Edited,
}

impl Status {
    /// The word for the status, which the first line counts, and rows and
    /// the JSON say.
    pub(super) fn word(self) -> &'static str {
        match self {
            Status::Moved => "moved",
            Status::New => "new",
            Status::Gone => "gone",
            Status::Grew => "grew",
            Status::Shrank => "shrank",
            Status::Edited => "edited",
        }
    }
}

/// What happened to partners that changed: the statuses of an item that
/// both builds have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PartnerStatus {
    /// [`Status::Moved`].
    Moved,
    /// [`Status::Grew`].
    Grew,
    /// [`Status::Shrank`].
    Shrank,
    /// [`Status::Edited`].
    Edited,
}

impl From<PartnerStatus> for Status {
    fn from(status: PartnerStatus) -> Status {
        match status {
            PartnerStatus::Moved => Status::Moved,
            PartnerStatus::Grew => Status::Grew,
            PartnerStatus::Shrank => Status::Shrank,
            PartnerStatus::Edited => Status::Edited,
        }
    }
}

/// An item that changed, with the item in each build that has it, as an
/// index into that build's items.
#[derive(Debug, Clone, Copy)]
pub(super) enum Change {
    /// Partners that moved to another block, changed size or are edited,
    /// with the rule that paired them.
    Partners {
        old: usize,
        new: usize,
        status: PartnerStatus,
        paired_by: PairedBy,
    },
    /// An item that only the new build has.
    New { new: usize },
    /// An item that only the old build has.
    Gone { old: usize },
}

impl Change {
    /// What happened to the item.
    pub(super) fn status(&self) -> Status {
        match *self {
            Change::Partners { status, .. } => status.into(),
            Change::New { .. } => Status::New,
            Change::Gone { .. } => Status::Gone,
        }
    }

    /// The item in the old build, unless it is new: for the JSON's keys
    /// and the cells that say each build's.
    pub(super) fn old_index(&self) -> Option<usize> {
        match *self {
            Change::Partners { old, .. } | Change::Gone { old } => Some(old),
            Change::New { .. } => None,
        }
    }

    /// The item in the new build, unless it is gone.
    pub(super) fn new_index(&self) -> Option<usize> {
        match *self {
            Change::Partners { new, .. } | Change::New { new } => Some(new),
            Change::Gone { .. } => None,
        }
    }

    /// The item in the old build, when it is gone.
    pub(super) fn gone_index(&self) -> Option<usize> {
        match *self {
            Change::Gone { old } => Some(old),
            Change::Partners { .. } | Change::New { .. } => None,
        }
    }

    /// The rule that paired the item's partners, when both builds have it.
    pub(super) fn paired_by(&self) -> Option<PairedBy> {
        match *self {
            Change::Partners { paired_by, .. } => Some(paired_by),
            Change::New { .. } | Change::Gone { .. } => None,
        }
    }
}

/// The items of both builds, paired, and what changed.
pub(super) struct Items<'a> {
    pub(super) old: &'a Build,
    pub(super) new: &'a Build,
    pub(super) pairing: Pairing,
    /// The block that holds each item of the old build, as an index into
    /// the blocks, or `None` when none does.
    pub(super) old_blocks: Vec<Option<usize>>,
    /// The same for the new build.
    pub(super) new_blocks: Vec<Option<usize>>,
    /// The items that changed, in the order of the old build's items, then
    /// the new items.
    pub(super) changes: Vec<Change>,
    /// How many paired items are the same: the same size, in the same
    /// block, and not edited.
    pub(super) unchanged: usize,
}

/// Pairs the items of the builds and works out what changed.
pub(super) fn compare<'a>(old: &'a Build, new: &'a Build, blocks: &[Block]) -> Items<'a> {
    let pairing = pairing::pair(old, new);
    let old_blocks = item_blocks(&old.binary, blocks);
    let new_blocks = item_blocks(&new.binary, blocks);
    let edits = Edits::new(old, new, &pairing);
    let mut changes = Vec::new();
    let mut unchanged = 0;
    for (index, partner) in pairing.old_to_new.iter().enumerate() {
        let Some((partner, paired_by)) = *partner else {
            changes.push(Change::Gone { old: index });
            continue;
        };
        let (old_size, new_size) = (old.binary.items[index].size, new.binary.items[partner].size);
        let status = if old_blocks[index] != new_blocks[partner] {
            PartnerStatus::Moved
        } else if new_size > old_size {
            PartnerStatus::Grew
        } else if new_size < old_size {
            PartnerStatus::Shrank
        } else if edits.edited(index, partner) {
            PartnerStatus::Edited
        } else {
            unchanged += 1;
            continue;
        };
        changes.push(Change::Partners {
            old: index,
            new: partner,
            status,
            paired_by,
        });
    }
    for (index, partner) in pairing.new_to_old.iter().enumerate() {
        if partner.is_none() {
            changes.push(Change::New { new: index });
        }
    }
    Items {
        old,
        new,
        pairing,
        old_blocks,
        new_blocks,
        changes,
        unchanged,
    }
}

/// The block that holds each item of `binary`, by where it starts.
fn item_blocks(binary: &Binary, blocks: &[Block]) -> Vec<Option<usize>> {
    binary
        .items
        .iter()
        .map(|item| {
            let after = blocks.partition_point(|block| block.range.start <= item.address);
            let block = after.checked_sub(1)?;
            blocks[block].range.contains(item.address).then_some(block)
        })
        .collect()
}

impl<'a> Items<'a> {
    /// Where the compiler inlined each of the `gone` functions, by their
    /// indices in the old build, as the new build's DWARF says, when it
    /// inlined every copy: for a function whose name finds no item of the
    /// new build, as `elfq NEW why NAME` would say. The functions it inlined
    /// each into are indices into the new build's items. The DWARF is read
    /// only when such a name finds nothing.
    pub(super) fn inlined_into(
        &self,
        new_names: &Names,
        gone: impl IntoIterator<Item = usize>,
    ) -> HashMap<usize, Vec<usize>> {
        if !self.new.binary.has_dwarf {
            return HashMap::new();
        }
        gone.into_iter()
            .filter_map(|index| {
                let item = &self.old.binary.items[index];
                if item.kind != ItemKind::Function || pairing::is_made_name(item, &item.name) {
                    return None;
                }
                // A name that finds an item, or several, names no function
                // that is only inlined: one build's
                // `std::fs::OpenOptions::_open` finds the other's
                // `<std::fs::OpenOptions>::_open`, which Rust demangles
                // otherwise.
                if !new_names.nodes(&item.name).is_empty() {
                    return None;
                }
                Some((index, new_names.inlined_everywhere(&item.name)?.into))
            })
            .collect()
    }

    /// The item that a change is about: the new build's, or for a gone
    /// item, the old build's.
    pub(super) fn item(&self, change: &Change) -> &'a Item {
        match *change {
            Change::Partners { new, .. } | Change::New { new } => &self.new.binary.items[new],
            Change::Gone { old } => &self.old.binary.items[old],
        }
    }

    /// How many bytes the item grew by: less than 0 when it shrank, all its
    /// bytes when it is new, and less all its bytes when it is gone.
    pub(super) fn delta(&self, change: &Change) -> i128 {
        let old_size = |old: usize| i128::from(self.old.binary.items[old].size);
        let new_size = |new: usize| i128::from(self.new.binary.items[new].size);
        match *change {
            Change::Partners { old, new, .. } => new_size(new) - old_size(old),
            Change::New { new } => new_size(new),
            Change::Gone { old } => -old_size(old),
        }
    }

    /// The block that a change counts in, in the CHANGE table: the new
    /// build's, or for a gone item, the old build's.
    pub(super) fn block(&self, change: &Change) -> Option<usize> {
        match *change {
            Change::Partners { new, .. } | Change::New { new } => self.new_blocks[new],
            Change::Gone { old } => self.old_blocks[old],
        }
    }

    /// The old build's name of a paired item, when the new build names it
    /// otherwise, and the two share no name, as a weak alias and the
    /// function it names do, or two functions that identical code folding
    /// made one of.
    pub(super) fn was(&self, change: &Change) -> Option<&'a str> {
        let Change::Partners {
            old: old_index,
            new: new_index,
            ..
        } = *change
        else {
            return None;
        };
        let (old_item, new_item) = (
            &self.old.binary.items[old_index],
            &self.new.binary.items[new_index],
        );
        let share = old_item.names().any(|a| new_item.names().any(|b| a == b));
        (!share).then_some(old_item.name.as_str())
    }
}
