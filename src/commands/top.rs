//! `elfq FILE top`: the items that keep the most bytes in the file, as a
//! tree.
//!
//! The tree is the top of the dominator tree, cut down to its largest
//! items: each item hangs under the closest item that every path to it
//! passes through, and the items at the top are where the program starts,
//! or are reached from more than one of those places.

use std::collections::HashSet;
use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::json;
use super::tree::{KeptItem, RetainedTree};
use crate::binary::{Binary, FileKind};
use crate::cli::Format;
use crate::graph::Node;
use crate::render::{Align, Table};

/// How many items the tree shows at most, besides the lines that sum up the
/// rest.
pub(super) const SHOWN: usize = 30;

#[derive(Serialize)]
struct TopJson<'a> {
    /// The bytes of every item in the file.
    bytes: u64,
    /// The top of the tree: where the program starts, and the items that
    /// several of those places reach. Each has the whole tree of what it
    /// keeps, and the largest come first.
    items: Vec<KeptItem<'a>>,
    /// The items that nothing reaches, largest first.
    unreachable: Vec<json::Item<'a>>,
}

pub fn run(binary: &Binary, format: Format, out: &mut dyn Write) -> Result<()> {
    if binary.kind == FileKind::SharedLibrary {
        bail!(
            "elfq doesn't know yet which functions a shared library offers to other files, so \
             it can't tell what keeps the most bytes in it"
        );
    }
    let retained = RetainedTree::build(binary)?;
    if format == Format::Json {
        let answer = TopJson {
            bytes: binary.items.iter().map(|item| item.size).sum(),
            items: retained.to_json(retained.tree.top()),
            unreachable: unreachable(&retained)
                .into_iter()
                .map(|index| json::Item::new(binary, Node::Item(index)))
                .collect(),
        };
        return json::write(&answer, out);
    }
    let shown = write_largest(&retained, SHOWN, out)?;
    write_unreachable(&retained, out)?;
    retained.write_pointer_note(&shown, out)
}

/// Writes the tree of the `count` items that keep the most bytes, and
/// returns the items shown.
pub(super) fn write_largest(
    retained: &RetainedTree,
    count: usize,
    out: &mut dyn Write,
) -> Result<Vec<usize>> {
    let binary = retained.binary;
    let tree = &retained.tree;
    let total: u64 = binary.items.iter().map(|item| item.size).sum();
    writeln!(
        out,
        "The items that keep the most of the file's {total} bytes:"
    )?;
    writeln!(out)?;
    let reachable: Vec<usize> = (0..binary.items.len())
        .filter(|&index| tree.is_reachable(Node::Item(index)))
        .collect();
    let shown = retained.largest(&reachable, count, HashSet::new());
    let mut table = Table::new(&[
        ("RETAINED", Align::Right),
        ("OWN", Align::Right),
        ("ITEM", Align::Left),
    ]);
    let (visible, hidden) = retained.split(tree.top(), &shown);
    for &index in &visible {
        retained.add_rows(&shown, Node::Item(index), "", "", &mut table);
    }
    retained.add_more(&hidden, "", &mut table);
    table.write(out)?;
    Ok(shown.into_iter().collect())
}

/// The items that nothing reaches, largest first.
fn unreachable(retained: &RetainedTree) -> Vec<usize> {
    let binary = retained.binary;
    let mut unreachable: Vec<usize> = (0..binary.items.len())
        .filter(|&index| !retained.tree.is_reachable(Node::Item(index)))
        .collect();
    unreachable.sort_by_key(|&index| (std::cmp::Reverse(binary.items[index].size), index));
    unreachable
}

/// Names the items that nothing reaches, if any.
fn write_unreachable(retained: &RetainedTree, out: &mut dyn Write) -> Result<()> {
    let binary = retained.binary;
    let unreachable = unreachable(retained);
    if unreachable.is_empty() {
        return Ok(());
    }
    let bytes: u64 = unreachable
        .iter()
        .map(|&index| binary.items[index].size)
        .sum();
    let names: Vec<&str> = unreachable
        .iter()
        .map(|&index| binary.items[index].name.as_str())
        .collect();
    writeln!(out)?;
    if let [name] = names[..] {
        writeln!(
            out,
            "Nothing reaches {name}, {bytes} bytes, as far as elfq can tell. It may be dead code, \
             or used in a way elfq can't see."
        )?;
    } else {
        let nodes: Vec<Node> = unreachable.iter().map(|&index| Node::Item(index)).collect();
        writeln!(
            out,
            "Nothing reaches {}, {bytes} bytes in all, as far as elfq can tell: {}. They may be \
             dead code, or used in a way elfq can't see.",
            super::count_text(binary, &nodes),
            super::names_text(&names)
        )?;
    }
    Ok(())
}
