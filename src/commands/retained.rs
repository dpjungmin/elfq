//! `elfq FILE retained SYMBOL`: what SYMBOL keeps in the file.
//!
//! The answer is the part of the dominator tree under SYMBOL: each item
//! hangs under the closest item that every path to it passes through, and
//! its row names that item, unless `--tree` draws the tree. Large trees
//! show only their largest items, unless `--all` asks for every one. With
//! `--range`, only the bytes in the range count.

use std::collections::HashSet;
use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::retained_tree::{self, KeptItem, RetainedTree};
use super::symbol::Names;
use super::{json, text};
use crate::binary::{AddressRange, Binary, FileKind};
use crate::cli::Format;
use crate::graph::{Node, Unresolved};
use crate::render::{Align, Table};

#[derive(Serialize)]
struct RetainedJson<'a> {
    symbol: json::Symbol<'a>,
    /// Whether anything reaches SYMBOL from where the program starts. When
    /// nothing does, nothing is there only because of it.
    reachable: bool,
    /// The range of memory that sizes count, or `null` for all of it.
    range: Option<json::Range>,
    /// SYMBOL's own bytes, or with a range, those in it, its load image's
    /// too.
    own: u64,
    /// How many of `own` are in SYMBOL's load image: 0 without a range.
    load_image: u64,
    /// SYMBOL's own bytes and those of everything it keeps: the bytes that
    /// would go away if it did. With a range, only those in it.
    retained: u64,
    /// The items that only SYMBOL leads to, as the rows of the tree in the
    /// text's order. Those with no `parent` are those that SYMBOL keeps
    /// itself. `retained` is `own` and their `own` together.
    items: Vec<KeptItem<'a>>,
    /// What SYMBOL and the items it keeps use that other code uses too, and
    /// so isn't counted, sorted by name. With a range, only items with bytes
    /// in it.
    shares: Vec<json::Item<'a>>,
    /// How many calls and jumps in SYMBOL and the items it keeps go through
    /// pointers set at run time, which elfq can't follow. A function called
    /// that way counts for the code or data that holds its address.
    indirect_calls: usize,
}

/// Answers `elfq FILE retained SYMBOL`: what removing SYMBOL would save.
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
    let node = symbol.node();
    let index = symbol.index;
    let name = &symbol.lead_name;
    let retained = &symbol.retained;
    let items = symbol.counted_items();
    let own = retained.own(index);
    let own_bytes = text::bytes_text(own);
    let range = range.map(|range| text::range_text(binary, range, all));
    if items.is_empty() {
        // How many of them are in its load image, if any are.
        let load_image = match retained.load_image(index) {
            0 => String::new(),
            bytes if bytes == own => ", all in its load image".to_string(),
            bytes => format!(", {bytes} of them in its load image"),
        };
        match &range {
            Some(range) if own == 0 => writeln!(
                out,
                "Removing {name} would save none of the bytes in {range}."
            )?,
            Some(range) => writeln!(
                out,
                "Removing {name} would save its own {own_bytes} in {range}{load_image}, and \
                 nothing else."
            )?,
            None => writeln!(
                out,
                "Removing {name} would save its own {own_bytes}, and nothing else."
            )?,
        }
        write_shared(&symbol, all, out)?;
        return symbol.write_indirect_note(out);
    }
    let in_range = range
        .map(|range| format!(" in {range}"))
        .unwrap_or_default();
    writeln!(
        out,
        "Removing {name} would save {}{in_range}:",
        text::bytes_text(retained.retained(index))
    )?;
    writeln!(out)?;
    let shown = retained.largest(&[node], &items, text::shown(all));
    let mut table = Table::new(&[
        ("RETAINED", Align::Right),
        ("OWN", Align::Right),
        ("ITEM", Align::Left),
    ]);
    let shape = retained_tree::shape(format);
    table.tree(&[retained.tree_row(&shown, index, shape)], shape);
    table.write(out)?;
    write_shared(&symbol, all, out)?;
    symbol.write_indirect_note(out)
}

fn write_json(symbol: &SymbolInTree, out: &mut dyn Write) -> Result<()> {
    let retained = &symbol.retained;
    let binary = retained.binary;
    let node = symbol.node();
    let reachable = symbol.is_reachable();
    let shares = if reachable {
        shared(symbol)
    } else {
        Vec::new()
    };
    let answer = RetainedJson {
        symbol: json::Symbol::new(binary, node),
        reachable,
        range: retained.range.map(json::Range::new),
        own: retained.own(symbol.index),
        load_image: retained.load_image(symbol.index),
        retained: retained.tree.retained(node),
        items: retained.to_json(retained.tree.children(node)),
        shares: shares
            .into_iter()
            .map(|node| json::Item::new(binary, node))
            .collect(),
        indirect_calls: symbol.indirect_calls().len(),
    };
    json::write(&answer, out)
}

/// What SYMBOL and the items it keeps use that other code uses too, and so
/// isn't counted, sorted by name. With a range, only items with bytes in it.
fn shared(symbol: &SymbolInTree) -> Vec<Node> {
    let binary = symbol.retained.binary;
    let mut kept: HashSet<Node> = symbol
        .retained
        .tree
        .exclusive(symbol.node())
        .into_iter()
        .collect();
    kept.insert(symbol.node());
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

/// Names what SYMBOL and the items it keeps use that other code uses too,
/// and so isn't counted, in order of name: as many as [`text::named`] says,
/// and how many more, or with `all`, every one.
fn write_shared(symbol: &SymbolInTree, all: bool, out: &mut dyn Write) -> Result<()> {
    let binary = symbol.retained.binary;
    let mut shared: Vec<&str> = shared(symbol)
        .into_iter()
        .map(|node| node.name(binary))
        .collect();
    shared.dedup();
    if shared.is_empty() {
        return Ok(());
    }
    let named = text::shown_count(shared.len(), text::named(all));
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
    /// SYMBOL's item, as an index into [`Binary::items`].
    index: usize,
    /// The item's name as the answer's first line gives it (see
    /// [`Symbol::lead_name`]).
    ///
    /// [`Symbol::lead_name`]: super::symbol::Symbol::lead_name
    lead_name: String,
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
        let names = Names::new(binary);
        let symbol = names.find(symbol)?;
        let node = symbol.node;
        let index = super::readable_item(binary, node)?;
        let name = node.name(binary);
        if binary.kind == FileKind::SharedLibrary {
            bail!(
                "elfq doesn't know yet which functions a shared library offers to other files, \
                 so it can't tell what only {name} leads to"
            );
        }
        let retained = RetainedTree::build(binary, range)?.with_names(names);
        Ok(SymbolInTree {
            retained,
            index,
            lead_name: symbol.lead_name(binary),
        })
    }

    /// SYMBOL's item, as a node of the graph.
    fn node(&self) -> Node {
        Node::Item(self.index)
    }

    /// Whether anything reaches SYMBOL from where the program starts.
    fn is_reachable(&self) -> bool {
        self.retained.tree.is_reachable(self.node())
    }

    /// Says that nothing reaches SYMBOL, so it is dead code, or used in a way
    /// elfq can't see.
    fn write_unreachable(&self, out: &mut dyn Write) -> Result<()> {
        writeln!(out, "{}", text::nothing_reaches(&self.lead_name))?;
        Ok(())
    }

    /// The calls and jumps through pointers set at run time in SYMBOL and
    /// the items exclusive to it.
    fn indirect_calls(&self) -> Vec<Unresolved> {
        let graph = &self.retained.graph;
        std::iter::once(self.index)
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
        let name = self.node().name(self.retained.binary);
        let in_symbol = indirect.iter().any(|u| u.from == self.index);
        let in_items = indirect.iter().any(|u| u.from != self.index);
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
            .exclusive(self.node())
            .into_iter()
            .filter_map(|node| match node {
                Node::Item(index) => Some(index),
                Node::Import(_) => None,
            })
            .collect()
    }
}
