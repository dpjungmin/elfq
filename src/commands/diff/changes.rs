//! What changed, item by item: each paired item that moved to another block,
//! changed size or is edited, and each item that is new or gone.
//!
//! The unit test of [`Items::was`] is in the tests of `rows.rs`, which
//! build `Items` for the rows and for `was` alike.

use std::collections::HashMap;

use super::Build;
use super::blocks::Block;
use super::edited::Edits;
use super::pairing::{self, PairedBy, Pairing, Side};
use crate::binary::{Binary, Item, ItemKind};
use crate::commands::symbol::Names;

/// What happened to an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// than where they say where something is, or its code now uses
    /// different items than before (see `edited.rs`).
    Edited,
}

impl Status {
    /// The word for the status, which the first line counts, and rows and
    /// the JSON say.
    pub fn word(self) -> &'static str {
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

/// An item that changed.
#[derive(Debug, Clone, Copy)]
pub(super) struct Change {
    pub status: Status,
    /// The item in the old build, as an index into its items: `None` for a
    /// new item.
    pub old: Option<usize>,
    /// The item in the new build: `None` for a gone item.
    pub new: Option<usize>,
}

/// The items of both builds, paired, and what changed.
pub(super) struct Items<'a> {
    pub old: &'a Build,
    pub new: &'a Build,
    pub pairing: Pairing,
    /// The block that holds each item of the old build, as an index into
    /// the blocks, or `None` when none does.
    pub old_blocks: Vec<Option<usize>>,
    /// The same for the new build.
    pub new_blocks: Vec<Option<usize>>,
    /// The items that changed, in the order of the old build's items, then
    /// the new items.
    pub changes: Vec<Change>,
    /// How many paired items are the same: the same size, in the same
    /// block, and not edited.
    pub unchanged: usize,
}

/// Pairs the items of the builds and works out what changed.
pub(super) fn compare<'a>(old: &'a Build, new: &'a Build, blocks: &[Block]) -> Items<'a> {
    let side = |build: &'a Build| Side {
        binary: &build.binary,
        graph: &build.graph,
    };
    let pairing = pairing::pair(side(old), side(new));
    let old_blocks = item_blocks(&old.binary, blocks);
    let new_blocks = item_blocks(&new.binary, blocks);
    let edits = Edits::new(old, new, &pairing);
    let mut changes = Vec::new();
    let mut unchanged = 0;
    for (index, partner) in pairing.old_to_new.iter().enumerate() {
        let Some((partner, _)) = *partner else {
            changes.push(Change {
                status: Status::Gone,
                old: Some(index),
                new: None,
            });
            continue;
        };
        let (old_size, new_size) = (old.binary.items[index].size, new.binary.items[partner].size);
        let status = if old_blocks[index] != new_blocks[partner] {
            Status::Moved
        } else if new_size > old_size {
            Status::Grew
        } else if new_size < old_size {
            Status::Shrank
        } else if edits.edited(index, partner) {
            Status::Edited
        } else {
            unchanged += 1;
            continue;
        };
        changes.push(Change {
            status,
            old: Some(index),
            new: Some(partner),
        });
    }
    for (index, partner) in pairing.new_to_old.iter().enumerate() {
        if partner.is_none() {
            changes.push(Change {
                status: Status::New,
                old: None,
                new: Some(index),
            });
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
    pub fn inlined_into(
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
    pub fn item(&self, change: &Change) -> &'a Item {
        match (change.new, change.old) {
            (Some(index), _) => &self.new.binary.items[index],
            (None, Some(index)) => &self.old.binary.items[index],
            (None, None) => unreachable!("a change is about an item"),
        }
    }

    /// How many bytes the item grew by: less than 0 when it shrank, all its
    /// bytes when it is new, and less all its bytes when it is gone.
    pub fn change(&self, change: &Change) -> i128 {
        let size = |binary: &Binary, index: Option<usize>| {
            i128::from(index.map_or(0, |index| binary.items[index].size))
        };
        size(&self.new.binary, change.new) - size(&self.old.binary, change.old)
    }

    /// The block that a change counts in, in the CHANGE table: the new
    /// build's, or for a gone item, the old build's.
    pub fn block(&self, change: &Change) -> Option<usize> {
        match (change.new, change.old) {
            (Some(index), _) => self.new_blocks[index],
            (None, Some(index)) => self.old_blocks[index],
            (None, None) => None,
        }
    }

    /// How the rule that paired a change's items, if any.
    pub fn paired_by(&self, change: &Change) -> Option<PairedBy> {
        Some(self.pairing.old_to_new[change.old?]?.1)
    }

    /// The old build's name of a paired item, when the new build names it
    /// otherwise, and the two share no name, as a weak alias and the
    /// function it names do, or two functions that identical code folding
    /// made one of.
    pub fn was(&self, change: &Change) -> Option<&'a str> {
        let old = &self.old.binary.items[change.old?];
        let new = &self.new.binary.items[change.new?];
        let share = old.names().any(|a| new.names().any(|b| a == b));
        (!share).then_some(old.name.as_str())
    }
}
