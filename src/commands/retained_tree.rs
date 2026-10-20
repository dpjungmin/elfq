//! The dominator tree as the overview, `top` and `retained` show it: items
//! with their retained sizes, each row naming the item that keeps it,
//! functions marked when only pointers lead to them below the top, and
//! large trees cut down to their largest items. With `--range`, sizes count
//! only the bytes in the range, items that keep none of them are left out,
//! and items whose load images are in it are marked.

use std::collections::HashSet;
use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::symbol::Names;
use super::{json, text};
use crate::binary::{AddressRange, Binary, ItemKind};
use crate::cli::Format;
use crate::graph::{DominatorTree, Graph, Node};
use crate::render::{Align, Table, TreeRow, TreeShape};

/// One row of the tree, for `--json`: an item, and the row of the item that
/// keeps it.
#[derive(Serialize)]
pub(super) struct KeptItem<'a> {
    /// Which row this is, counting from 0.
    id: usize,
    #[serde(flatten)]
    item: json::Item<'a>,
    /// Its own bytes that sizes count, as in the OWN column: all of them, or
    /// with a range, those in it. 0 when elfq doesn't know its size, where
    /// OWN shows `?`.
    own: u64,
    /// How many of `own` are bytes of its load image, which a range counts
    /// where the file stores it, and where the text marks the row "(load
    /// image)", or "(with its load image)" when the range holds both
    /// copies. 0 without a range.
    load_image: u64,
    /// Its own bytes and those of everything it keeps.
    retained: u64,
    /// Whether nothing calls this function directly, so that it hangs under,
    /// and counts for, the code or data that holds its address. `false` at
    /// the top of the whole tree, where the program starts, such as for a
    /// function listed in `.init_array`.
    through_pointer: bool,
    /// The id of the row of the item that keeps it, or `null` for an item
    /// at the top of the list: in `top`, at the top of the whole tree, and
    /// in `retained`, one that SYMBOL keeps itself.
    parent: Option<usize>,
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
    /// The names that find each item, which rows use to name the item that
    /// keeps them.
    names: Names<'a>,
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
            .filter(|r| r.kind.is_call())
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
            names: Names::new(binary),
        }
    }

    /// The tree, with `names` to name items by: those that found SYMBOL, so
    /// that the work of finding names is done only once.
    pub fn with_names(self, names: Names<'a>) -> RetainedTree<'a> {
        RetainedTree { names, ..self }
    }

    /// The names that find each item, as SYMBOL takes them.
    pub fn names(&self) -> &Names<'a> {
        &self.names
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

    /// The item's own bytes that sizes count: all of them, or those in the
    /// range, its load image's too (see [`Binary::bytes_in`]).
    pub fn own(&self, index: usize) -> u64 {
        self.binary.bytes_in(&self.binary.items[index], self.range)
    }

    /// How many of the item's own bytes that sizes count are in its load
    /// image: 0 without a range.
    pub fn load_image(&self, index: usize) -> u64 {
        self.range.map_or(0, |range| {
            self.binary
                .load_image_bytes_in(&self.binary.items[index], range)
        })
    }

    /// The OWN column for an item: its own bytes, or with a range, those in
    /// it, its load image's too.
    fn own_text(&self, index: usize) -> String {
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

    /// The words after the name of an item: "(load image)" when its bytes
    /// that sizes count are those of its load image, or "(with its load
    /// image)" when they are those of both copies, as in a range that holds
    /// both; "(through a pointer)" for a function below the top that only
    /// pointers lead to, and so counts for whatever holds its address; or
    /// both, as in "(load image, through a pointer)".
    pub fn mark(&self, index: usize) -> Option<String> {
        let load_image = match self.load_image(index) {
            0 => None,
            bytes if bytes == self.own(index) => Some("load image"),
            _ => Some("with its load image"),
        };
        let pointer = self.through_pointer(index).then_some("through a pointer");
        let words: Vec<&str> = [load_image, pointer].into_iter().flatten().collect();
        (!words.is_empty()).then(|| format!("({})", words.join(", ")))
    }

    /// Whether `index` is a function below the top of the tree that nothing
    /// calls directly, and so counts for whatever holds its address.
    fn through_pointer(&self, index: usize) -> bool {
        self.binary.items[index].kind == ItemKind::Function
            && !self.called.contains(&index)
            && self.tree.parent(Node::Item(index)).is_some()
    }

    /// Writes the tree of the `count` items that keep the most bytes, as
    /// `top` and the overview show it.
    pub fn write_largest(&self, count: usize, format: Format, out: &mut dyn Write) -> Result<()> {
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
        let shape = shape(format);
        table.tree(&self.rows(&shown, tree.top(), shape), shape);
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
    /// and what they keep, for a table that shows them in `shape`.
    fn rows(&self, shown: &HashSet<usize>, nodes: &[Node], shape: TreeShape) -> Vec<TreeRow> {
        let (visible, hidden): (Vec<usize>, Vec<usize>) =
            self.counted(nodes).partition(|index| shown.contains(index));
        let mut rows: Vec<TreeRow> = visible
            .iter()
            .map(|&index| self.tree_row(shown, index, shape))
            .collect();
        rows.extend(self.more(&hidden));
        rows
    }

    /// The row of the item at `index`, with the rows of its shown children
    /// under it, for a table that shows them in `shape`. As rows, those
    /// call it by its short name.
    pub fn tree_row(&self, shown: &HashSet<usize>, index: usize, shape: TreeShape) -> TreeRow {
        let cells = vec![self.retained(index).to_string(), self.own_text(index)];
        let children = self.rows(shown, self.tree.children(Node::Item(index)), shape);
        let name = &self.binary.items[index].name;
        // Finding a short name takes a while, so only rows that show it
        // find it.
        let short_name = if children.is_empty() || shape == TreeShape::Drawn {
            name.clone()
        } else {
            self.names.short_name(Node::Item(index))
        };
        TreeRow {
            cells,
            name: name.clone(),
            mark: self.mark(index),
            short_name,
            children,
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

    /// The rows of the items in `nodes` and of everything they keep, in the
    /// order of the text's rows: each item, largest first, followed by what
    /// it keeps. Imports are left out, since their code isn't in the file,
    /// and so are items that keep none of the range's bytes.
    pub fn to_json(&self, nodes: &[Node]) -> Vec<KeptItem<'a>> {
        let mut rows = Vec::new();
        self.add_json(nodes, None, &mut rows);
        rows
    }

    /// Adds the rows of the items in `nodes`, which the row `parent` keeps,
    /// each followed by the rows of what it keeps in turn.
    fn add_json(&self, nodes: &[Node], parent: Option<usize>, rows: &mut Vec<KeptItem<'a>>) {
        for index in self.counted(nodes) {
            let id = rows.len();
            rows.push(KeptItem {
                id,
                item: json::Item::new(self.binary, Node::Item(index)),
                own: self.own(index),
                load_image: self.load_image(index),
                retained: self.retained(index),
                through_pointer: self.through_pointer(index),
                parent,
            });
            self.add_json(self.tree.children(Node::Item(index)), Some(id), rows);
        }
    }

    pub fn retained(&self, index: usize) -> u64 {
        self.tree.retained(Node::Item(index))
    }
}

/// How the overview, `top` and `retained` show their trees: drawn with
/// `--tree`, or else as rows that each name the item that keeps them, as
/// in "crc_table, kept by crc32".
pub(super) fn shape(format: Format) -> TreeShape {
    match format {
        Format::Tree => TreeShape::Drawn,
        _ => TreeShape::Rows { words: "kept by" },
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::graph::ReferenceKind;

    #[test]
    fn a_load_image_can_be_reached_through_a_pointer() {
        // As if apply_filter only took the address of filter_tap, which
        // both run from ITCM in the Cortex-M7 sample, stored in flash.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-cortex-m7.elf");
        let binary = Binary::load(&path).unwrap();
        let filter_tap = binary
            .items
            .iter()
            .position(|item| item.name == "filter_tap")
            .unwrap();
        let mark = |range: &str| {
            let mut graph = Graph::build(&binary);
            for reference in &mut graph.references {
                if reference.to == Node::Item(filter_tap) {
                    reference.kind = ReferenceKind::Address;
                }
            }
            let range = range.parse().unwrap();
            RetainedTree::from_graph(&binary, graph, Some(range)).mark(filter_tap)
        };
        assert_eq!(
            mark("0x30000000..0x30000edc").as_deref(),
            Some("(load image, through a pointer)")
        );
        assert_eq!(
            mark("0x0..0x40000000").as_deref(),
            Some("(with its load image, through a pointer)")
        );
    }
}
