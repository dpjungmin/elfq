//! The dominator tree as `retained` and `top` show it: items with their
//! retained sizes, functions marked when only pointers lead to them, and
//! large trees cut down to their largest items. With `--range`, sizes count
//! only the bytes in the range, and items that keep none of them are left
//! out.

use std::collections::HashSet;
use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::{json, text};
use crate::binary::{AddressRange, Binary, Item, ItemKind};
use crate::graph::{DominatorTree, Graph, Node, ReferenceKind};
use crate::render::{Align, Table, TreeRow};

/// An item and everything it keeps, for `--json`.
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
        super::total((0..self.binary.items.len()).map(|index| self.own(index)))
    }

    /// The bytes that the items where the program starts keep, and so
    /// everything reachable: all the bytes that sizes count, but those of
    /// items that nothing reaches.
    pub fn kept(&self) -> u64 {
        super::total(self.tree.top().iter().map(|&node| self.tree.retained(node)))
    }

    /// Explains why there are no bytes to count, if there aren't: a range
    /// that no section is in is most likely mistyped, in some stripped files
    /// elfq can't find the functions yet, and hand-written assembly often
    /// leaves out the sizes.
    pub fn check_has_bytes(&self) -> Result<()> {
        let binary = self.binary;
        if self.bytes() > 0 {
            return Ok(());
        }
        let place = super::empty_place(binary, self.range)?;
        let starts_in_place = |item: &&Item| {
            self.range
                .is_none_or(|range| range.overlap(item.address, 1) > 0)
        };
        if let Some(item) = binary.items.iter().find(starts_in_place) {
            bail!(
                "elfq doesn't know where {} ends, so it can't count the bytes in {place}",
                item.name
            );
        }
        bail!("elfq found no functions or data in {place}")
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
            (None, _) => text::size_text(item),
            (Some(_), own) if item.size_estimated && own > 0 => format!("~{own}"),
            (Some(_), own) => own.to_string(),
        }
    }

    /// Whether to show an item: with a range, only items that keep some of
    /// its bytes are worth a row.
    fn counts(&self, index: usize) -> bool {
        self.range.is_none() || self.retained(index) > 0
    }

    /// The mark after the name of a function that only pointers lead to,
    /// and so counts for whatever holds its address.
    fn mark(&self, index: usize) -> Option<&'static str> {
        self.through_pointer(index).then_some("(through a pointer)")
    }

    /// Whether `index` is a function that nothing calls directly, and so
    /// counts for whatever holds its address.
    pub fn through_pointer(&self, index: usize) -> bool {
        self.binary.items[index].kind == ItemKind::Function
            && !self.called.contains(&index)
            && self.tree.parent(Node::Item(index)).is_some()
    }

    /// Writes the tree of the `count` items that keep the most bytes, as
    /// `top` and the overview show it.
    pub fn write_largest(&self, count: usize, out: &mut dyn Write) -> Result<()> {
        let tree = &self.tree;
        let reachable: Vec<usize> = (0..self.binary.items.len())
            .filter(|&index| tree.is_reachable(Node::Item(index)))
            .collect();
        let shown = self.largest(tree.top(), &reachable, count);
        let mut table = Table::new(&[
            ("RETAINED", Align::Right),
            ("OWN", Align::Right),
            ("ITEM", Align::Left),
        ]);
        table.tree(&self.rows(&shown, tree.top()));
        table.write(out)?;
        Ok(())
    }

    /// The items to show of a large tree drawn from `roots`: the `count`
    /// largest of `items`, and every item between each of them and the
    /// roots, so that the tree holds together. An item that would be left
    /// out on its own, keeping nothing that gets a row, is shown too, since
    /// its row takes no more room than "1 more" would.
    pub fn largest(&self, roots: &[Node], items: &[usize], count: usize) -> HashSet<usize> {
        let is_root: HashSet<Node> = roots.iter().copied().collect();
        let mut by_size: Vec<usize> = items
            .iter()
            .copied()
            .filter(|&index| self.counts(index))
            .collect();
        by_size.sort_by_key(|&index| (std::cmp::Reverse(self.retained(index)), index));
        let mut shown = HashSet::new();
        for &index in by_size.iter().take(count) {
            let mut node = Node::Item(index);
            while let Node::Item(item) = node {
                if !shown.insert(item) || is_root.contains(&node) {
                    break;
                }
                match self.tree.parent(node) {
                    Some(parent) => node = parent,
                    None => break,
                }
            }
        }
        let lone: Vec<usize> = shown
            .iter()
            .map(|&index| self.tree.children(Node::Item(index)))
            .chain([roots])
            .filter_map(|nodes| self.lone(nodes, &shown))
            .collect();
        shown.extend(lone);
        shown
    }

    /// The one item in `nodes` that gets a row but isn't in `shown`, if
    /// there is only one and it keeps nothing that gets a row.
    fn lone(&self, nodes: &[Node], shown: &HashSet<usize>) -> Option<usize> {
        let mut hidden = self.counted(nodes).filter(|index| !shown.contains(index));
        match (hidden.next(), hidden.next()) {
            (Some(lone), None) if self.kept_items(lone) == 0 => Some(lone),
            _ => None,
        }
    }

    /// The rows of the shown items in `nodes`, each with the rows of its
    /// shown children under it, and one row that sums up those not shown
    /// and what they keep.
    pub fn rows(&self, shown: &HashSet<usize>, nodes: &[Node]) -> Vec<TreeRow> {
        let (visible, hidden): (Vec<usize>, Vec<usize>) =
            self.counted(nodes).partition(|index| shown.contains(index));
        let mut rows: Vec<TreeRow> = visible
            .iter()
            .map(|&index| self.tree_row(shown, index))
            .collect();
        rows.extend(self.more(&hidden));
        rows
    }

    /// The row of the item at `index`, with the rows of its shown children
    /// under it.
    pub fn tree_row(&self, shown: &HashSet<usize>, index: usize) -> TreeRow {
        let cells = vec![self.retained(index).to_string(), self.own_text(index)];
        let children = self.rows(shown, self.tree.children(Node::Item(index)));
        TreeRow {
            mark: self.mark(index),
            ..TreeRow::new(cells, &self.binary.items[index].name, children)
        }
    }

    /// The items in `nodes` that get rows: not imports, which have no bytes
    /// in the file, nor items that keep none of the range's bytes.
    fn counted(&self, nodes: &[Node]) -> impl Iterator<Item = usize> {
        nodes.iter().filter_map(|&node| match node {
            Node::Item(index) if self.counts(index) => Some(index),
            _ => None,
        })
    }

    /// How many items the item at `index` keeps that would get rows.
    fn kept_items(&self, index: usize) -> usize {
        self.counted(self.tree.children(Node::Item(index)))
            .map(|child| 1 + self.kept_items(child))
            .sum()
    }

    /// The row that sums up `hidden` items and what they keep, if there are
    /// any.
    fn more(&self, hidden: &[usize]) -> Option<TreeRow> {
        if hidden.is_empty() {
            return None;
        }
        let bytes = super::total(hidden.iter().map(|&index| self.retained(index)));
        let items: usize = hidden.iter().map(|&index| 1 + self.kept_items(index)).sum();
        let cells = vec![bytes.to_string(), String::new()];
        Some(TreeRow::new(cells, format!("{items} more"), Vec::new()))
    }

    /// The items in `nodes`, each with the whole tree of what it keeps.
    /// Imports are left out, since their code isn't in the file, and so are
    /// items that keep none of the range's bytes.
    pub fn to_json(&self, nodes: &[Node]) -> Vec<KeptItem<'a>> {
        self.counted(nodes)
            .map(|index| KeptItem {
                item: json::Item::new(self.binary, Node::Item(index)),
                retained: self.retained(index),
                through_pointer: self.through_pointer(index),
                keeps: self.to_json(self.tree.children(Node::Item(index))),
            })
            .collect()
    }

    pub fn retained(&self, index: usize) -> u64 {
        self.tree.retained(Node::Item(index))
    }
}
