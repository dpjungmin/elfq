//! `elfq FILE retained SYMBOL`: what SYMBOL keeps in the file.
//!
//! The answer is the part of the dominator tree under SYMBOL: each item
//! hangs under the closest item that every path to it passes through, and
//! its row names that item, unless `--tree` draws the tree. Large trees
//! show only their largest items, unless `--all` asks for every one. With
//! `--range`, only the bytes in the range count.
//!
//! What removing SYMBOL loses is what `graph::Removal` finds: the branches
//! of the tree under the removal's tops. For a SYMBOL that something
//! reaches, the only top is SYMBOL itself, so what is lost is SYMBOL and
//! everything it keeps. [`Parts`] works out the bytes saved, the table and
//! the notes from the removal.

use std::collections::HashSet;
use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::checks::{check_code_is_readable, readable_item};
use super::kept::Kept;
use super::retained_tree::{KeptItem, RetainedTree};
use super::symbol::Names;
use super::text::Naming;
#[cfg(feature = "tui")]
use super::tree_answer::TabAnswer;
use super::tree_answer::TreeAnswer;
use super::{Outcome, json, text};
use crate::binary::{AddressRange, Binary, FileKind};
use crate::cli::Format;
use crate::graph::{Graph, Node, Removal, Unresolved};

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
pub(super) fn run(
    binary: &Binary,
    symbol: &str,
    range: Option<AddressRange>,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    check_code_is_readable(binary)?;
    let names = Kept::new(Names::new(binary));
    let symbol = names.find(symbol)?;
    let node = symbol.node;
    let index = refusal(binary, node)?;
    let lead_name = symbol.lead_name(binary);
    let graph = Kept::new(Graph::build(binary));
    let retained = Kept::new(RetainedTree::naming(
        binary,
        &graph,
        range,
        names.into_inner(),
    ));
    let symbol = SymbolInTree {
        retained: &retained,
        index,
        lead_name: &lead_name,
    };
    match format {
        Format::Json => write_json(&symbol, out)?,
        Format::Text | Format::Tree => answer(&symbol, all, format).write(out)?,
    }
    Ok(Outcome::Answered)
}

/// Why `retained` can't answer about `node`, if it can't: it is an import,
/// or a function whose end elfq doesn't know, or the file is a shared
/// library. Otherwise, the index of `node`'s item.
pub(super) fn refusal(binary: &Binary, node: Node) -> Result<usize> {
    let index = readable_item(binary, node)?;
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
pub(super) fn answer(symbol: &SymbolInTree, all: bool, format: Format) -> TreeAnswer {
    if !symbol.is_reachable() {
        return TreeAnswer::sentences(vec![text::nothing_reaches(symbol.lead_name)]);
    }
    let parts = Parts::new(*symbol);
    parts.answer(&parts.shown(all), all, format)
}

/// What `retained SYMBOL` writes as text, as tab 6 of browse shows it, for
/// a tree without a range: with every item, as with `--all`; its notes as
/// the command writes them without `--all`, and naming only the first item
/// of each list, as `shorten` writes it; and whether the command needs
/// `--all` to print it all. What every form shares is worked out once.
#[cfg(feature = "tui")]
pub(super) fn answers(symbol: &SymbolInTree, shorten: &dyn Fn(&str) -> String) -> TabAnswer {
    if !symbol.is_reachable() {
        let sentence = text::nothing_reaches(symbol.lead_name);
        return TabAnswer::sentences(vec![sentence]);
    }
    let parts = Parts::new(*symbol);
    let (every, largest) = (parts.shown(true), parts.shown(false));
    let answer = parts.answer(&every, true, Format::Text);
    let brief = parts.notes(&Naming::of(false));
    // Without --all, the tree leaves out items, or the notes names.
    let needs_all = largest.len() < every.len() || brief != answer.notes;
    TabAnswer {
        briefest: parts.notes(&Naming::first(shorten)),
        brief,
        needs_all,
        answer,
    }
}

/// Writes what `retained SYMBOL --json` writes about the symbol.
pub(super) fn write_json(symbol: &SymbolInTree, out: &mut dyn Write) -> Result<()> {
    let retained = symbol.retained;
    let binary = retained.binary;
    let node = symbol.node();
    let parts = Parts::new(*symbol);
    let answer = RetainedJson {
        symbol: json::Symbol::new(binary, node),
        reachable: symbol.is_reachable(),
        range: retained.range.map(json::Range::new),
        own: retained.own(symbol.index),
        load_image: retained.load_image(symbol.index),
        retained: parts.removal.retained(),
        items: retained.to_json(retained.tree.children(node)),
        shares: (parts.shared.iter())
            .map(|&node| json::Item::new(binary, node))
            .collect(),
        indirect_calls: parts.indirect.len(),
    };
    json::write(&answer, out)
}

/// What the nodes that `removal` loses use that other code uses too, and
/// so isn't counted, sorted by name. With a range, only items with bytes in
/// it. Nothing is lost when nothing reaches the removed items, and so
/// nothing is shared either.
fn shared(retained: &RetainedTree, removal: &Removal) -> Vec<Node> {
    let binary = retained.binary;
    let mut shared: Vec<Node> = (retained.graph.references.iter())
        .filter(|r| removal.is_lost(Node::Item(r.from)) && !removal.is_lost(r.to))
        .map(|r| r.to)
        .filter(|&node| match (retained.range, node) {
            (None, _) => true,
            (Some(_), Node::Item(index)) => retained.own(index) > 0,
            (Some(_), Node::Import(_)) => false,
        })
        .collect();
    shared.sort_by_key(|&node| (node.name(binary), node));
    shared.dedup();
    shared
}

/// What every form of the answer about SYMBOL shares, worked out once:
/// what removing it loses, and what the notes name. The bytes saved, the
/// notes and the table of the trees under the removal's tops depend on
/// SYMBOL only through the removal. Which items compete for rows, and the
/// lead line, are worked out for one SYMBOL only (see [`Parts::shown`]).
struct Parts<'t, 'g, 'a> {
    symbol: SymbolInTree<'t, 'g, 'a>,
    /// What removing SYMBOL loses: the branches of the tree under the
    /// removal's tops. For a SYMBOL that something reaches, the only top
    /// is SYMBOL itself, so what is lost is SYMBOL and everything it keeps.
    /// For one that nothing reaches, nothing is lost.
    removal: Removal,
    /// The lost items under the tops that sizes count: all of them, or with
    /// a range, those with bytes of their own in it. For SYMBOL, the items
    /// that it keeps. In no particular order.
    under_tops: Vec<usize>,
    /// What the lost items use that other code uses too (see [`shared`]).
    shared: Vec<Node>,
    /// The calls and jumps through pointers set at run time in the items
    /// that go: the lost ones, and the removed ones that nothing reaches.
    indirect: Vec<Unresolved>,
}

impl<'t, 'g, 'a> Parts<'t, 'g, 'a> {
    /// The parts of the answer about `symbol`, from what removing it loses,
    /// which every part starts from.
    fn new(symbol: SymbolInTree<'t, 'g, 'a>) -> Parts<'t, 'g, 'a> {
        let retained = symbol.retained;
        let (graph, tree) = (retained.graph, &retained.tree);
        let removal = Removal::find(retained.binary, graph, tree, &[symbol.node()]);
        // Imports are left out: their code isn't in the file.
        let item_index = |node: Node| match node {
            Node::Item(index) => Some(index),
            Node::Import(_) => None,
        };
        // A removed item that nothing reaches isn't lost, but it goes too.
        let indirect = (removal.lost().chain(removal.unreachable()))
            .filter_map(item_index)
            .flat_map(|item| graph.unresolved_in(item))
            .filter(|unresolved| unresolved.is_indirect())
            .copied()
            .collect();
        let under_tops = (removal.tops().iter())
            .flat_map(|&top| tree.exclusive(top))
            .filter_map(item_index)
            .filter(|&index| retained.range.is_none() || retained.own(index) > 0)
            .collect();
        Parts {
            symbol,
            shared: shared(retained, &removal),
            indirect,
            under_tops,
            removal,
        }
    }

    /// The items that the tree shows: the largest 30 of the lost items
    /// under the tops and the items that keep them, or with `all`, every
    /// one. Only items with bytes of their own compete for rows; an item
    /// that keeps bytes in a range but has none there shows only between
    /// others and SYMBOL, unlike in `top`. The tops don't compete for
    /// rows: a top shows only as the item that keeps rows that show. So
    /// SYMBOL, the only top, shows whenever the tree has rows.
    fn shown(&self, all: bool) -> HashSet<usize> {
        let retained = self.symbol.retained;
        retained.largest(self.removal.tops(), &self.under_tops, text::shown(all))
    }

    /// The answer about removing SYMBOL, with the rows of the `shown`
    /// items, in the shape that `format` asks for, and with `all`, every
    /// name in the notes.
    fn answer(&self, shown: &HashSet<usize>, all: bool, format: Format) -> TreeAnswer {
        let symbol = &self.symbol;
        let name = symbol.lead_name;
        let retained = symbol.retained;
        let binary = retained.binary;
        let index = symbol.index;
        let own = retained.own(index);
        let own_bytes = text::bytes_text(own);
        let range = (retained.range).map(|range| text::range_text(binary, range, all));
        let notes = self.notes(&Naming::of(all));
        if self.under_tops.is_empty() {
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
            text::bytes_text(self.removal.retained())
        );
        TreeAnswer {
            lead: vec![lead],
            table: Some(retained.table(shown, self.removal.tops(), format)),
            notes,
        }
    }

    /// The notes under the tree, naming the items they list as `naming`
    /// says: what the lost items share with other code, and the calls that
    /// elfq can't follow.
    fn notes(&self, naming: &Naming) -> Vec<String> {
        [self.shared_note(naming), self.indirect_note(naming)]
            .into_iter()
            .flatten()
            .collect()
    }

    /// Names what the lost items use that other code uses too, and so
    /// isn't counted, in order of name, as `naming` names them. `None` when
    /// there is nothing.
    fn shared_note(&self, naming: &Naming) -> Option<String> {
        let binary = self.symbol.retained.binary;
        let mut shared: Vec<&str> = (self.shared.iter()).map(|node| node.name(binary)).collect();
        shared.dedup();
        if shared.is_empty() {
            return None;
        }
        Some(format!(
            "Shared with other code, so not counted: {}.",
            naming.list(&shared)
        ))
    }

    /// Says that what the code of the items that go calls through pointers
    /// may count for other items, if any of it calls anything that way,
    /// naming the functions that do, as `naming` names them: the removed
    /// ones first, then the others.
    fn indirect_note(&self, naming: &Naming) -> Option<String> {
        let indirect = &self.indirect;
        if indirect.is_empty() {
            return None;
        }
        let removal = &self.removal;
        let mut callers: Vec<usize> = indirect.iter().map(|u| u.from).collect();
        callers.sort_by_key(|&index| (!removal.is_removed(Node::Item(index)), index));
        callers.dedup();
        let binary = self.symbol.retained.binary;
        let names: Vec<&str> = callers
            .iter()
            .map(|&index| Node::Item(index).name(binary))
            .collect();
        let (one, several) = text::call_and_jump_verbs(indirect);
        let verb = if names.len() == 1 { one } else { several };
        let (through, reach) = if indirect.len() == 1 {
            ("a pointer", "that reaches")
        } else {
            ("pointers", "they reach")
        };
        Some(format!(
            "{} also {verb} through {through}, so what {reach} may count elsewhere.",
            naming.phrase(&names)
        ))
    }
}

/// SYMBOL's item, in the tree of retained sizes that says what only it
/// leads to.
#[derive(Clone, Copy)]
pub(super) struct SymbolInTree<'t, 'g, 'a> {
    pub(super) retained: &'t RetainedTree<'g, 'a>,
    /// SYMBOL's item, as an index into [`Binary::items`].
    pub(super) index: usize,
    /// The item's name as the answer's first line gives it (see
    /// [`Symbol::lead_name`]).
    ///
    /// [`Symbol::lead_name`]: super::symbol::Symbol::lead_name
    pub(super) lead_name: &'t str,
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
}
