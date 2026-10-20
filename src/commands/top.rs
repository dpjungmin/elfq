//! `elfq FILE top`: the items that keep the most bytes in the file.
//!
//! The answer is the top of the dominator tree, cut down to its largest
//! items unless `--all` asks for every one: each item hangs under the
//! closest item that every path to it passes through, and its row names
//! that item, unless `--tree` draws the tree. The items at the top are
//! where the program starts, or are reached from more than one of those
//! places. With `--range`, only the bytes in the range count.

use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::retained_tree::{KeptItem, RetainedTree};
use super::{json, text};
use crate::binary::{AddressRange, Binary, FileKind};
use crate::cli::Format;
use crate::graph::Node;

#[derive(Serialize)]
struct TopJson<'a> {
    /// The range of memory that sizes count, or `null` for all of it.
    range: Option<json::Range>,
    /// The bytes of every item in the file, or in the range: the sum of
    /// `own` in `items` and `unreachable`.
    bytes: u64,
    /// The items that something reaches, as the rows of the tree in the
    /// text's order. Those at the top, with no `parent`, are where the
    /// program starts and the items that several of those places reach.
    items: Vec<KeptItem<'a>>,
    /// The items that nothing reaches, largest first.
    unreachable: Vec<Unreachable<'a>>,
}

/// An item that nothing reaches.
#[derive(Serialize)]
struct Unreachable<'a> {
    #[serde(flatten)]
    item: json::Item<'a>,
    /// Its bytes that sizes count: all of them, or with a range, those in
    /// it. 0 when elfq doesn't know its size.
    own: u64,
    /// How many of `own` are bytes of its load image, as in the rows of
    /// `items`.
    load_image: u64,
}

/// Answers `elfq FILE top`: the largest items, or every item with `all`.
pub fn run(
    binary: &Binary,
    range: Option<AddressRange>,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    if binary.kind == FileKind::SharedLibrary {
        bail!(
            "elfq doesn't know yet which functions a shared library offers to other files, so \
             it can't tell what keeps the most bytes in it"
        );
    }
    let retained = RetainedTree::build(binary, range)?;
    super::check_has_bytes(binary, range)?;
    if format == Format::Json {
        let answer = TopJson {
            range: range.map(json::Range::new),
            bytes: retained.bytes(),
            items: retained.to_json(retained.tree.top()),
            unreachable: unreachable(&retained)
                .into_iter()
                .map(|index| Unreachable {
                    item: json::Item::new(binary, Node::Item(index)),
                    own: retained.own(index),
                    load_image: retained.load_image(index),
                })
                .collect(),
        };
        return json::write(&answer, out);
    }
    // When nothing reaches the items in the range, which items nothing
    // reaches is the whole answer.
    let kept = retained.kept() > 0;
    if kept {
        let total = text::bytes_text(retained.bytes());
        match range {
            Some(range) => writeln!(
                out,
                "{total} of code and data in {}, by what keeps them:",
                text::range_text(binary, range, all)
            )?,
            None => writeln!(out, "{total} of code and data, by what keeps them:")?,
        }
        writeln!(out)?;
        retained.write_largest(text::shown(all), format, out)?;
    }
    if let Some(unreachable) = unreachable_text(&retained, all) {
        if kept {
            writeln!(out)?;
        }
        writeln!(out, "{unreachable}")?;
    }
    Ok(())
}

/// The items that nothing reaches, largest first, leaving out those with no
/// bytes in the range.
fn unreachable(retained: &RetainedTree) -> Vec<usize> {
    let binary = retained.binary;
    let mut unreachable: Vec<usize> = (0..binary.items.len())
        .filter(|&index| !retained.tree.is_reachable(Node::Item(index)))
        .filter(|&index| retained.range.is_none() || retained.own(index) > 0)
        .collect();
    unreachable.sort_by_key(|&index| (std::cmp::Reverse(retained.own(index)), index));
    unreachable
}

/// The sentence that names the items that nothing reaches, if any, largest
/// first: as many as [`text::names_text`] names, or with `all`, every one.
fn unreachable_text(retained: &RetainedTree, all: bool) -> Option<String> {
    let binary = retained.binary;
    let unreachable = unreachable(retained);
    if unreachable.is_empty() {
        return None;
    }
    let bytes = super::total(unreachable.iter().map(|&index| retained.own(index)));
    // Marked as rows are, when the bytes counted are a load image's.
    let names: Vec<String> = unreachable
        .iter()
        .map(|&index| {
            let name = &binary.items[index].name;
            match retained.mark(index) {
                Some(mark) => format!("{name} {mark}"),
                None => name.clone(),
            }
        })
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    if let [name] = names[..] {
        return Some(format!(
            "Nothing reaches {name}, {}: it is dead code, or used in a way elfq can't see.",
            text::bytes_text(bytes)
        ));
    }
    let nodes: Vec<Node> = unreachable.iter().map(|&index| Node::Item(index)).collect();
    Some(format!(
        "Nothing reaches {}, {} in all: {}. They are dead code, or used in a way elfq can't see.",
        text::count_text(binary, &nodes),
        text::bytes_text(bytes),
        text::names_text(&names, all)
    ))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::graph::Graph;

    #[test]
    fn an_item_that_nothing_reaches_is_marked_as_rows_are() {
        // As if nothing used `stats`, a variable in .data: in flash, the
        // range counts its load image.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-cortex-m7.elf");
        let binary = Binary::load(&path).unwrap();
        let stats = binary
            .items
            .iter()
            .position(|item| item.name == "stats")
            .unwrap();
        let mut graph = Graph::build(&binary);
        graph
            .references
            .retain(|reference| reference.to != Node::Item(stats));
        let flash = "0x30000000..0x30000edc".parse().unwrap();
        let retained = RetainedTree::from_graph(&binary, graph, Some(flash));
        assert_eq!(
            unreachable_text(&retained, false).unwrap(),
            "Nothing reaches stats (load image), 32 bytes: it is dead code, or used in a way \
             elfq can't see."
        );
    }
}
