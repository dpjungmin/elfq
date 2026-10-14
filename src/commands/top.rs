//! `elfq FILE top`: the items that keep the most bytes in the file, as a
//! tree.
//!
//! The tree is the top of the dominator tree, cut down to its largest
//! items unless `--all` asks for every one: each item hangs under the
//! closest item that every path to it passes through, and the items at the
//! top are where the program starts, or are reached from more than one of
//! those places. With `--range`, only the bytes in the range count.

use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::json;
use super::tree::{KeptItem, RetainedTree};
use crate::binary::{AddressRange, Binary, FileKind, Item};
use crate::cli::Format;
use crate::graph::Node;
use crate::render::{Align, Table};

#[derive(Serialize)]
struct TopJson<'a> {
    /// The range of memory that sizes count, or `null` for all of it.
    range: Option<json::Range>,
    /// The bytes of every item in the file, or in the range.
    bytes: u64,
    /// The top of the tree: where the program starts, and the items that
    /// several of those places reach. Each has the whole tree of what it
    /// keeps, and the largest come first.
    items: Vec<KeptItem<'a>>,
    /// The items that nothing reaches, largest first.
    unreachable: Vec<json::Item<'a>>,
}

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
    check_has_bytes(&retained)?;
    if format == Format::Json {
        let answer = TopJson {
            range: range.map(json::Range::new),
            bytes: retained.bytes(),
            items: retained.to_json(retained.tree.top()),
            unreachable: unreachable(&retained)
                .into_iter()
                .map(|index| json::Item::new(binary, Node::Item(index)))
                .collect(),
        };
        return json::write(&answer, out);
    }
    // When nothing reaches the items in the range, which items nothing
    // reaches is the whole answer.
    let kept = retained.kept() > 0;
    if kept {
        let total = super::bytes_text(retained.bytes());
        match range {
            Some(range) => writeln!(
                out,
                "{total} of code and data in {}, by what keeps them:",
                super::range_text(binary, range)
            )?,
            None => writeln!(out, "{total} of code and data, by what keeps them:")?,
        }
        writeln!(out)?;
        write_largest(&retained, super::shown(all), out)?;
    }
    if let Some(unreachable) = unreachable_text(&retained) {
        if kept {
            writeln!(out)?;
        }
        writeln!(out, "{unreachable}")?;
    }
    Ok(())
}

/// Explains why there are no bytes to count, if there aren't: a range that
/// no section is in is most likely mistyped, in stripped firmware elfq
/// can't find the functions yet, and hand-written assembly often leaves out
/// the sizes.
pub(super) fn check_has_bytes(retained: &RetainedTree) -> Result<()> {
    let binary = retained.binary;
    if retained.bytes() > 0 {
        return Ok(());
    }
    let place = match retained.range {
        Some(range) if super::sections_in(binary, range).is_empty() => bail!(
            "nothing in the file is in {}; the overview lists the ranges that the file's \
             sections occupy",
            super::range_argument(range)
        ),
        Some(range) => super::range_text(binary, range),
        None => "the file".to_string(),
    };
    if !super::lists_functions(binary) {
        bail!("the file has no symbols or unwind table, so elfq can't find its functions yet");
    }
    let starts_in_place = |item: &&Item| {
        retained
            .range
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

/// Writes the tree of the `count` items that keep the most bytes.
pub(super) fn write_largest(
    retained: &RetainedTree,
    count: usize,
    out: &mut dyn Write,
) -> Result<()> {
    let binary = retained.binary;
    let tree = &retained.tree;
    let reachable: Vec<usize> = (0..binary.items.len())
        .filter(|&index| tree.is_reachable(Node::Item(index)))
        .collect();
    let shown = retained.largest(tree.top(), &reachable, count);
    let mut table = Table::new(&[
        ("RETAINED", Align::Right),
        ("OWN", Align::Right),
        ("ITEM", Align::Left),
    ]);
    table.tree(&retained.rows(&shown, tree.top()));
    table.write(out)?;
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

/// The sentence that names the items that nothing reaches, if any.
fn unreachable_text(retained: &RetainedTree) -> Option<String> {
    let binary = retained.binary;
    let unreachable = unreachable(retained);
    if unreachable.is_empty() {
        return None;
    }
    let bytes: u64 = unreachable.iter().map(|&index| retained.own(index)).sum();
    let names: Vec<&str> = unreachable
        .iter()
        .map(|&index| binary.items[index].name.as_str())
        .collect();
    if let [name] = names[..] {
        return Some(format!(
            "Nothing reaches {name}, {}: it is dead code, or used in a way elfq can't see.",
            super::bytes_text(bytes)
        ));
    }
    let nodes: Vec<Node> = unreachable.iter().map(|&index| Node::Item(index)).collect();
    Some(format!(
        "Nothing reaches {}, {} in all: {}. They are dead code, or used in a way elfq can't see.",
        super::count_text(binary, &nodes),
        super::bytes_text(bytes),
        super::names_text(&names)
    ))
}
