//! The dominator tree as `retained`, `exclusive` and `top` show it: items
//! with their retained sizes, functions marked when only pointers lead to
//! them, and large trees cut down to their largest items. With `--range`,
//! sizes count only the bytes in the range, and items that keep none of
//! them are left out.

use std::collections::HashSet;
use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::json;
use crate::binary::{AddressRange, Binary, ItemKind};
use crate::graph::{DominatorTree, Graph, Node, ReferenceKind};
use crate::render::Table;

/// An item and everything it keeps, for `--format json`.
#[derive(Serialize)]
pub(super) struct KeptItem<'a> {
    #[serde(flatten)]
    item: json::Item<'a>,
    /// Its own bytes and those of everything it keeps.
    retained: u64,
    /// Whether nothing calls this function directly, so that it counts for
    /// the code or data that holds its address.
    through_pointer: bool,
    /// The items that only it leads to, each with what it keeps in turn,
    /// largest first.
    keeps: Vec<KeptItem<'a>>,
}

/// A binary's graph and dominator tree.
pub(super) struct RetainedTree<'a> {
    pub binary: &'a Binary,
    pub graph: Graph,
    pub tree: DominatorTree,
    /// The range of memory that sizes count, if not all of it.
    pub range: Option<AddressRange>,
    /// The items that something calls directly, rather than only through a
    /// pointer.
    called: HashSet<usize>,
}

impl<'a> RetainedTree<'a> {
    /// Builds the graph and its dominator tree, or explains why elfq can't
    /// yet.
    pub fn build(binary: &'a Binary, range: Option<AddressRange>) -> Result<RetainedTree<'a>> {
        Ok(RetainedTree::from_graph(
            binary,
            super::reference_graph(binary)?,
            range,
        ))
    }

    /// Builds the dominator tree of a graph already built.
    pub fn from_graph(
        binary: &'a Binary,
        graph: Graph,
        range: Option<AddressRange>,
    ) -> RetainedTree<'a> {
        let tree = DominatorTree::build(binary, &graph, range);
        let called = graph
            .references
            .iter()
            .filter(|r| matches!(r.kind, ReferenceKind::Call | ReferenceKind::TailCall))
            .filter_map(|r| match r.to {
                Node::Item(index) => Some(index),
                Node::Import(_) => None,
            })
            .collect();
        RetainedTree {
            binary,
            graph,
            tree,
            range,
            called,
        }
    }

    /// The bytes of the file that sizes count: all of them, or those in
    /// the range.
    pub fn bytes(&self) -> u64 {
        (0..self.binary.items.len())
            .map(|index| self.own(index))
            .sum()
    }

    /// The item's own bytes that sizes count: all of them, or those in the
    /// range.
    pub fn own(&self, index: usize) -> u64 {
        let item = &self.binary.items[index];
        match self.range {
            Some(range) => range.overlap(item.address, item.size),
            None => item.size,
        }
    }

    /// The OWN column for an item. Outside the range it is 0.
    pub fn own_text(&self, index: usize) -> String {
        let item = &self.binary.items[index];
        match (self.range, self.own(index)) {
            (None, _) => super::size_text(item),
            (Some(_), own) if item.size_estimated && own > 0 => format!("~{own}"),
            (Some(_), own) => own.to_string(),
        }
    }

    /// Whether to show an item: with a range, only items that keep some of
    /// its bytes are worth a row.
    fn counts(&self, index: usize) -> bool {
        self.range.is_none() || self.retained(index) > 0
    }

    /// An item's name, marked when it is a function that only pointers lead
    /// to, and so counts for whatever holds its address.
    pub fn label(&self, index: usize) -> String {
        let name = &self.binary.items[index].name;
        if self.through_pointer(index) {
            format!("{name} (through a pointer)")
        } else {
            name.clone()
        }
    }

    /// Explains the marks that [`RetainedTree::label`] added to `shown`, if
    /// any.
    pub fn write_pointer_note(&self, shown: &[usize], out: &mut dyn Write) -> Result<()> {
        if shown.iter().any(|&index| self.through_pointer(index)) {
            writeln!(out)?;
            writeln!(
                out,
                "Nothing calls the functions marked \"through a pointer\" directly, so elfq \
                 counts them for the code or data that holds their address."
            )?;
        }
        Ok(())
    }

    /// Whether `index` is a function that nothing calls directly, and so
    /// counts for whatever holds its address.
    pub fn through_pointer(&self, index: usize) -> bool {
        self.binary.items[index].kind == ItemKind::Function
            && !self.called.contains(&index)
            && self.tree.parent(Node::Item(index)).is_some()
    }

    /// The items to show of a large tree: the `count` largest of `items`,
    /// and every item between each of them and the top of what is shown,
    /// so that the tree holds together. `shown` starts with the items that
    /// are always shown, such as the top.
    pub fn largest(
        &self,
        items: &[usize],
        count: usize,
        mut shown: HashSet<usize>,
    ) -> HashSet<usize> {
        let mut by_size: Vec<usize> = items
            .iter()
            .copied()
            .filter(|&index| self.counts(index))
            .collect();
        by_size.sort_by_key(|&index| (std::cmp::Reverse(self.retained(index)), index));
        for &index in by_size.iter().take(count) {
            let mut node = Node::Item(index);
            while let Node::Item(item) = node {
                if !shown.insert(item) {
                    break;
                }
                match self.tree.parent(node) {
                    Some(parent) => node = parent,
                    None => break,
                }
            }
        }
        shown
    }

    /// Adds `node`'s row and, below it, its shown children, with one line
    /// that sums up the children not shown. `prefix` draws the tree's
    /// branches for this row, and `indent` for the rows below it.
    pub fn add_rows(
        &self,
        shown: &HashSet<usize>,
        node: Node,
        prefix: &str,
        indent: &str,
        table: &mut Table,
    ) {
        let Node::Item(index) = node else { return };
        table.row(vec![
            self.retained(index).to_string(),
            self.own_text(index),
            format!("{prefix}{}", self.label(index)),
        ]);
        let (visible, hidden) = self.split(self.tree.children(node), shown);
        let lines = visible.len() + usize::from(!hidden.is_empty());
        for (i, &child) in visible.iter().enumerate() {
            let last = i + 1 == lines;
            self.add_rows(
                shown,
                Node::Item(child),
                &format!("{indent}{}", if last { "└─ " } else { "├─ " }),
                &format!("{indent}{}", if last { "   " } else { "│  " }),
                table,
            );
        }
        self.add_more(&hidden, &format!("{indent}└─ "), table);
    }

    /// Splits `nodes` into the items shown and those not, leaving out
    /// imports, which have no bytes in the file, and items that keep none
    /// of the range's bytes.
    pub fn split(&self, nodes: &[Node], shown: &HashSet<usize>) -> (Vec<usize>, Vec<usize>) {
        nodes
            .iter()
            .filter_map(|&node| match node {
                Node::Item(index) if self.counts(index) => Some(index),
                _ => None,
            })
            .partition(|index| shown.contains(index))
    }

    /// Adds the line that sums up `hidden` items, if there are any.
    pub fn add_more(&self, hidden: &[usize], prefix: &str, table: &mut Table) {
        if hidden.is_empty() {
            return;
        }
        let bytes: u64 = hidden.iter().map(|&index| self.retained(index)).sum();
        table.row(vec![
            bytes.to_string(),
            String::new(),
            format!("{prefix}{} more", hidden.len()),
        ]);
    }

    /// The items in `nodes`, each with the whole tree of what it keeps.
    /// Imports are left out, since their code isn't in the file, and so are
    /// items that keep none of the range's bytes.
    pub fn to_json(&self, nodes: &[Node]) -> Vec<KeptItem<'a>> {
        nodes
            .iter()
            .filter_map(|&node| match node {
                Node::Item(index) if self.counts(index) => Some(KeptItem {
                    item: json::Item::new(self.binary, node),
                    retained: self.retained(index),
                    through_pointer: self.through_pointer(index),
                    keeps: self.to_json(self.tree.children(node)),
                }),
                _ => None,
            })
            .collect()
    }

    pub fn retained(&self, index: usize) -> u64 {
        self.tree.retained(Node::Item(index))
    }
}
