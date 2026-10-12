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
    let retained = exclusive.tree.retained(node);
    if items.is_empty() {
        writeln!(
            out,
            "{name} keeps only its own {own} bytes: nothing else is in the file only because \
             of it."
        )?;
    } else {
        let nodes: Vec<Node> = items.iter().map(|&index| Node::Item(index)).collect();
        writeln!(
            out,
            "{name} keeps {retained} bytes in the file: its own {own}, and {} in {} that only it \
             leads to.",
            retained - own,
            super::count_text(binary, &nodes)
        )?;
        writeln!(out)?;
        let shown = largest(&exclusive, &items);
        let mut table = Table::new(&[
            ("RETAINED", Align::Right),
            ("OWN", Align::Right),
            ("ITEM", Align::Left),
        ]);
        add_rows(&exclusive, &shown, node, "", "", &mut table);
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
        exclusive.write_pointer_note(&shown, out)?;
        return Ok(());
    }
    write_shared(&exclusive, out)
}

/// The items to show: SYMBOL's item and the largest of those exclusive to
/// it, with every item between them and SYMBOL's so that the tree holds
/// together.
fn largest(exclusive: &Exclusive, items: &[usize]) -> HashSet<usize> {
    let tree = &exclusive.tree;
    let mut by_size = items.to_vec();
    by_size.sort_by_key(|&index| (std::cmp::Reverse(tree.retained(Node::Item(index))), index));
    let mut shown = HashSet::new();
    if let Node::Item(top) = exclusive.node {
        shown.insert(top);
    }
    for &index in by_size.iter().take(SHOWN) {
        let mut node = Node::Item(index);
        while node != exclusive.node {
            let Node::Item(item) = node else { break };
            if !shown.insert(item) {
                break;
            }
            match tree.parent(node) {
                Some(parent) => node = parent,
                None => break,
            }
        }
    }
    shown
}

/// Adds `node`'s row and, below it, its shown children. `prefix` draws the
/// tree's branches for this row, and `indent` for the rows below it.
fn add_rows(
    exclusive: &Exclusive,
    shown: &HashSet<usize>,
    node: Node,
    prefix: &str,
    indent: &str,
    table: &mut Table,
) {
    let tree = &exclusive.tree;
    let Node::Item(index) = node else { return };
    table.row(vec![
        tree.retained(node).to_string(),
        super::size_text(&exclusive.binary.items[index]),
        format!("{prefix}{}", exclusive.label(index)),
    ]);
    let children: Vec<usize> = tree
        .children(node)
        .iter()
        .filter_map(|&child| match child {
            Node::Item(child) => Some(child),
            Node::Import(_) => None,
        })
        .collect();
    let (visible, hidden): (Vec<usize>, Vec<usize>) = children
        .into_iter()
        .partition(|child| shown.contains(child));
    let lines = visible.len() + usize::from(!hidden.is_empty());
    for (i, &child) in visible.iter().enumerate() {
        let last = i + 1 == lines;
        add_rows(
            exclusive,
            shown,
            Node::Item(child),
            &format!("{indent}{}", if last { "└─ " } else { "├─ " }),
            &format!("{indent}{}", if last { "   " } else { "│  " }),
            table,
        );
    }
    if !hidden.is_empty() {
        let bytes: u64 = hidden
            .iter()
            .map(|&child| tree.retained(Node::Item(child)))
            .sum();
        table.row(vec![
            bytes.to_string(),
            String::new(),
            format!("{indent}└─ {} more", hidden.len()),
        ]);
    }
}

/// Names what SYMBOL's code uses that other code uses too, and so isn't
/// counted.
fn write_shared(exclusive: &Exclusive, out: &mut dyn Write) -> Result<()> {
    const NAMED: usize = 10;
    let binary = exclusive.binary;
    let mut kept: HashSet<Node> = exclusive
        .tree
        .exclusive(exclusive.node)
        .into_iter()
        .collect();
    kept.insert(exclusive.node);
    let mut shared: Vec<&str> = exclusive
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
