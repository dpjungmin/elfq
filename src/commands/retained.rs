//! `elfq FILE retained SYMBOL`: what SYMBOL keeps in the file, as a tree.
//!
//! The tree is the part of the dominator tree under SYMBOL: each item hangs
//! under the closest item that every path to it passes through. Large trees
//! show only their largest items.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use anyhow::Result;

use super::exclusive::Exclusive;
use crate::binary::Binary;
use crate::graph::Node;
use crate::render::{Align, Table};

/// How many items a tree shows at most, besides the lines that sum up the
/// rest.
const SHOWN: usize = 30;

pub fn run(path: &Path, binary: &Binary, symbol: &str, out: &mut dyn Write) -> Result<()> {
    let Some(exclusive) = Exclusive::find(binary, symbol, out)? else {
        return Ok(());
    };
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
        return write_shared(&exclusive, out);
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
    retained.write_pointer_note(&shown, out)
}

/// Names what SYMBOL's code uses that other code uses too, and so isn't
/// counted.
fn write_shared(exclusive: &Exclusive, out: &mut dyn Write) -> Result<()> {
    const NAMED: usize = 10;
    let binary = exclusive.retained.binary;
    let mut kept: HashSet<Node> = exclusive
        .retained
        .tree
        .exclusive(exclusive.node)
        .into_iter()
        .collect();
    kept.insert(exclusive.node);
    let mut shared: Vec<&str> = exclusive
        .retained
        .graph
        .references
        .iter()
        .filter(|r| kept.contains(&Node::Item(r.from)) && !kept.contains(&r.to))
        .map(|r| r.to.name(binary))
        .collect();
    shared.sort_unstable();
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
