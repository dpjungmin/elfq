//! `elfq FILE retained SYMBOL`: what SYMBOL keeps in the file, as a tree.
//!
//! The tree is the part of the dominator tree under SYMBOL: each item hangs
//! under the closest item that every path to it passes through. Large trees
//! show only their largest items.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use super::exclusive::Exclusive;
use super::json;
use super::tree::KeptItem;
use crate::binary::Binary;
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
    /// SYMBOL's own bytes and those of everything it keeps: the bytes that
    /// would go away if it did.
    retained: u64,
    /// The items that only SYMBOL leads to, as a tree: each has what it
    /// keeps in turn, and the largest come first.
    keeps: Vec<KeptItem<'a>>,
    /// What SYMBOL's code uses that other code uses too, and so isn't
    /// counted, sorted by name.
    shares: Vec<json::Item<'a>>,
    /// How many calls and jumps in SYMBOL and the items it keeps go through
    /// pointers set at run time, which elfq can't follow. A function called
    /// that way counts for the code or data that holds its address.
    indirect_calls: usize,
}

pub fn run(
    path: &Path,
    binary: &Binary,
    symbol: &str,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    let exclusive = Exclusive::find(binary, symbol)?;
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
    let items = exclusive.items();
    let own = binary.items[index].size;
    if items.is_empty() {
        writeln!(
            out,
            "{name} keeps only its own {own} bytes: nothing else is in the file only because \
             of it."
        )?;
        write_shared(&exclusive, out)?;
        return exclusive.write_indirect_note(out);
    }
    let retained = &exclusive.retained;
    let nodes: Vec<Node> = items.iter().map(|&index| Node::Item(index)).collect();
    writeln!(
        out,
        "{name} keeps {} bytes in the file: its own {own}, and {} in {} that only it leads to.",
        retained.retained(index),
        retained.retained(index) - own,
        super::count_text(binary, &nodes)
    )?;
    writeln!(out)?;
    let shown = retained.largest(&items, SHOWN, HashSet::from([index]));
    let mut table = Table::new(&[
        ("RETAINED", Align::Right),
        ("OWN", Align::Right),
        ("ITEM", Align::Left),
    ]);
    retained.add_rows(&shown, node, "", "", &mut table);
    table.write(out)?;
    write_shared(&exclusive, out)?;
    // Not counting SYMBOL's own item.
    let shown_items = shown.len() - 1;
    if shown_items < items.len() {
        writeln!(out)?;
        writeln!(
            out,
            "The tree shows the largest {shown_items} of the {} items. To list them all:\n  \
             elfq {} exclusive {}",
            items.len(),
            super::shell_quote(&path.display().to_string()),
            super::shell_quote(symbol)
        )?;
    }
    let shown: Vec<usize> = shown.into_iter().collect();
    retained.write_pointer_note(&shown, out)?;
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
/// sorted by name.
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
    writeln!(out, "Also uses, but shares with other code: {names}.")?;
    Ok(())
}
