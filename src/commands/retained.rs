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
use super::{Kept, TreeAnswer, json, text};
use crate::binary::{AddressRange, Binary, FileKind};
use crate::cli::Format;
use crate::graph::{Graph, Node, Unresolved};
use crate::render::Align;

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
    super::check_code_is_readable(binary)?;
    let names = Kept::new(Names::new(binary));
    let symbol = names.find(symbol)?;
    let node = symbol.node;
    let index = refusal(binary, node)?;
    let lead_name = symbol.lead_name(binary);
    let graph = Kept::new(Graph::build(binary));
    let retained =
        Kept::new(RetainedTree::new(binary, &graph, range).with_names(names.into_inner()));
    let symbol = SymbolInTree {
        retained: &retained,
        index,
        lead_name: &lead_name,
    };
    if format == Format::Json {
        return write_json(&symbol, out);
    }
    answer(&symbol, all, format).write(out)
}

/// Why `retained` can't answer about `node`, if it can't: it is an import,
/// or a function whose end elfq doesn't know, or the file is a shared
/// library. Otherwise, the index of `node`'s item.
pub(super) fn refusal(binary: &Binary, node: Node) -> Result<usize> {
    let index = super::readable_item(binary, node)?;
    if binary.kind == FileKind::SharedLibrary {
        let name = node.name(binary);
        bail!("elfq can't yet tell what only {name} leads to in a shared library");
    }
    Ok(index)
}

/// What `retained SYMBOL` writes as text: what removing SYMBOL would save,
/// and the tree of the items that only it leads to, the largest 30 or with
/// `all`, every one, in the shape that `format` asks for, then what it
/// shares with other code and the calls that elfq can't follow.
fn answer(symbol: &SymbolInTree, all: bool, format: Format) -> TreeAnswer {
    if !symbol.is_reachable() {
        return TreeAnswer::sentences(vec![text::nothing_reaches(symbol.lead_name)]);
    }
    let name = symbol.lead_name;
    let retained = symbol.retained;
    let binary = retained.binary;
    let index = symbol.index;
    let items = symbol.counted_items();
    let own = retained.own(index);
    let own_bytes = text::bytes_text(own);
    let range = (retained.range).map(|range| text::range_text(binary, range, all));
    let notes: Vec<String> = [shared_note(symbol, all), symbol.indirect_note(all)]
        .into_iter()
        .flatten()
        .collect();
    if items.is_empty() {
        // How many of them are in its load image, if any are.
        let load_image = match retained.load_image(index) {
            0 => String::new(),
            bytes if bytes == own => ", all in its load image".to_string(),
            bytes => format!(", {bytes} of them in its load image"),
        };
        let lead = match &range {
            Some(range) if own == 0 => {
                format!("Removing {name} would save none of the bytes in {range}.")
            }
            Some(range) => {
                format!(
                    "Removing {name} would save only its own {own_bytes} in {range}{load_image}."
                )
            }
            None => format!("Removing {name} would save only its own {own_bytes}."),
        };
        return TreeAnswer {
            notes,
            ..TreeAnswer::sentences(vec![lead])
        };
    }
    let in_range = range
        .map(|range| format!(" in {range}"))
        .unwrap_or_default();
    let lead = format!(
        "Removing {name} would save {}{in_range}:",
        text::bytes_text(retained.retained(index))
    );
    let shown = retained.largest(&[symbol.node()], &items, text::shown(all));
    retained.names().will_name(shown.len());
    let shape = retained_tree::shape(format);
    TreeAnswer {
        lead: vec![lead],
        columns: vec![
            ("RETAINED", Align::Right),
            ("OWN", Align::Right),
            ("ITEM", Align::Left),
        ],
        rows: vec![retained.tree_row(&shown, index, shape)],
        shape,
        notes,
    }
}

/// Writes what `retained SYMBOL --json` writes about the symbol.
pub(super) fn write_json(symbol: &SymbolInTree, out: &mut dyn Write) -> Result<()> {
    let retained = symbol.retained;
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
/// and how many more, or with `all`, every one. `None` when there is
/// nothing.
fn shared_note(symbol: &SymbolInTree, all: bool) -> Option<String> {
    let binary = symbol.retained.binary;
    let mut shared: Vec<&str> = shared(symbol)
        .into_iter()
        .map(|node| node.name(binary))
        .collect();
    shared.dedup();
    if shared.is_empty() {
        return None;
    }
    let named = text::shown_count(shared.len(), text::named(all));
    let mut names = shared[..named].join(", ");
    if named < shared.len() {
        names += &format!(" and {} more", shared.len() - named);
    }
    Some(format!("Shared with other code, so not counted: {names}."))
}

/// SYMBOL's item, in the tree of retained sizes that says what only it
/// leads to.
pub(super) struct SymbolInTree<'t, 'g, 'a> {
    pub retained: &'t RetainedTree<'g, 'a>,
    /// SYMBOL's item, as an index into [`Binary::items`].
    pub index: usize,
    /// The item's name as the answer's first line gives it (see
    /// [`Symbol::lead_name`]).
    ///
    /// [`Symbol::lead_name`]: super::symbol::Symbol::lead_name
    pub lead_name: &'t str,
}

impl SymbolInTree<'_, '_, '_> {
    /// SYMBOL's item, as a node of the graph.
    fn node(&self) -> Node {
        Node::Item(self.index)
    }

    /// Whether anything reaches SYMBOL from where the program starts.
    fn is_reachable(&self) -> bool {
        self.retained.tree.is_reachable(self.node())
    }

    /// The calls and jumps through pointers set at run time in SYMBOL and
    /// the items exclusive to it.
    fn indirect_calls(&self) -> Vec<Unresolved> {
        let graph = self.retained.graph;
        std::iter::once(self.index)
            .chain(self.items())
            .flat_map(|item| graph.unresolved_in(item))
            .filter(|unresolved| unresolved.is_indirect())
            .copied()
            .collect()
    }

    /// Says that what SYMBOL's code calls through pointers may count for
    /// other items, if it calls anything that way, naming the functions that
    /// do: SYMBOL first, then those that only it leads to, as many as
    /// [`text::names_text`] names, or with `all`, every one.
    fn indirect_note(&self, all: bool) -> Option<String> {
        let indirect = self.indirect_calls();
        if indirect.is_empty() {
            return None;
        }
        let mut callers: Vec<usize> = indirect.iter().map(|u| u.from).collect();
        callers.sort_by_key(|&index| (index != self.index, index));
        callers.dedup();
        let binary = self.retained.binary;
        let names: Vec<&str> = callers
            .iter()
            .map(|&index| Node::Item(index).name(binary))
            .collect();
        let (one, several) = text::call_and_jump_verbs(&indirect);
        let verb = if names.len() == 1 { one } else { several };
        let (through, reach) = if indirect.len() == 1 {
            ("a pointer", "that reaches")
        } else {
            ("pointers", "they reach")
        };
        Some(format!(
            "{} also {verb} through {through}, so what {reach} may count elsewhere.",
            text::names_text(&names, all)
        ))
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
