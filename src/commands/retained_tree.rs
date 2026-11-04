//! The dominator tree as the overview, `top` and `retained` show it: items
//! with their retained sizes, each row naming the item that keeps it,
//! functions marked when only pointers lead to them below the top, and
//! large trees cut down to their largest items. With `--range`, sizes count
//! only the bytes in the range, items that keep none of them are left out,
//! and items whose load images are in it are marked.

use std::collections::HashSet;
use std::ops::Deref;

use serde::Serialize;

use super::symbol::Names;
use super::tree_answer::{self, TreeTable};
use super::{json, text};
use crate::binary::{AddressRange, Binary, ItemKind, total};
use crate::cli::Format;
use crate::graph::{DominatorTree, Graph, Node};
use crate::render::{Align, TreeRow, TreeShape};

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
pub(super) struct RetainedTree<'g, 'a> {
    pub(super) binary: &'a Binary,
    pub(super) graph: &'g Graph,
    pub(super) tree: Held<'g, DominatorTree>,
    /// The range of memory that sizes count, if not all of it.
    pub(super) range: Option<AddressRange>,
    /// The items that something calls directly, rather than only through a
    /// pointer.
    called: HashSet<usize>,
    /// The names that find each item, which rows use to name the item that
    /// keeps them.
    names: Held<'g, Names<'a>>,
}

/// What a tree owns, or borrows from whoever worked it out before, as
/// browse's session does.
pub(super) enum Held<'g, T> {
    Owned(T),
    #[cfg_attr(not(feature = "tui"), expect(dead_code))]
    Borrowed(&'g T),
}

impl<T> Deref for Held<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        match self {
            Held::Owned(value) => value,
            Held::Borrowed(value) => value,
        }
    }
}

impl<'g, 'a> RetainedTree<'g, 'a> {
    /// Builds the dominator tree of `graph`, the file's reference graph.
    pub(super) fn new(
        binary: &'a Binary,
        graph: &'g Graph,
        range: Option<AddressRange>,
    ) -> RetainedTree<'g, 'a> {
        RetainedTree::naming(binary, graph, range, Names::new(binary))
    }

    /// The same, with `names` to name items by, such as those that found
    /// SYMBOL, so that the work of finding names is done only once.
    pub(super) fn naming(
        binary: &'a Binary,
        graph: &'g Graph,
        range: Option<AddressRange>,
        names: Names<'a>,
    ) -> RetainedTree<'g, 'a> {
        let tree = DominatorTree::build(binary, graph, range);
        RetainedTree {
            binary,
            graph,
            tree: Held::Owned(tree),
            range,
            called: called(graph),
            names: Held::Owned(names),
        }
    }

    /// The same tree, without a range, from `tree`, the dominator tree of
    /// `graph` that the caller built before, with `names` to name items by,
    /// so that neither is worked out again.
    #[cfg(feature = "tui")]
    pub(super) fn borrowing(
        binary: &'a Binary,
        graph: &'g Graph,
        tree: &'g DominatorTree,
        names: &'g Names<'a>,
    ) -> RetainedTree<'g, 'a> {
        RetainedTree {
            binary,
            graph,
            tree: Held::Borrowed(tree),
            range: None,
            called: called(graph),
            names: Held::Borrowed(names),
        }
    }

    /// The names that find each item, as SYMBOL takes them.
    pub(super) fn names(&self) -> &Names<'a> {
        &self.names
    }

    /// The bytes of the file that sizes count: all of them, or those in
    /// the range.
    pub(super) fn bytes(&self) -> u64 {
        total((0..self.binary.items.len()).map(|index| self.own(index)))
    }

    /// The bytes that the items where the program starts keep, and so
    /// everything reachable: all the bytes that sizes count, but those of
    /// items that nothing reaches.
    pub(super) fn kept(&self) -> u64 {
        total(self.tree.top().iter().map(|&node| self.tree.retained(node)))
    }

    /// The item's own bytes that sizes count: all of them, or those in the
    /// range, its load image's too (see [`Binary::bytes_in`]).
    pub(super) fn own(&self, index: usize) -> u64 {
        self.binary.bytes_in(&self.binary.items[index], self.range)
    }

    /// How many of the item's own bytes that sizes count are in its load
    /// image: 0 without a range.
    pub(super) fn load_image(&self, index: usize) -> u64 {
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

    /// Whether some of the item's own bytes are in the range, as every
    /// item's are when there is none. `retained` lets only such items
    /// compete for rows, and names only such items among what it shares.
    pub(super) fn has_bytes_in_range(&self, index: usize) -> bool {
        self.range.is_none() || self.own(index) > 0
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
    pub(super) fn mark(&self, index: usize) -> Option<String> {
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

    /// The table of the tree of the `count` items that keep the most bytes,
    /// as `top` and the overview show it, in the shape that `format` asks
    /// for. Every item that something reaches competes for a row, with a
    /// range those with no bytes of their own in it too, when they keep
    /// some. `retained` lets only items with bytes of their own compete.
    pub(super) fn largest_table(&self, count: usize, format: Format) -> TreeTable {
        let tree = &self.tree;
        let reachable: Vec<usize> = (0..self.binary.items.len())
            .filter(|&index| tree.is_reachable(Node::Item(index)))
            .collect();
        let shown = self.largest(tree.top(), &reachable, count);
        self.table(&shown, tree.top(), format)
    }

    /// The table of the trees under `tops`, with rows for the `shown`
    /// items, in the shape that `format` asks for.
    pub(super) fn table(&self, shown: &HashSet<usize>, tops: &[Node], format: Format) -> TreeTable {
        self.names.will_name(shown.len());
        let shape = shape(format);
        TreeTable {
            columns: COLUMNS.into(),
            rows: self.rows(shown, tops, shape),
            shape,
        }
    }

    /// The items to show of a large tree drawn from `tops`: the `count`
    /// largest of `items` that get rows, every item between each of them
    /// and the tops, and a lone item that would be left out on its own
    /// (see [`tree_answer::add_lone`]).
    pub(super) fn largest(&self, tops: &[Node], items: &[usize], count: usize) -> HashSet<usize> {
        let is_top: HashSet<Node> = tops.iter().copied().collect();
        let candidates = items.iter().copied().filter(|&index| self.counts(index));
        let largest = tree_answer::largest(candidates, |index| self.retained(index), count);
        let mut shown = tree_answer::with_chains(largest, |index| {
            let node = Node::Item(index);
            match self.tree.parent(node) {
                Some(Node::Item(parent)) if !is_top.contains(&node) => Some(parent),
                _ => None,
            }
        });
        tree_answer::add_lone(&mut shown, self.counted(tops), |index| {
            self.counted_children(index)
        });
        shown
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
    fn tree_row(&self, shown: &HashSet<usize>, index: usize, shape: TreeShape) -> TreeRow {
        let cells = vec![self.retained(index).to_string(), self.own_text(index)];
        let children = self.rows(shown, self.tree.children(Node::Item(index)), shape);
        let name = &self.binary.items[index].name;
        let short_name = match tree_answer::is_named_below(&children, shape) {
            true => self.names.short_name(Node::Item(index)),
            false => name.clone(),
        };
        TreeRow {
            cells,
            name: name.clone(),
            mark: self.mark(index),
            short_name,
            children,
            node: Some(Node::Item(index)),
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

    /// The items that the item at `index` keeps itself that get rows.
    fn counted_children(&self, index: usize) -> impl Iterator<Item = usize> {
        self.counted(self.tree.children(Node::Item(index)))
    }

    /// The row that sums up `hidden` items and what they keep, if there are
    /// any.
    fn more(&self, hidden: &[usize]) -> Option<TreeRow> {
        if hidden.is_empty() {
            return None;
        }
        let bytes = total(hidden.iter().map(|&index| self.retained(index)));
        let items = tree_answer::with_descendants(hidden, |index| self.counted_children(index));
        let cells = vec![bytes.to_string(), String::new()];
        Some(TreeRow::new(
            cells,
            format!("{} more", items.len()),
            Vec::new(),
        ))
    }

    /// The rows of the items in `nodes` and of everything they keep, in the
    /// order of the text's rows: each item, largest first, followed by what
    /// it keeps. Imports are left out, since their code isn't in the file,
    /// and so are items that keep none of the range's bytes.
    pub(super) fn to_json(&self, nodes: &[Node]) -> Vec<KeptItem<'a>> {
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

    fn retained(&self, index: usize) -> u64 {
        self.tree.retained(Node::Item(index))
    }
}

/// The items that something calls directly, rather than only through a
/// pointer, in `graph`.
fn called(graph: &Graph) -> HashSet<usize> {
    graph
        .references
        .iter()
        .filter(|r| r.kind.is_call())
        .filter_map(|r| match r.to {
            Node::Item(index) => Some(index),
            Node::Import(_) => None,
        })
        .collect()
}

/// The columns of the trees of the overview, `top` and `retained`.
const COLUMNS: [(&str, Align); 3] = [
    ("RETAINED", Align::Right),
    ("OWN", Align::Right),
    ("ITEM", Align::Left),
];

/// How the overview, `top` and `retained` show their trees: drawn with
/// `--tree`, or else as rows that each name the item that keeps them, as
/// in "crc_table, kept by crc32".
fn shape(format: Format) -> TreeShape {
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
            RetainedTree::new(&binary, &graph, Some(range)).mark(filter_tap)
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
