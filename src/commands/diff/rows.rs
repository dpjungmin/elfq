//! The rows that the text shows of what changed, in the MOVED and CHANGE
//! tables: the methods of [`Items`] that pick and order them, with the
//! steps of [`Items::change_rows`], and the rows themselves.

use std::borrow::Cow;
use std::cmp::{Ordering, Reverse};
use std::collections::{HashMap, HashSet};

use super::changes::{Change, Items, PartnerStatus, Status};
use super::pairing;
use crate::binary::ItemKind;
use crate::commands::text::{Detail, shown};
use crate::graph::Node;

impl<'a> Items<'a> {
    /// For each new item, the new item that heads its subtree: the highest
    /// of the new items above it in the new build's dominator tree, up to
    /// the first item that isn't new or is in another block. Likewise for
    /// each gone item in the old build's tree. Without a tree, each item
    /// heads its own. Indexed by change.
    pub(super) fn heads(&self) -> Vec<usize> {
        // The change of each new item, and of each gone item.
        let mut new_changes = HashMap::new();
        let mut gone_changes = HashMap::new();
        for (at, change) in self.changes.iter().enumerate() {
            match *change {
                Change::New { new } => {
                    new_changes.insert(new, at);
                }
                Change::Gone { old } => {
                    gone_changes.insert(old, at);
                }
                Change::Partners { .. } => {}
            }
        }
        // The change of the item above a change's item in its subtree.
        let above = |at: usize| -> Option<usize> {
            let (build, index, blocks, changes) = match self.changes[at] {
                Change::New { new } => (self.new, new, &self.new_blocks, &new_changes),
                Change::Gone { old } => (self.old, old, &self.old_blocks, &gone_changes),
                Change::Partners { .. } => return None,
            };
            let Some(Node::Item(parent)) = build.tree.as_ref()?.parent(Node::Item(index)) else {
                return None;
            };
            let &parent_change = changes.get(&parent)?;
            (blocks[parent] == blocks[index]).then_some(parent_change)
        };
        // Each path up is walked once: the heads found are kept.
        let mut found: Vec<Option<usize>> = vec![None; self.changes.len()];
        let mut heads = Vec::with_capacity(self.changes.len());
        for start in 0..self.changes.len() {
            let mut path = Vec::new();
            let mut at = start;
            let head = loop {
                if let Some(head) = found[at] {
                    break head;
                }
                path.push(at);
                match above(at) {
                    Some(next) => at = next,
                    None => break at,
                }
            };
            for at in path {
                found[at] = Some(head);
            }
            heads.push(head);
        }
        heads
    }

    /// The MOVED table's rows, largest first, before they are written: one
    /// for each item that moved, but one for all the items of whole
    /// sections that moved by one offset between the same two blocks,
    /// unless `detail` is [`Detail::All`].
    pub(super) fn moved_rows(&self, detail: Detail) -> Vec<MovedRow> {
        let old = &self.old.binary;
        // The items that moved by each offset between each two blocks.
        let mut groups: Vec<Vec<MovedItem>> = Vec::new();
        let mut group_of = HashMap::new();
        for (at, change) in self.changes.iter().enumerate() {
            let Change::Partners {
                old: old_index,
                new: new_index,
                status: PartnerStatus::Moved,
                ..
            } = *change
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
            groups[group].push(MovedItem {
                at,
                old: old_index,
                new: new_index,
            });
        }
        // How many items each section of the old build holds.
        let mut in_section: HashMap<usize, usize> = HashMap::new();
        for item in &old.items {
            *in_section.entry(item.section).or_default() += 1;
        }
        let mut rows = Vec::new();
        for moved in groups {
            let mut sections: Vec<(u64, usize, usize)> = Vec::new();
            for item in moved.iter().map(|item| &old.items[item.old]) {
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
            if detail == Detail::All || moved.len() < 2 || !whole {
                rows.extend(moved.into_iter().map(MovedRow::Item));
            } else {
                sections.sort_unstable();
                let sections = sections
                    .into_iter()
                    .map(|(_, section, _)| old.sections[section].name.clone())
                    .collect();
                rows.push(MovedRow::Sections { moved, sections });
            }
        }
        // What orders the rows: the bytes of their items in the old build,
        // largest first, then where the first of them starts there.
        let order = |moved: &[MovedItem]| {
            let size: u64 = moved.iter().map(|item| old.items[item.old].size).sum();
            let start = moved.iter().map(|item| old.items[item.old].address).min();
            (Reverse(size), start)
        };
        rows.sort_by_cached_key(|row| order(row.moved()));
        rows
    }

    /// The CHANGE table's rows, grouped by block in the order of the blocks,
    /// each block's largest first: every item that changed but moved, and
    /// unless `detail` is [`Detail::All`], a new or gone item with the new
    /// or gone items in its block that only it keeps, as [`Items::heads`]
    /// gives them, as one row, rows folded into one, and the largest rows,
    /// as many as `top` shows, with every block that changed showing at
    /// least its largest, and a row for the rest of each block.
    pub(super) fn change_rows(&self, heads: &[usize], detail: Detail) -> Vec<ChangeRow> {
        let rows = self.rows_by_head(heads, detail);
        let rows = match detail {
            Detail::All => rows,
            Detail::Brief => self.fold(rows),
        };
        let ranked = self.rank(rows, shown(detail));
        let shown_rows = rows_shown(&ranked, detail);
        by_block(ranked, shown_rows)
    }

    /// A row for each change but those that moved, or unless `detail` is
    /// [`Detail::All`], for each head in `heads`, which stands for the
    /// changes under it too, with their deltas added up; in the order of
    /// each row's first change.
    fn rows_by_head(&self, heads: &[usize], detail: Detail) -> Vec<ChangeRow> {
        let mut rows: Vec<ChangeRow> = Vec::new();
        let mut row_of = vec![None; self.changes.len()];
        for (at, change) in self.changes.iter().enumerate() {
            if change.status() == Status::Moved {
                continue;
            }
            let head = match detail {
                Detail::All => at,
                Detail::Brief => heads[at],
            };
            let row = *row_of[head].get_or_insert_with(|| {
                rows.push(ChangeRow::Item {
                    at: head,
                    kept: 0,
                    delta: 0,
                });
                rows.len() - 1
            });
            if let ChangeRow::Item { kept, delta, .. } = &mut rows[row] {
                *kept += usize::from(at != head);
                *delta += self.delta(change);
            }
        }
        rows
    }

    /// `rows` with the rows of data objects without a name whose estimated
    /// size changed, and of new and gone items that share a name, each
    /// folded into one row per block, when there are several.
    fn fold(&self, rows: Vec<ChangeRow>) -> Vec<ChangeRow> {
        let keys: Vec<Option<FoldKey>> = rows
            .iter()
            .map(|row| match row {
                ChangeRow::Item { at, kept: 0, .. } => self.fold_key(&self.changes[*at]),
                // A row that stands for the items it keeps too stays a row;
                // rows_by_head makes no Fold or More rows.
                ChangeRow::Item { .. } | ChangeRow::Fold { .. } | ChangeRow::More { .. } => None,
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
                        ChangeRow::Item { at, .. } => self.changes[*at].status() == status,
                        ChangeRow::Fold { .. } | ChangeRow::More { .. } => false,
                    })
                    .count()
            };
            let (new, gone) = (count(Status::New), count(Status::Gone));
            let delta = folded.iter().map(ChangeRow::delta).sum();
            unfolded.push(match key {
                FoldKey::Unnamed(block) => ChangeRow::Fold {
                    fold: Fold::Unnamed {
                        count: folded.len(),
                    },
                    delta,
                    block,
                },
                FoldKey::Name(block, name) => ChangeRow::Fold {
                    fold: Fold::Name {
                        name: name.to_string(),
                        new,
                        gone,
                    },
                    delta,
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
        match *change {
            Change::Partners { old, status, .. } => {
                let resized = match status {
                    PartnerStatus::Grew | PartnerStatus::Shrank => true,
                    PartnerStatus::Moved | PartnerStatus::Edited => false,
                };
                let estimated = || self.old.binary.items[old].size_estimated || item.size_estimated;
                (resized && item.kind == ItemKind::Data && made && estimated())
                    .then_some(FoldKey::Unnamed(block))
            }
            Change::New { .. } | Change::Gone { .. } => {
                (!made).then_some(FoldKey::Name(block, &item.name))
            }
        }
    }

    /// The rows, best first, each with what ranks it (see
    /// [`Ranked::order`]). When there are more than the table has room for,
    /// `room`, only the largest and the largest of each block are sorted,
    /// ahead of the rest. Those are all the rows that [`rows_shown`] shows,
    /// except a block's one row left, which ranks below its block's sorted
    /// rows and so still comes after them.
    fn rank(&self, rows: Vec<ChangeRow>, room: usize) -> Vec<Ranked<'a>> {
        let mut ranked: Vec<Ranked> = rows.into_iter().map(|row| self.ranked(row)).collect();
        if ranked.len() <= room {
            ranked.sort_by(Ranked::order);
            return ranked;
        }
        // Ties keep the order of the changes, as a stable sort does.
        let mut positions: Vec<usize> = (0..ranked.len()).collect();
        positions
            .select_nth_unstable_by(room, |&a, &b| ranked[a].order(&ranked[b]).then(a.cmp(&b)));
        let mut candidate = vec![false; ranked.len()];
        for &position in &positions[..room] {
            candidate[position] = true;
        }
        let mut largest: HashMap<Option<usize>, usize> = HashMap::new();
        for (position, row) in ranked.iter().enumerate() {
            let best = largest.entry(row.block).or_insert(position);
            if row.order(&ranked[*best]).is_lt() {
                *best = position;
            }
        }
        for position in largest.into_values() {
            candidate[position] = true;
        }
        let (mut candidates, rest): (Vec<_>, Vec<_>) = ranked
            .into_iter()
            .zip(candidate)
            .partition(|&(_, candidate)| candidate);
        candidates.sort_by(|(a, _), (b, _)| a.order(b));
        candidates
            .into_iter()
            .chain(rest)
            .map(|(row, _)| row)
            .collect()
    }

    /// `row`, with what ranks it among the others.
    fn ranked(&self, row: ChangeRow) -> Ranked<'a> {
        let (edited, name, block) = match &row {
            ChangeRow::Item { at, .. } => {
                let change = &self.changes[*at];
                let edited = change.status() == Status::Edited;
                (
                    edited,
                    Cow::Borrowed(self.item(change).name.as_str()),
                    self.block(change),
                )
            }
            ChangeRow::Fold { fold, block, .. } => (false, Cow::Owned(fold.text()), *block),
            ChangeRow::More { block, .. } => (false, Cow::Borrowed(""), *block),
        };
        Ranked {
            bytes: row.delta().unsigned_abs(),
            edited,
            name,
            block,
            row,
        }
    }
}

/// Which of the `ranked` rows the table shows: the largest of each block,
/// then the largest of the rest, as many as it has room for, or with
/// [`Detail::All`], every one; and the row of a block that has only one
/// left, since a row for the rest would take as much room.
fn rows_shown(ranked: &[Ranked], detail: Detail) -> Vec<bool> {
    // The largest row of each block, then the largest of the rest.
    let mut shown_rows = vec![detail == Detail::All; ranked.len()];
    let mut firsts = HashSet::new();
    for (position, row) in ranked.iter().enumerate() {
        if firsts.insert(row.block) {
            shown_rows[position] = true;
        }
    }
    let mut room = shown(detail).saturating_sub(firsts.len());
    for is_shown in &mut shown_rows {
        if room == 0 {
            break;
        }
        if !*is_shown {
            *is_shown = true;
            room -= 1;
        }
    }
    // A block with one row left shows it.
    let mut left: HashMap<Option<usize>, (usize, usize)> = HashMap::new();
    for (position, row) in ranked.iter().enumerate() {
        if !shown_rows[position] {
            let (count, last) = left.entry(row.block).or_default();
            *count += 1;
            *last = position;
        }
    }
    for (count, last) in left.into_values() {
        if count == 1 {
            shown_rows[last] = true;
        }
    }
    shown_rows
}

/// The `ranked` rows that `shown_rows` marks, grouped by block in the order
/// of the blocks, and those in no block last, each block's in ranked order,
/// then a row for the rest of the block's rows, with how many items they
/// stand for and their delta.
fn by_block(ranked: Vec<Ranked>, shown_rows: Vec<bool>) -> Vec<ChangeRow> {
    let mut rows: Vec<(Ranked, bool)> = ranked.into_iter().zip(shown_rows).collect();
    rows.sort_by_key(|(row, _)| (row.block.is_none(), row.block));
    let mut table = Vec::new();
    let mut rows = rows.into_iter().peekable();
    while let Some((first, _)) = rows.peek() {
        let block = first.block;
        let (mut count, mut delta) = (0, 0);
        while let Some((Ranked { row, .. }, is_shown)) =
            rows.next_if(|(next, _)| next.block == block)
        {
            if is_shown {
                table.push(row);
            } else {
                count += row.items();
                delta += row.delta();
            }
        }
        if count > 0 {
            table.push(ChangeRow::More {
                count,
                delta,
                block,
            });
        }
    }
    table
}

/// A row of the MOVED table.
pub(super) enum MovedRow {
    /// An item that moved.
    Item(MovedItem),
    /// All the items of whole sections, which moved by one offset between
    /// the same two blocks, and the old build's sections, in address order.
    Sections {
        moved: Vec<MovedItem>,
        sections: Vec<String>,
    },
}

/// An item that moved to another block: its place in the changes, and the
/// item in each build, as an index into its items.
#[derive(Debug, Clone, Copy)]
pub(super) struct MovedItem {
    pub(super) at: usize,
    pub(super) old: usize,
    pub(super) new: usize,
}

impl MovedRow {
    /// The items that the row stands for.
    pub(super) fn moved(&self) -> &[MovedItem] {
        match self {
            MovedRow::Item(item) => std::slice::from_ref(item),
            MovedRow::Sections { moved, .. } => moved,
        }
    }
}

/// A row of the CHANGE table.
pub(super) enum ChangeRow {
    /// An item that changed, by its place in the changes. A new or gone
    /// item stands for the `kept` new or gone items in its block that only
    /// it keeps too, and `delta` adds up the deltas of them all.
    Item { at: usize, kept: usize, delta: i128 },
    /// Several changes in a block, folded into one row.
    Fold {
        fold: Fold,
        delta: i128,
        block: Option<usize>,
    },
    /// The rest of a block's rows, which the table leaves out: how many
    /// items they stand for, and their delta.
    More {
        count: usize,
        delta: i128,
        block: Option<usize>,
    },
}

impl ChangeRow {
    /// How many bytes the row's items grew by, less than 0 when they
    /// shrank.
    fn delta(&self) -> i128 {
        match self {
            ChangeRow::Item { delta, .. }
            | ChangeRow::Fold { delta, .. }
            | ChangeRow::More { delta, .. } => *delta,
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
    pub(super) fn text(&self) -> String {
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

/// A row of the CHANGE table, with what ranks it among the others.
struct Ranked<'a> {
    /// How many bytes the row's items grew or shrank by: its delta, without
    /// the sign.
    bytes: u128,
    /// Whether the row is an edited item's, which comes after the other
    /// rows of its size.
    edited: bool,
    /// The name that orders rows of the same size: the item's, or the
    /// fold's words.
    name: Cow<'a, str>,
    /// The block that the row counts in (see [`Items::block`]).
    block: Option<usize>,
    row: ChangeRow,
}

impl Ranked<'_> {
    /// How the row ranks against `other`, best first: the largest first,
    /// and of the same size, edited items last, then by name.
    fn order(&self, other: &Ranked) -> Ordering {
        (other.bytes.cmp(&self.bytes))
            .then(self.edited.cmp(&other.edited))
            .then_with(|| self.name.cmp(&other.name))
    }
}

#[cfg(test)]
mod tests {
    use object::SectionKind;

    use super::*;
    use crate::binary::{Binary, Item, Section, Source};
    use crate::commands::diff::Build;
    use crate::commands::diff::pairing::{PairedBy, Pairing};
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

    /// Partners `old` and `new`, with `status`, paired by name.
    fn partners(status: PartnerStatus, old: usize, new: usize) -> Change {
        Change::Partners {
            old,
            new,
            status,
            paired_by: PairedBy::Name,
        }
    }

    /// The rows of CHANGE, each as its delta and what it says.
    fn table(items: &Items, heads: &[usize]) -> Vec<(i128, String)> {
        items
            .change_rows(heads, Detail::Brief)
            .iter()
            .map(|row| {
                let text = match row {
                    ChangeRow::Item { at, .. } => items.item(&items.changes[*at]).name.clone(),
                    ChangeRow::Fold { fold, .. } => fold.text(),
                    ChangeRow::More { count, .. } => format!("{count} more"),
                };
                (row.delta(), text)
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
            partners(PartnerStatus::Shrank, 0, 0),
            partners(PartnerStatus::Shrank, 1, 1),
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
            Change::Gone { old: 0 },
            Change::New { new: 0 },
            Change::New { new: 1 },
            Change::New { new: 2 },
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
    fn edited_rows_come_last_of_the_same_delta() {
        // An edited function, and a new one of no size, which its name
        // would put after it.
        let old = build(&[(&["b_edited"], 4)]);
        let new = build(&[(&["b_edited"], 4), (&["c_empty"], 0)]);
        let changes = vec![
            partners(PartnerStatus::Edited, 0, 0),
            Change::New { new: 1 },
        ];
        let items = changed(&old, &new, changes);
        assert_eq!(
            table(&items, &[0, 1]),
            [(0, "c_empty".to_string()), (0, "b_edited".to_string())]
        );
    }

    #[test]
    fn rows_go_by_block_and_those_in_no_block_come_last() {
        let (old, new) = (build(&[]), build(&[(&["a"], 9), (&["b"], 6), (&["c"], 3)]));
        let changes: Vec<Change> = (0..3).map(|new| Change::New { new }).collect();
        let mut items = changed(&old, &new, changes);
        items.new_blocks = vec![None, Some(1), Some(0)];
        assert_eq!(
            table(&items, &[0, 1, 2]),
            [
                (3, "c".to_string()),
                (6, "b".to_string()),
                (9, "a".to_string())
            ]
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
            .map(|new| Change::New { new })
            .collect();
        let items = changed(&old, &new, changes);
        let heads: Vec<usize> = (0..36).map(|at| at.min(31)).collect();
        let rows = table(&items, &heads);
        assert_eq!(rows.len(), 31);
        assert_eq!(rows[29], (71, "f29".to_string()));
        assert_eq!(rows[30], (134, "6 more".to_string()));
        // With one row left, the row takes the place of "1 more".
        let new = build(&functions[..31]);
        let changes: Vec<Change> = (0..31).map(|new| Change::New { new }).collect();
        let items = changed(&old, &new, changes);
        let rows = table(&items, &(0..31).collect::<Vec<_>>());
        assert_eq!(rows.len(), 31);
        assert_eq!(rows[30], (70, "f30".to_string()));
    }
}
