//! The rows and the table of `callers` and `callees` for SYMBOL: each row
//! is how one node uses the other, and where.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use super::flat_answer::FlatRow;
use super::text;
use crate::binary::Binary;
use crate::graph::{Node, Reference};
use crate::render::Align;

/// One line of a callers or callees table: the references between one pair
/// of nodes that are worded the same way.
pub(super) struct Row {
    /// The node on the other end: the caller, or the callee.
    pub(super) node: Node,
    /// How the node is used, in the words that [`rows`] was given: the HOW
    /// column's, or JSON's.
    pub(super) how: &'static str,
    pub(super) sites: Vec<u64>,
}

/// Groups references into rows, in the order each row first appears.
/// `other_end` picks the node that the table lists for each reference, and
/// `how` says how it is used: references with the same words share a row.
pub(super) fn rows<'a>(
    references: impl IntoIterator<Item = &'a Reference>,
    other_end: impl Fn(&Reference) -> Node,
    how: impl Fn(&Reference) -> &'static str,
) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    // Where each row is in `rows`, since a function can have thousands.
    let mut index: HashMap<(Node, &'static str), usize> = HashMap::new();
    for reference in references {
        let node = other_end(reference);
        let how = how(reference);
        let row = *index.entry((node, how)).or_insert_with(|| {
            rows.push(Row {
                node,
                how,
                sites: Vec::new(),
            });
            rows.len() - 1
        });
        rows[row].sites.push(reference.site);
    }
    rows
}

/// The distinct nodes in `rows`, in order.
pub(super) fn distinct_nodes(rows: &[Row]) -> Vec<Node> {
    let mut seen = HashSet::new();
    rows.iter()
        .map(|row| row.node)
        .filter(|&node| seen.insert(node))
        .collect()
}

/// Which end of the uses a table lists.
#[derive(Clone, Copy)]
pub(super) enum End {
    /// What uses SYMBOL, as `callers` lists it.
    Callers,
    /// What SYMBOL uses, as `callees` lists it.
    Callees,
}

/// The table of `callers` or `callees`: its columns, HOW, AT and the
/// caller or callee, and the rows of the `count` largest nodes (see
/// [`largest_rows`]), then a last row such as "70 more" that counts the
/// rest.
pub(super) fn table(
    binary: &Binary,
    rows: Vec<Row>,
    end: End,
    count: usize,
) -> (Vec<(String, Align)>, Vec<FlatRow>) {
    let header = match end {
        End::Callers => "CALLER",
        End::Callees => "CALLEE",
    };
    let columns = ["HOW", "AT", header].map(|header| (header.to_string(), Align::Left));
    let (rows, left_out) = largest_rows(binary, rows, count);
    let mut table: Vec<FlatRow> = (rows.into_iter())
        .map(|row| {
            let cells = vec![row.how.to_string(), text::at_column(&row.sites)];
            let name = row.node.name(binary).to_string();
            let mut flat = FlatRow::new(cells, name, Some(row.node));
            // A caller uses SYMBOL in its own code, which browse opens at
            // the first place; a callee is used in SYMBOL's code.
            flat.at = match end {
                End::Callers => Some(row.sites[0]),
                End::Callees => None,
            };
            flat
        })
        .collect();
    if left_out > 0 {
        let cells = vec![String::new(), String::new()];
        table.push(FlatRow::new(cells, format!("{left_out} more"), None));
    }
    (columns.into(), table)
}

/// Whether the table of `callers` or `callees` for `nodes` nodes needs
/// `--all` to list every one.
#[cfg(feature = "tui")]
pub(super) fn table_needs_all(nodes: usize) -> bool {
    left_out(nodes, text::shown(text::Detail::Brief)) > 0
}

/// How many of `nodes` nodes a table that lists `count` leaves out (see
/// [`text::shown_count`]).
fn left_out(nodes: usize, count: usize) -> usize {
    nodes - text::shown_count(nodes, count)
}

/// The rows of the `count` nodes in `rows` with the largest own sizes
/// ([`Node::size`]), in the order they come in, and how many nodes are left
/// out. A node that would be left out on its own is kept too, since its rows
/// take no more room than "1 more" would. Of nodes the same size, the name
/// that sorts first is kept.
fn largest_rows(binary: &Binary, rows: Vec<Row>, count: usize) -> (Vec<Row>, usize) {
    let mut nodes = distinct_nodes(&rows);
    let left_out = left_out(nodes.len(), count);
    if left_out == 0 {
        return (rows, 0);
    }
    let shown = nodes.len() - left_out;
    nodes.sort_by_key(|&node| (Reverse(node.size(binary)), node.name(binary), node));
    let kept: HashSet<Node> = nodes.into_iter().take(shown).collect();
    let rows = rows
        .into_iter()
        .filter(|row| kept.contains(&row.node))
        .collect();
    (rows, left_out)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn a_long_table_keeps_the_rows_of_its_largest_items() {
        let path = Path::new("testdata/bin/c-x86_64.elf");
        let mut binary = Binary::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap();
        for (index, (name, size)) in [("d", 20), ("b", 10), ("a", 10), ("c", 5)]
            .into_iter()
            .enumerate()
        {
            binary.items[index].name = name.to_string();
            binary.items[index].size = size;
        }
        let row = |node, how| Row {
            node,
            how,
            sites: Vec::new(),
        };
        binary.imports.push("write".to_string());
        let import = Node::Import(binary.imports.len() - 1);
        let (d, b, a, c) = (Node::Item(0), Node::Item(1), Node::Item(2), Node::Item(3));
        let rows = || {
            vec![
                row(c, "call"),
                row(import, "call"),
                row(a, "call"),
                row(b, "call"),
                row(d, "call"),
                row(c, "pointer"),
            ]
        };
        let kept = |count| {
            let (rows, left_out) = largest_rows(&binary, rows(), count);
            let rows: Vec<(&str, &str)> = rows
                .iter()
                .map(|row| (row.node.name(&binary), row.how))
                .collect();
            (rows, left_out)
        };

        // d and the first name of the two of size 10, in their order. The
        // import counts as size 0, and c's two rows count as one item.
        assert_eq!(kept(2), (vec![("a", "call"), ("d", "call")], 3));
        assert_eq!(
            kept(3),
            (vec![("a", "call"), ("b", "call"), ("d", "call")], 2)
        );
        // With room for all but one, the last is kept too.
        assert_eq!(kept(4).0.len(), 6);
        assert_eq!(kept(4).1, 0);
    }

    #[cfg(feature = "tui")]
    #[test]
    fn a_table_needs_all_exactly_when_it_would_cut() {
        // A table lists 30 nodes and counts the rest in a last row, which
        // never counts one: it lists 31 nodes in full.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-x86_64.elf");
        let binary = Binary::load(&path).unwrap();
        for count in 29..=33 {
            // The cells of the table of `count` nodes: brief, or with
            // every row.
            let cells = |detail| {
                let rows = (0..count)
                    .map(|index| Row {
                        node: Node::Item(index),
                        how: "call",
                        sites: vec![0],
                    })
                    .collect();
                let (_, rows) = table(&binary, rows, End::Callers, text::shown(detail));
                rows.iter().map(FlatRow::all_cells).collect::<Vec<_>>()
            };
            assert_eq!(
                table_needs_all(count),
                cells(text::Detail::All) != cells(text::Detail::Brief),
                "{count} nodes"
            );
        }
    }
}
