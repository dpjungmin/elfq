//! `elfq FILE retained SYMBOL`: what SYMBOL keeps in the file, as a tree.
//!
//! The tree is the part of the dominator tree under SYMBOL: each item hangs
//! under the closest item that every path to it passes through. Large trees
//! show only their largest items, unless `--all` asks for every one. With
//! `--range`, only the bytes in the range count.

use std::collections::HashSet;
use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::json;
use super::retained_tree::{KeptItem, RetainedTree};
use crate::binary::{AddressRange, Binary, FileKind, ItemKind};
use crate::cli::Format;
use crate::graph::{Node, Unresolved};
use crate::render::{Align, Table};

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
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    let symbol = SymbolInTree::find(binary, symbol, range)?;
    if format == Format::Json {
        return write_json(&symbol, out);
    }
    if !symbol.is_reachable() {
        return symbol.write_unreachable(out);
    }
    let node = symbol.node;
    let Node::Item(index) = node else {
        unreachable!("SymbolInTree::find rejects imports");
    };
    let name = node.name(binary);
    let retained = &symbol.retained;
    let items = symbol.counted_items();
    let own = retained.own(index);
    let own_bytes = super::bytes_text(own);
    let range = range.map(|range| super::range_text(binary, range));
    if items.is_empty() {
        match &range {
            Some(range) if own == 0 => writeln!(
                out,
                "Removing {name} would save none of the bytes in {range}."
            )?,
            Some(range) => writeln!(
                out,
                "Removing {name} would save its own {own_bytes} in {range}, and nothing else."
            )?,
            None => writeln!(
                out,
                "Removing {name} would save its own {own_bytes}, and nothing else."
            )?,
        }
        write_shared(&symbol, out)?;
        return symbol.write_indirect_note(out);
    }
    let in_range = range
        .map(|range| format!(" in {range}"))
        .unwrap_or_default();
    writeln!(
        out,
        "Removing {name} would save {}{in_range}:",
        super::bytes_text(retained.retained(index))
    )?;
    writeln!(out)?;
    let shown = retained.largest(&[node], &items, super::shown(all));
    let mut table = Table::new(&[
        ("RETAINED", Align::Right),
        ("OWN", Align::Right),
        ("ITEM", Align::Left),
    ]);
    table.tree(&[retained.tree_row(&shown, index)]);
    table.write(out)?;
    write_shared(&symbol, out)?;
    symbol.write_indirect_note(out)
}

fn write_json(symbol: &SymbolInTree, out: &mut dyn Write) -> Result<()> {
    let retained = &symbol.retained;
    let binary = retained.binary;
    let node = symbol.node;
    let reachable = symbol.is_reachable();
    let shares = if reachable {
        shared(symbol)
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
        indirect_calls: symbol.indirect_calls().len(),
    };
    json::write(&answer, out)
}

/// What SYMBOL's code uses that other code uses too, and so isn't counted,
/// sorted by name. With a range, only items with bytes in it.
fn shared(symbol: &SymbolInTree) -> Vec<Node> {
    let binary = symbol.retained.binary;
    let mut kept: HashSet<Node> = symbol
        .retained
        .tree
        .exclusive(symbol.node)
        .into_iter()
        .collect();
    kept.insert(symbol.node);
    let mut shared: Vec<Node> = symbol
        .retained
        .graph
        .references
        .iter()
        .filter(|r| kept.contains(&Node::Item(r.from)) && !kept.contains(&r.to))
        .map(|r| r.to)
        .filter(|&node| match (symbol.retained.range, node) {
            (None, _) => true,
            (Some(_), Node::Item(index)) => symbol.retained.own(index) > 0,
            (Some(_), Node::Import(_)) => false,
        })
        .collect();
    shared.sort_by_key(|&node| (node.name(binary), node));
    shared.dedup();
    shared
}

/// Names what SYMBOL's code uses that other code uses too, and so isn't
/// counted.
fn write_shared(symbol: &SymbolInTree, out: &mut dyn Write) -> Result<()> {
    const NAMED: usize = 10;
    let binary = symbol.retained.binary;
    let mut shared: Vec<&str> = shared(symbol)
        .into_iter()
        .map(|node| node.name(binary))
        .collect();
    shared.dedup();
    if shared.is_empty() {
        return Ok(());
    }
    let named = super::shown_count(shared.len(), NAMED);
    let mut names = shared[..named].join(", ");
    if named < shared.len() {
        names += &format!(" and {} more", shared.len() - named);
    }
    writeln!(out)?;
    writeln!(out, "Shared with other code, so not counted: {names}.")?;
    Ok(())
}

/// SYMBOL's item, in the tree of retained sizes that says what only it
/// leads to.
struct SymbolInTree<'a> {
    retained: RetainedTree<'a>,
    /// SYMBOL's item.
    node: Node,
}

impl<'a> SymbolInTree<'a> {
    /// Finds SYMBOL's item and what is exclusive to it, counting only the
    /// bytes in `range`, if there is one.
    fn find(
        binary: &'a Binary,
        symbol: &str,
        range: Option<AddressRange>,
    ) -> Result<SymbolInTree<'a>> {
        super::check_code_is_readable(binary)?;
        let node = super::symbol::find(binary, symbol)?;
        let name = node.name(binary);
        let Node::Item(index) = node else {
            bail!("{name} is imported from a shared library, so its code isn't in this file");
        };
        let item = &binary.items[index];
        if item.kind == ItemKind::Function && item.size == 0 {
            bail!("elfq doesn't know where {name} ends, so it can't read its code");
        }
        if binary.kind == FileKind::SharedLibrary {
            bail!(
                "elfq doesn't know yet which functions a shared library offers to other files, \
                 so it can't tell what only {name} leads to"
            );
        }
        let retained = RetainedTree::build(binary, range)?;
        Ok(SymbolInTree { retained, node })
    }

    /// Whether anything reaches SYMBOL from where the program starts.
    fn is_reachable(&self) -> bool {
        self.retained.tree.is_reachable(self.node)
    }

    /// Says that nothing reaches SYMBOL, so it is dead code, or used in a way
    /// elfq can't see.
    fn write_unreachable(&self, out: &mut dyn Write) -> Result<()> {
        writeln!(
            out,
            "{}",
            super::nothing_reaches(self.node.name(self.retained.binary))
        )?;
        Ok(())
    }

    /// The calls and jumps through pointers set at run time in SYMBOL and
    /// the items exclusive to it.
    fn indirect_calls(&self) -> Vec<Unresolved> {
        let Node::Item(index) = self.node else {
            return Vec::new();
        };
        let graph = &self.retained.graph;
        std::iter::once(index)
            .chain(self.items())
            .flat_map(|item| graph.unresolved_in(item))
            .filter(|unresolved| unresolved.is_indirect())
            .copied()
            .collect()
    }

    /// Says that what SYMBOL's code calls through pointers may count for
    /// other items, if it calls anything that way.
    fn write_indirect_note(&self, out: &mut dyn Write) -> Result<()> {
        let indirect = self.indirect_calls();
        if indirect.is_empty() {
            return Ok(());
        }
        let name = self.node.name(self.retained.binary);
        let in_symbol = indirect.iter().any(|u| Node::Item(u.from) == self.node);
        let in_items = indirect.iter().any(|u| Node::Item(u.from) != self.node);
        let calls = indirect.iter().any(|u| u.is_call);
        let jumps = indirect.iter().any(|u| !u.is_call);
        // The verb after one subject, and after two.
        let (one, two) = match (calls, jumps) {
            (true, true) => ("calls and jumps", "call and jump"),
            (true, false) => ("calls", "call"),
            (false, _) => ("jumps", "jump"),
        };
        let what = match (in_symbol, in_items) {
            (true, false) => format!("{name} also {one}"),
            (false, _) => format!("Code that only {name} leads to also {one}"),
            (true, true) => format!("{name} and the code that only it leads to also {two}"),
        };
        let (through, reach) = if indirect.len() == 1 {
            ("a pointer", "that reaches")
        } else {
            ("pointers", "they reach")
        };
        writeln!(out)?;
        writeln!(
            out,
            "{what} through {through}, which elfq can't follow, so what {reach} may count \
             elsewhere."
        )?;
        Ok(())
    }

    /// The items exclusive to SYMBOL that sizes count: all of them, or with
    /// a range, those with bytes in it. In no particular order.
    fn counted_items(&self) -> Vec<usize> {
        let retained = &self.retained;
        self.items()
            .into_iter()
            .filter(|&index| retained.range.is_none() || retained.own(index) > 0)
            .collect()
    }

    /// The items exclusive to SYMBOL's item, in no particular order.
    /// Imports are left out: their code isn't in the file.
    fn items(&self) -> Vec<usize> {
        self.retained
            .tree
            .exclusive(self.node)
            .into_iter()
            .filter_map(|node| match node {
                Node::Item(index) => Some(index),
                Node::Import(_) => None,
            })
            .collect()
    }
}
