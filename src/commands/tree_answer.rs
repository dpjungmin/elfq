//! The answer of a command whose table holds a tree, as `top`, `retained`,
//! `why` and `inlined FUNCTION` write it, and as a tab of browse shows it.
//! Unlike a [`FlatAnswer`], whose rows are one to a line and whose notes
//! follow one empty line together, its rows hang under one another, and
//! each of its notes follows an empty line.
//!
//! A large tree shows only its largest rows, and the functions at the end
//! of this file cut it down: the largest items, and the items between them
//! and the top so that the tree holds together ([`largest`] and
//! [`with_chains`]), a lone item that would be left out on its own
//! ([`add_lone`]), and a row such as "70 more" that sums up the rest
//! ([`with_descendants`]). `reach --tree` uses all but [`largest`]: it
//! picks its largest items with its own `Text::largest`, which breaks ties
//! by name where [`largest`] breaks them by number. They work on items
//! numbered as each command numbers them, and take the tree's sizes,
//! parents and children as closures.
//!
//! [`FlatAnswer`]: super::flat_answer::FlatAnswer

use std::cmp::Reverse;
use std::collections::HashSet;
use std::io::Write;

use anyhow::Result;

use crate::render::{Align, Table, TreeRow, TreeShape};

/// The text of an answer whose table holds a tree: its first lines, the
/// table, and the notes under it, each after an empty line.
pub(super) struct TreeAnswer {
    /// The first lines, or what the answer says in place of a table.
    pub(super) lead: Vec<String>,
    pub(super) table: Option<TreeTable>,
    pub(super) notes: Vec<String>,
}

/// A table whose last column holds a tree.
pub(super) struct TreeTable {
    /// The columns, the last of which holds the tree.
    pub(super) columns: Vec<(&'static str, Align)>,
    /// The rows at the top of the tree.
    pub(super) rows: Vec<TreeRow>,
    /// How the table shows the tree, which the rows were made for, since
    /// rows find the short names they show only when rows under them name
    /// them (see [`is_named_below`]).
    pub(super) shape: TreeShape,
}

impl TreeAnswer {
    /// An answer of sentences, without a table.
    pub(super) fn sentences(lead: Vec<String>) -> TreeAnswer {
        TreeAnswer {
            lead,
            table: None,
            notes: Vec::new(),
        }
    }

    /// What [`TreeAnswer::write`] writes.
    #[cfg(feature = "tui")]
    pub(super) fn written(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out).expect("writing to memory");
        out
    }

    pub(super) fn write(&self, out: &mut dyn Write) -> Result<()> {
        for line in &self.lead {
            writeln!(out, "{line}")?;
        }
        if let Some(table) = &self.table {
            writeln!(out)?;
            table.write(out)?;
        }
        for note in &self.notes {
            writeln!(out)?;
            writeln!(out, "{note}")?;
        }
        Ok(())
    }
}

impl TreeTable {
    pub(super) fn write(&self, out: &mut dyn Write) -> Result<()> {
        let mut table = Table::new(&self.columns);
        table.tree(&self.rows, self.shape);
        table.write(out)?;
        Ok(())
    }
}

/// Whether the rows under a row, `children`, name it, in a table that shows
/// them in `shape`, so that the row needs its short name. Finding a short
/// name takes a while, so only rows that show it find it.
pub(super) fn is_named_below(children: &[TreeRow], shape: TreeShape) -> bool {
    !children.is_empty() && shape != TreeShape::Drawn
}

/// An answer of `why` or `retained` as a tab of browse shows it.
#[cfg(feature = "tui")]
pub(super) struct TabAnswer {
    /// The answer with every row and name, as with `--all`.
    pub(super) answer: TreeAnswer,
    /// Its notes as the command words them without `--all`, which name
    /// fewer items.
    pub(super) brief: Vec<String>,
    /// Its notes naming only the first item of each list, shortened, for a
    /// screen with little room for them.
    pub(super) briefest: Vec<String>,
    /// Whether the command needs `--all` to print all of `answer`.
    pub(super) needs_all: bool,
}

#[cfg(feature = "tui")]
impl TabAnswer {
    /// An answer of sentences, the same with `--all` and without.
    pub(super) fn sentences(lead: Vec<String>) -> TabAnswer {
        TabAnswer {
            answer: TreeAnswer::sentences(lead),
            brief: Vec::new(),
            briefest: Vec::new(),
            needs_all: false,
        }
    }
}

/// The `count` largest of `items`, as `size` gives their sizes, largest
/// first, and of the same size, the lowest number first.
pub(super) fn largest(
    items: impl IntoIterator<Item = usize>,
    size: impl Fn(usize) -> u64,
    count: usize,
) -> Vec<usize> {
    let mut by_size: Vec<usize> = items.into_iter().collect();
    by_size.sort_by_key(|&item| (Reverse(size(item)), item));
    by_size.truncate(count);
    by_size
}

/// `items`, and every item between each of them and the top of the tree, so
/// that a tree of them holds together. `parent` gives the item that each
/// hangs under, or `None` at the top.
pub(super) fn with_chains(
    items: impl IntoIterator<Item = usize>,
    parent: impl Fn(usize) -> Option<usize>,
) -> HashSet<usize> {
    let mut shown = HashSet::new();
    for item in items {
        let mut on_chain = Some(item);
        while let Some(item) = on_chain
            && shown.insert(item)
        {
            on_chain = parent(item);
        }
    }
    shown
}

/// Adds to `shown` each item that would be the only one left out of a list
/// of items side by side, when nothing hangs under it, since its row takes
/// no more room than "1 more" would. The lists are `top`, the items at the
/// top of the tree, and the items under each shown item, as `children`
/// gives them: those that would get rows.
pub(super) fn add_lone<I: IntoIterator<Item = usize>>(
    shown: &mut HashSet<usize>,
    top: impl IntoIterator<Item = usize>,
    children: impl Fn(usize) -> I,
) {
    let lone_in = |items: &mut dyn Iterator<Item = usize>| {
        let mut hidden = items.filter(|item| !shown.contains(item));
        match (hidden.next(), hidden.next()) {
            (Some(lone), None) if children(lone).into_iter().next().is_none() => Some(lone),
            _ => None,
        }
    };
    let mut lone: Vec<usize> = shown
        .iter()
        .filter_map(|&item| lone_in(&mut children(item).into_iter()))
        .collect();
    lone.extend(lone_in(&mut top.into_iter()));
    shown.extend(lone);
}

/// `items` and every item under them, as `children` gives them, each before
/// the items under it: what a row such as "70 more" sums up.
pub(super) fn with_descendants<I: IntoIterator<Item = usize>>(
    items: &[usize],
    children: impl Fn(usize) -> I,
) -> Vec<usize> {
    let mut all = Vec::new();
    // The items still to list, the next one last. A loop, so that listing
    // every row under a large "70 more" takes no stack; the rows shown are
    // built recursively, on the large stack that lib.rs sets up.
    let mut next: Vec<usize> = items.iter().rev().copied().collect();
    while let Some(item) = next.pop() {
        all.push(item);
        let start = next.len();
        next.extend(children(item));
        next[start..].reverse();
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree of items 0 to 5: 0 and 4 at the top, 1 and 2 under 0, 3
    /// under 1, and 5 under 2 when `deep`.
    fn children(item: usize, deep: bool) -> Vec<usize> {
        match item {
            0 => vec![1, 2],
            1 => vec![3],
            2 if deep => vec![5],
            _ => Vec::new(),
        }
    }

    fn parent(item: usize) -> Option<usize> {
        match item {
            1 | 2 => Some(0),
            3 => Some(1),
            5 => Some(2),
            _ => None,
        }
    }

    fn sorted(items: &HashSet<usize>) -> Vec<usize> {
        let mut items: Vec<usize> = items.iter().copied().collect();
        items.sort();
        items
    }

    #[test]
    fn the_largest_come_first_and_of_the_same_size_the_lowest_number() {
        let sizes = [5, 9, 9, 1];
        assert_eq!(largest(0..4, |item| sizes[item], 3), [1, 2, 0]);
    }

    #[test]
    fn a_shown_item_brings_the_items_above_it() {
        assert_eq!(sorted(&with_chains([3], parent)), [0, 1, 3]);
        assert_eq!(sorted(&with_chains([3, 2], parent)), [0, 1, 2, 3]);
    }

    #[test]
    fn an_item_left_out_alone_with_nothing_under_it_is_shown() {
        // 2 is the only one of 0's children left out, and 4 the only one
        // at the top.
        let mut shown = with_chains([3], parent);
        add_lone(&mut shown, [0, 4], |item| children(item, false));
        assert_eq!(sorted(&shown), [0, 1, 2, 3, 4]);
        // Not when something hangs under it.
        let mut shown = with_chains([3], parent);
        add_lone(&mut shown, [0, 4], |item| children(item, true));
        assert_eq!(sorted(&shown), [0, 1, 3, 4]);
        // Nor when two are left out side by side, as 1 and 2 are.
        let mut shown = with_chains([0], parent);
        add_lone(&mut shown, [0, 4], |item| children(item, false));
        assert_eq!(sorted(&shown), [0, 4]);
    }

    #[test]
    fn descendants_come_after_the_item_they_hang_under() {
        let all = with_descendants(&[0, 4], |item| children(item, true));
        assert_eq!(all, [0, 1, 3, 2, 5, 4]);
    }
}
