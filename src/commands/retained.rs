//! `elfq FILE retained SYMBOL`: what SYMBOL keeps in the file, as a tree.
//!
//! The tree is the part of the dominator tree under SYMBOL: each item hangs
//! under the closest item that every path to it passes through. Large trees
//! show only their largest items. With `--range`, only the bytes in the
//! range count.

use std::collections::HashSet;
use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::exclusive::Exclusive;
use super::json;
use super::tree::KeptItem;
use crate::binary::{AddressRange, Binary};
use crate::cli::Format;
use crate::graph::Node;
use crate::render::{Align, Table};

/// How many items a tree shows at most, besides the lines that sum up the
/// rest.
const SHOWN: usize = 30;

#[derive(Serialize)]
struct RetainedJson<'a> {
    symbol: json::Item<'a>,
    /// Whether anything reaches SYMBOL from where the program starts. When
    /// nothing does, nothing is there only because of it.
    reachable: bool,
    /// The range of memory that sizes count, or `null` for all of it.
    range: Option<json::Range>,
    /// SYMBOL's own bytes and those of everything it keeps: the bytes that
    /// would go away if it did. With a range, only those in it.
    retained: u64,
    /// The items that only SYMBOL leads to, as a tree: each has what it
    /// keeps in turn, and the largest come first.
    keeps: Vec<KeptItem<'a>>,
    /// What SYMBOL's code uses that other code uses too, and so isn't
    /// counted, sorted by name. With a range, only items with bytes in it.
    shares: Vec<json::Item<'a>>,
    /// How many calls and jumps in SYMBOL and the items it keeps go through
    /// pointers set at run time, which elfq can't follow. A function called
    /// that way counts for the code or data that holds its address.
    indirect_calls: usize,
}

pub fn run(
    binary: &Binary,
    symbol: &str,
    range: Option<AddressRange>,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    let exclusive = Exclusive::find(binary, symbol, range)?;
    if format == Format::Json {
        return write_json(&exclusive, out);
    }
    if !exclusive.is_reachable() {
        return exclusive.write_unreachable(out);
    }
    let node = exclusive.node;
    let Node::Item(index) = node else {
        unreachable!("Exclusive::find rejects imports");
    };
    let name = node.name(binary);
    let retained = &exclusive.retained;
    let items = exclusive.counted_items();
    let own = retained.own(index);
    let range = range.map(|range| super::range_text(binary, range));
    if items.is_empty() {
        match &range {
            Some(range) if own == 0 => writeln!(
                out,
                "Removing {name} would save none of the bytes in {range}."
            )?,
            Some(range) => writeln!(
                out,
                "Removing {name} would save its own {own} bytes in {range}, and nothing else."
            )?,
            None => writeln!(
                out,
                "Removing {name} would save its own {own} bytes, and nothing else."
            )?,
        }
        write_shared(&exclusive, out)?;
        return exclusive.write_indirect_note(out);
    }
    let in_range = range
        .map(|range| format!(" in {range}"))
        .unwrap_or_default();
    writeln!(
        out,
        "Removing {name} would save {} bytes{in_range}:",
        retained.retained(index)
    )?;
    writeln!(out)?;
    let shown = retained.largest(&items, SHOWN, HashSet::from([index]));
    let mut table = Table::new(&[
        ("RETAINED", Align::Right),
        ("OWN", Align::Right),
        ("ITEM", Align::Left),
    ]);
    table.tree(&[retained.tree_row(&shown, index)]);
    table.write(out)?;
    write_shared(&exclusive, out)?;
    exclusive.write_indirect_note(out)
}

fn write_json(exclusive: &Exclusive, out: &mut dyn Write) -> Result<()> {
    let retained = &exclusive.retained;
    let binary = retained.binary;
    let node = exclusive.node;
    let reachable = exclusive.is_reachable();
    let shares = if reachable {
        shared(exclusive)
    } else {
        Vec::new()
    };
    let answer = RetainedJson {
        symbol: json::Item::new(binary, node),
        reachable,
        range: retained.range.map(json::Range::new),
        retained: retained.tree.retained(node),
        keeps: retained.to_json(retained.tree.children(node)),
        shares: shares
            .into_iter()
            .map(|node| json::Item::new(binary, node))
            .collect(),
        indirect_calls: exclusive.indirect_calls().len(),
    };
    json::write(&answer, out)
}

/// What SYMBOL's code uses that other code uses too, and so isn't counted,
/// sorted by name. With a range, only items with bytes in it.
fn shared(exclusive: &Exclusive) -> Vec<Node> {
    let binary = exclusive.retained.binary;
    let mut kept: HashSet<Node> = exclusive
        .retained
        .tree
        .exclusive(exclusive.node)
        .into_iter()
        .collect();
    kept.insert(exclusive.node);
    let mut shared: Vec<Node> = exclusive
        .retained
        .graph
        .references
        .iter()
        .filter(|r| kept.contains(&Node::Item(r.from)) && !kept.contains(&r.to))
        .map(|r| r.to)
        .filter(|&node| match (exclusive.retained.range, node) {
            (None, _) => true,
            (Some(_), Node::Item(index)) => exclusive.retained.own(index) > 0,
            (Some(_), Node::Import(_)) => false,
        })
        .collect();
    shared.sort_by_key(|&node| (node.name(binary), node));
    shared.dedup();
    shared
}

/// Names what SYMBOL's code uses that other code uses too, and so isn't
/// counted.
fn write_shared(exclusive: &Exclusive, out: &mut dyn Write) -> Result<()> {
    const NAMED: usize = 10;
    let binary = exclusive.retained.binary;
    let mut shared: Vec<&str> = shared(exclusive)
        .into_iter()
        .map(|node| node.name(binary))
        .collect();
    shared.dedup();
    if shared.is_empty() {
        return Ok(());
    }
    let mut names = shared[..shared.len().min(NAMED)].join(", ");
    if shared.len() > NAMED {
        names += &format!(" and {} more", shared.len() - NAMED);
    }
    writeln!(out)?;
    writeln!(out, "Shared with other code, so not counted: {names}.")?;
    Ok(())
}
