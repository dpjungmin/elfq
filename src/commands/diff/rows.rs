//! What changed, item by item: each paired item that moved to another block,
//! changed size or is edited, and each item that is new or gone; and the
//! rows that the text shows for them, in the MOVED and CHANGE tables.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use super::Build;
use super::blocks::Block;
use super::edited::Edits;
use super::pairing::{self, PairedBy, Pairing, Side};
use crate::binary::{Binary, Item, ItemKind};
use crate::commands::symbol::Names;
use crate::commands::text::shown;
use crate::graph::Node;

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

    /// For each new item, the new item that heads its subtree: the highest
    /// of the new items above it in the new build's dominator tree, up to
    /// the first item that isn't new or is in another block. Likewise for
    /// each gone item in the old build's tree. Without a tree, each item
    /// heads its own. Indexed by change.
    pub fn heads(&self) -> Vec<usize> {
        // The change of each new item, and of each gone item.
        let mut new_changes = HashMap::new();
        let mut gone_changes = HashMap::new();
        for (at, change) in self.changes.iter().enumerate() {
            match (change.status, change.old, change.new) {
                (Status::New, _, Some(index)) => {
                    new_changes.insert(index, at);
                }
                (Status::Gone, Some(index), _) => {
                    gone_changes.insert(index, at);
                }
                _ => {}
            }
        }
        // The change of the item above a change's item in its subtree.
        let above = |at: usize| -> Option<usize> {
            let change = &self.changes[at];
            let (build, index, blocks, changes) = match change.status {
                Status::New => (self.new, change.new?, &self.new_blocks, &new_changes),
                Status::Gone => (self.old, change.old?, &self.old_blocks, &gone_changes),
                _ => return None,
            };
            let Some(Node::Item(parent)) = build.tree.as_ref()?.parent(Node::Item(index)) else {
                return None;
            };
            let &parent_change = changes.get(&parent)?;
            (blocks[parent] == blocks[index]).then_some(parent_change)
        };
        // Each path up is walked once: the heads found are kept.
        let mut heads: Vec<Option<usize>> = vec![None; self.changes.len()];
        for start in 0..self.changes.len() {
            let mut path = Vec::new();
            let mut at = start;
            let head = loop {
                if let Some(head) = heads[at] {
                    break head;
                }
                path.push(at);
                match above(at) {
                    Some(next) => at = next,
                    None => break at,
                }
            };
            for at in path {
                heads[at] = Some(head);
            }
        }
        heads
            .into_iter()
            .map(|head| head.unwrap_or_default())
            .collect()
    }

    /// The MOVED table's rows, largest first, before they are written: one
    /// for each item that moved, but one for all the items of whole
    /// sections that moved by one offset between the same two blocks,
    /// unless `all`.
    pub fn moved_rows(&self, all: bool) -> Vec<MovedRow> {
        let old = &self.old.binary;
        // The items that moved by each offset between each two blocks.
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let mut group_of = HashMap::new();
        for (at, change) in self.changes.iter().enumerate() {
            let (Status::Moved, Some(old_index), Some(new_index)) =
                (change.status, change.old, change.new)
            else {
                continue;
            };
            let offset = i128::from(self.new.binary.items[new_index].address)
                - i128::from(old.items[old_index].address);
            let key = (
                self.old_blocks[old_index],
                self.new_blocks[new_index],
                offset,
            );
            let group = *group_of.entry(key).or_insert_with(|| {
                groups.push(Vec::new());
                groups.len() - 1
            });
            groups[group].push(at);
        }
        // How many items each section of the old build holds.
        let mut in_section: HashMap<usize, usize> = HashMap::new();
        for item in &old.items {
            *in_section.entry(item.section).or_default() += 1;
        }
        let mut rows = Vec::new();
        for changes in groups {
            let mut sections: Vec<(u64, usize, usize)> = Vec::new();
            for &at in &changes {
                let item = &old.items[self.changes[at].old.unwrap_or_default()];
                match sections
                    .iter_mut()
                    .find(|(_, section, _)| *section == item.section)
                {
                    Some((start, _, count)) => {
                        *start = (*start).min(item.address);
                        *count += 1;
                    }
                    None => sections.push((item.address, item.section, 1)),
                }
            }
            let whole = sections
                .iter()
                .all(|&(_, section, count)| in_section[&section] == count);
            if all || changes.len() < 2 || !whole {
                rows.extend(changes.into_iter().map(MovedRow::Item));
            } else {
                sections.sort_unstable();
                let sections = sections
                    .into_iter()
                    .map(|(_, section, _)| old.sections[section].name.clone())
                    .collect();
                rows.push(MovedRow::Sections { changes, sections });
            }
        }
        rows.sort_by_key(|row| {
            let changes = row.changes();
            let size: u64 = changes
                .iter()
                .map(|&at| old.items[self.changes[at].old.unwrap_or_default()].size)
                .sum();
            let start = changes
                .iter()
                .map(|&at| old.items[self.changes[at].old.unwrap_or_default()].address)
                .min();
            (Reverse(size), start)
        });
        rows
    }

    /// The CHANGE table's rows, grouped by block in the order of `blocks`,
    /// each block's largest first: every item that changed but moved, and
    /// unless `all`, a new or gone item with the new or gone items in its
    /// block that only it keeps, as [`Items::heads`] gives them, as one row,
    /// rows folded into one, and the largest rows, as many as `top` shows,
    /// with every block that changed showing at least its largest, and a
    /// row for the rest of each block.
    pub fn change_rows(&self, heads: &[usize], blocks: usize, all: bool) -> Vec<ChangeRow> {
        // Each head's row, with the changes it stands for.
        let mut rows: Vec<ChangeRow> = Vec::new();
        let mut row_of = vec![None; self.changes.len()];
        for (at, change) in self.changes.iter().enumerate() {
            if change.status == Status::Moved {
                continue;
            }
            let head = if all { at } else { heads[at] };
            let row = *row_of[head].get_or_insert_with(|| {
                rows.push(ChangeRow::Item {
                    change: head,
                    kept: 0,
                    bytes: 0,
                });
                rows.len() - 1
            });
            if let ChangeRow::Item { kept, bytes, .. } = &mut rows[row] {
                *kept += usize::from(at != head);
                *bytes += self.change(change);
            }
        }
        if !all {
            rows = self.fold(rows);
        }
        let block = |row: &ChangeRow| match row {
            ChangeRow::Item { change, .. } => self.block(&self.changes[*change]),
            ChangeRow::Fold { block, .. } | ChangeRow::More { block, .. } => *block,
        };
        let mut ranked: Vec<(i128, Option<usize>, ChangeRow)> = rows
            .into_iter()
            .map(|row| (row.change().abs(), block(&row), row))
            .collect();
        // Largest first, and of the same size, edited items last, then by
        // name.
        let edited = |row: &ChangeRow| match row {
            ChangeRow::Item { change, .. } => self.changes[*change].status == Status::Edited,
            _ => false,
        };
        let rank = |(a, _, a_row): &(i128, Option<usize>, ChangeRow),
                    (b, _, b_row): &(i128, Option<usize>, ChangeRow)| {
            b.cmp(a)
                .then_with(|| edited(a_row).cmp(&edited(b_row)))
                .then_with(|| self.row_name(a_row).cmp(&self.row_name(b_row)))
        };
        let shown_rows = shown(all);
        if ranked.len() > shown_rows {
            // Only the largest rows and the largest of each block can be
            // shown, so only those are sorted, ahead of the rest, which are
            // only counted. Ties keep the order of the changes, as a stable
            // sort does.
            let mut order: Vec<usize> = (0..ranked.len()).collect();
            order.select_nth_unstable_by(shown_rows, |&a, &b| {
                rank(&ranked[a], &ranked[b]).then(a.cmp(&b))
            });
            let mut candidate = vec![false; ranked.len()];
            for &at in &order[..shown_rows] {
                candidate[at] = true;
            }
            let mut largest: HashMap<Option<usize>, usize> = HashMap::new();
            for at in 0..ranked.len() {
                let best = largest.entry(ranked[at].1).or_insert(at);
                if rank(&ranked[at], &ranked[*best]).is_lt() {
                    *best = at;
                }
            }
            for at in largest.into_values() {
                candidate[at] = true;
            }
            let (mut candidates, rest): (Vec<_>, Vec<_>) = ranked
                .into_iter()
                .zip(candidate)
                .partition(|&(_, candidate)| candidate);
            candidates.sort_by(|(a, _), (b, _)| rank(a, b));
            ranked = candidates
                .into_iter()
                .chain(rest)
                .map(|(row, _)| row)
                .collect();
        } else {
            ranked.sort_by(rank);
        }
        // The largest row of each block, then the largest of the rest.
        let mut shown = vec![all; ranked.len()];
        let mut firsts = HashSet::new();
        for (at, (_, block, _)) in ranked.iter().enumerate() {
            if firsts.insert(*block) {
                shown[at] = true;
            }
        }
        let mut room = shown_rows.saturating_sub(firsts.len());
        for is_shown in &mut shown {
            if room == 0 {
                break;
            }
            if !*is_shown {
                *is_shown = true;
                room -= 1;
            }
        }
        // A block with one row left shows it, since a row for the rest
        // would take as much room.
        let mut left: HashMap<Option<usize>, (usize, usize)> = HashMap::new();
        for (at, (_, block, _)) in ranked.iter().enumerate() {
            if !shown[at] {
                let (count, last) = left.entry(*block).or_default();
                *count += 1;
                *last = at;
            }
        }
        for (count, last) in left.into_values() {
            if count == 1 {
                shown[last] = true;
            }
        }
        // Each block's rows, in the order of the blocks, then the row for
        // the rest of them, with how many items they stand for.
        let mut ranked: Vec<_> = ranked.into_iter().zip(shown).collect();
        ranked.sort_by_key(|((_, block, _), _)| block.unwrap_or(blocks));
        let mut table = Vec::new();
        let mut ranked = ranked.into_iter().peekable();
        while let Some(((_, block, _), _)) = ranked.peek() {
            let block = *block;
            let (mut count, mut change) = (0, 0);
            while let Some(((_, _, row), shown)) =
                ranked.next_if(|((_, row_block, _), _)| *row_block == block)
            {
                if shown {
                    table.push(row);
                } else {
                    count += row.items();
                    change += row.change();
                }
            }
            if count > 0 {
                table.push(ChangeRow::More {
                    count,
                    change,
                    block,
                });
            }
        }
        table
    }

    /// `rows` with the rows of data objects without a name whose estimated
    /// size changed, and of new and gone items that share a name, each
    /// folded into one row per block, when there are several.
    fn fold(&self, rows: Vec<ChangeRow>) -> Vec<ChangeRow> {
        let keys: Vec<Option<FoldKey>> = rows
            .iter()
            .map(|row| match row {
                ChangeRow::Item {
                    change, kept: 0, ..
                } => self.fold_key(&self.changes[*change]),
                _ => None,
            })
            .collect();
        let mut counts: HashMap<FoldKey, usize> = HashMap::new();
        for key in keys.iter().flatten() {
            *counts.entry(key.clone()).or_default() += 1;
        }
        let mut folds: Vec<(FoldKey, Vec<ChangeRow>)> = Vec::new();
        let mut fold_of = HashMap::new();
        let mut unfolded = Vec::new();
        for (row, key) in rows.into_iter().zip(keys) {
            match key {
                Some(key) if counts[&key] > 1 => {
                    let fold = *fold_of.entry(key.clone()).or_insert_with(|| {
                        folds.push((key, Vec::new()));
                        folds.len() - 1
                    });
                    folds[fold].1.push(row);
                }
                _ => unfolded.push(row),
            }
        }
        for (key, folded) in folds {
            let count = |status: Status| {
                folded
                    .iter()
                    .filter(|row| match row {
                        ChangeRow::Item { change, .. } => self.changes[*change].status == status,
                        _ => false,
                    })
                    .count()
            };
            let (new, gone) = (count(Status::New), count(Status::Gone));
            let change = folded.iter().map(ChangeRow::change).sum();
            unfolded.push(match key {
                FoldKey::Unnamed(block) => ChangeRow::Fold {
                    fold: Fold::Unnamed {
                        count: folded.len(),
                    },
                    change,
                    block,
                },
                FoldKey::Name(block, name) => ChangeRow::Fold {
                    fold: Fold::Name {
                        name: name.to_string(),
                        new,
                        gone,
                    },
                    change,
                    block,
                },
            });
        }
        unfolded
    }

    /// Which row a change's row folds into, if any: data without a name
    /// whose estimated size changed folds with the others of its block, and
    /// a new or gone item that keeps no other with those of its name.
    fn fold_key(&self, change: &Change) -> Option<FoldKey<'a>> {
        let item = self.item(change);
        let made = pairing::is_made_name(item, &item.name);
        let block = self.block(change);
        match change.status {
            Status::Grew | Status::Shrank if item.kind == ItemKind::Data && made => {
                let old = &self.old.binary.items[change.old?];
                (old.size_estimated || item.size_estimated).then_some(FoldKey::Unnamed(block))
            }
            Status::New | Status::Gone if !made => Some(FoldKey::Name(block, &item.name)),
            _ => None,
        }
    }

    /// The name that orders rows of the same size: the item's, or the fold's
    /// words.
    fn row_name(&self, row: &ChangeRow) -> Cow<'a, str> {
        match row {
            ChangeRow::Item { change, .. } => {
                Cow::Borrowed(&self.item(&self.changes[*change]).name)
            }
            ChangeRow::Fold { fold, .. } => Cow::Owned(fold.text()),
            ChangeRow::More { .. } => Cow::Borrowed(""),
        }
    }
}

/// A row of the MOVED table.
pub(super) enum MovedRow {
    /// An item that moved, as an index into the changes.
    Item(usize),
    /// All the items of whole sections, which moved by one offset between
    /// the same two blocks: the changes, and the old build's sections, in
    /// address order.
    Sections {
        changes: Vec<usize>,
        sections: Vec<String>,
    },
}

impl MovedRow {
    /// The changes that the row stands for.
    pub fn changes(&self) -> &[usize] {
        match self {
            MovedRow::Item(change) => std::slice::from_ref(change),
            MovedRow::Sections { changes, .. } => changes,
        }
    }
}

/// A row of the CHANGE table.
pub(super) enum ChangeRow {
    /// An item that changed, as an index into the changes. A new or gone
    /// item stands for the `kept` new or gone items in its block that only
    /// it keeps too, and `bytes` is the change of them all.
    Item {
        change: usize,
        kept: usize,
        bytes: i128,
    },
    /// Several changes in a block, folded into one row.
    Fold {
        fold: Fold,
        change: i128,
        block: Option<usize>,
    },
    /// The rest of a block's rows, which the table leaves out: how many
    /// items they stand for, and their change.
    More {
        count: usize,
        change: i128,
        block: Option<usize>,
    },
}

impl ChangeRow {
    /// The row's change in bytes.
    pub fn change(&self) -> i128 {
        match self {
            ChangeRow::Item { bytes, .. } => *bytes,
            ChangeRow::Fold { change, .. } | ChangeRow::More { change, .. } => *change,
        }
    }

    /// How many items the row stands for, each of which has a row of its
    /// own with `--all`.
    fn items(&self) -> usize {
        match self {
            ChangeRow::Item { kept, .. } => kept + 1,
            ChangeRow::Fold {
                fold: Fold::Unnamed { count },
                ..
            }
            | ChangeRow::More { count, .. } => *count,
            ChangeRow::Fold {
                fold: Fold::Name { new, gone, .. },
                ..
            } => new + gone,
        }
    }
}

/// What a folded row stands for.
pub(super) enum Fold {
    /// Data objects without a name whose estimated size changed, as it
    /// does when what follows them moves.
    Unnamed { count: usize },
    /// New and gone items of one name.
    Name {
        name: String,
        new: usize,
        gone: usize,
    },
}

impl Fold {
    /// What the row says: "49 data objects without a name changed size",
    /// or "blanks.1 (2 new items with this name)".
    pub fn text(&self) -> String {
        match self {
            Fold::Unnamed { count } => format!("{count} data objects without a name changed size"),
            Fold::Name { name, new, gone } => {
                let counts: Vec<String> = [(new, "new"), (gone, "gone")]
                    .into_iter()
                    .filter(|(count, _)| **count > 0)
                    .map(|(count, word)| format!("{count} {word}"))
                    .collect();
                format!("{name} ({} items with this name)", counts.join(" and "))
            }
        }
    }
}

/// The rows that fold into one, by block.
#[derive(Clone, PartialEq, Eq, Hash)]
enum FoldKey<'a> {
    Unnamed(Option<usize>),
    Name(Option<usize>, &'a str),
}

#[cfg(test)]
mod tests {
    use object::SectionKind;

    use super::*;
    use crate::binary::{Section, Source};
    use crate::graph::Graph;

    /// A build whose `.text` holds a function of each size, with the first
    /// of its names as its name, one after another.
    fn build(functions: &[(&[&str], u64)]) -> Build {
        let text = Section {
            name: ".text".to_string(),
            address: 0x1000,
            size: 0x1_0000,
            kind: SectionKind::Text,
            allocated: true,
            file_range: None,
            entry_size: 0,
            load_address: None,
        };
        let mut address = 0x1000;
        let mut items = Vec::new();
        for &(names, size) in functions {
            items.push(Item {
                name: names[0].to_string(),
                aliases: names[1..].iter().map(|name| name.to_string()).collect(),
                kind: ItemKind::Function,
                address,
                size,
                size_estimated: false,
                section: 0,
                source: Source::SymbolTable,
            });
            address += size;
        }
        Build {
            binary: Binary::for_tests(vec![text], items, Vec::new()),
            graph: Graph::default(),
            tree: None,
        }
    }

    /// The `changes` from `old` to `new`, all in one block.
    fn changed<'a>(old: &'a Build, new: &'a Build, changes: Vec<Change>) -> Items<'a> {
        let (old_count, new_count) = (old.binary.items.len(), new.binary.items.len());
        Items {
            old,
            new,
            pairing: Pairing {
                old_to_new: vec![None; old_count],
                new_to_old: vec![None; new_count],
            },
            old_blocks: vec![Some(0); old_count],
            new_blocks: vec![Some(0); new_count],
            changes,
            unchanged: 0,
        }
    }

    fn change(status: Status, old: Option<usize>, new: Option<usize>) -> Change {
        Change { status, old, new }
    }

    /// The rows of CHANGE, each as its change and what it says.
    fn table(items: &Items, heads: &[usize]) -> Vec<(i128, String)> {
        items
            .change_rows(heads, 1, false)
            .iter()
            .map(|row| {
                let text = match row {
                    ChangeRow::Item { change, .. } => {
                        items.item(&items.changes[*change]).name.clone()
                    }
                    ChangeRow::Fold { fold, .. } => fold.text(),
                    ChangeRow::More { count, .. } => format!("{count} more"),
                };
                (row.change(), text)
            })
            .collect()
    }

    #[test]
    fn was_names_a_copy_but_not_an_alias() {
        // A copy of dev_read, and a function that identical code folding
        // made of several, named after another of them in the new build.
        let old = build(&[(&["dev_read"], 10), (&["Patch::grow", "Value::grow"], 10)]);
        let new = build(&[
            (&["dev_read.constprop.0"], 8),
            (&["Dependency::grow", "Value::grow"], 8),
        ]);
        let changes = vec![
            change(Status::Shrank, Some(0), Some(0)),
            change(Status::Shrank, Some(1), Some(1)),
        ];
        let items = changed(&old, &new, changes);
        assert_eq!(items.was(&items.changes[0]), Some("dev_read"));
        assert_eq!(items.was(&items.changes[1]), None);
    }

    #[test]
    fn new_and_gone_items_of_one_name_fold_into_a_row() {
        let old = build(&[(&["helper"], 4)]);
        let new = build(&[(&["helper"], 6), (&["helper"], 8), (&["main"], 2)]);
        let changes = vec![
            change(Status::Gone, Some(0), None),
            change(Status::New, None, Some(0)),
            change(Status::New, None, Some(1)),
            change(Status::New, None, Some(2)),
        ];
        let items = changed(&old, &new, changes);
        assert_eq!(
            table(&items, &[0, 1, 2, 3]),
            [
                (
                    10,
                    "helper (2 new and 1 gone items with this name)".to_string()
                ),
                (2, "main".to_string())
            ]
        );
    }

    #[test]
    fn edited_rows_come_last_of_the_same_change() {
        // An edited function, and a new one of no size, which its name
        // would put after it.
        let old = build(&[(&["b_edited"], 4)]);
        let new = build(&[(&["b_edited"], 4), (&["c_empty"], 0)]);
        let changes = vec![
            change(Status::Edited, Some(0), Some(0)),
            change(Status::New, None, Some(1)),
        ];
        let items = changed(&old, &new, changes);
        assert_eq!(
            table(&items, &[0, 1]),
            [(0, "c_empty".to_string()), (0, "b_edited".to_string())]
        );
    }

    #[test]
    fn the_rest_of_a_block_counts_its_items_but_not_one_row() {
        // 32 new functions of 100 bytes down to 70, and of 60, which keeps 4
        // new functions of a byte: 30 rows, and the rest of the block
        // stands for 6 items, as --all would show.
        let names: Vec<String> = (0..31).map(|at| format!("f{at}")).collect();
        let names: Vec<[&str; 1]> = names.iter().map(|name| [name.as_str()]).collect();
        let mut functions: Vec<(&[&str], u64)> = (0..31)
            .map(|at| (&names[at][..], 100 - at as u64))
            .collect();
        functions.push((&["g"], 60));
        functions.extend([(&["kept"][..], 1); 4]);
        let (old, new) = (build(&[]), build(&functions));
        let changes: Vec<Change> = (0..functions.len())
            .map(|index| change(Status::New, None, Some(index)))
            .collect();
        let items = changed(&old, &new, changes);
        let heads: Vec<usize> = (0..36).map(|at| at.min(31)).collect();
        let rows = table(&items, &heads);
        assert_eq!(rows.len(), 31);
        assert_eq!(rows[29], (71, "f29".to_string()));
        assert_eq!(rows[30], (134, "6 more".to_string()));
        // With one row left, the row takes the place of "1 more".
        let new = build(&functions[..31]);
        let changes: Vec<Change> = (0..31)
            .map(|index| change(Status::New, None, Some(index)))
            .collect();
        let items = changed(&old, &new, changes);
        let rows = table(&items, &(0..31).collect::<Vec<_>>());
        assert_eq!(rows.len(), 31);
        assert_eq!(rows[30], (70, "f30".to_string()));
    }
}
