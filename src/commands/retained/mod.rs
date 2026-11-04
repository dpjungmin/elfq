//! `elfq FILE retained SYMBOL...`: what SYMBOL keeps in the file, or what
//! several SYMBOLs keep when they go at once.
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
//! everything it keeps. [`Parts`] works out from the removal the bytes
//! saved, the items under the tops and the notes; the answer adds its lead
//! line and picks its rows. This file finds the SYMBOLs and answers about
//! one, and `several.rs` about several, from the same parts and with the
//! same words for the lead line.

mod parts;
mod several;

use std::collections::{HashMap, HashSet};
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
use crate::graph::{Graph, Node};
use parts::Parts;

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

/// Answers `elfq FILE retained SYMBOL...`: what removing the SYMBOLs in
/// `queries` at once would save. A SYMBOL given twice, or by a name and by
/// an address, counts once, and when only one is left, the answer is the
/// one about it alone.
pub(super) fn run(
    binary: &Binary,
    queries: &[String],
    range: Option<AddressRange>,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    check_code_is_readable(binary)?;
    let names = Kept::new(Names::new(binary));
    let found = find_symbols(binary, &names, queries)?;
    let graph = Kept::new(Graph::build(binary));
    let retained = Kept::new(RetainedTree::naming(
        binary,
        &graph,
        range,
        names.into_inner(),
    ));
    match &found[..] {
        [one] => {
            let symbol = SymbolInTree {
                retained: &retained,
                index: one.index,
                lead_name: &one.lead_name,
            };
            match format {
                Format::Json => write_json(&symbol, out)?,
                Format::Text | Format::Tree => answer(&symbol, all, format).write(out)?,
            }
        }
        // `arguments.rs` refuses --json with several SYMBOLs.
        _ => several::tree_answer(&retained, &found, all, format).write(out)?,
    }
    Ok(Outcome::Answered)
}

/// A SYMBOL that `retained` was given, and found.
struct Found {
    /// Its item, as an index into [`Binary::items`].
    index: usize,
    /// The item's name as the answer gives it (see [`Symbol::lead_name`]).
    ///
    /// [`Symbol::lead_name`]: super::symbol::Symbol::lead_name
    lead_name: String,
}

/// Finds what each of `queries` names, by `names`, and refuses an import,
/// a function whose end elfq doesn't know and a shared library, as with
/// one SYMBOL: the first argument that elfq can't answer about stops it,
/// with its error. Each item comes once, in the order given.
fn find_symbols(binary: &Binary, names: &Names, queries: &[String]) -> Result<Vec<Found>> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for query in queries {
        let symbol = names.find(query)?;
        let index = readable_item(binary, symbol.node)?;
        if seen.insert(index) {
            let lead_name = symbol.lead_name(binary);
            found.push(Found { index, lead_name });
        }
    }
    let items: Vec<&str> = (found.iter())
        .map(|found| binary.items[found.index].name.as_str())
        .collect();
    refuse_shared_library(binary, &items)?;
    Ok(found)
}

/// Why `retained` can't answer about `node`, for browse's tab 6, if it
/// can't: it is an import, or a function whose end elfq doesn't know, or
/// the file is a shared library. Otherwise, the index of `node`'s item.
#[cfg(feature = "tui")]
pub(super) fn refusal(binary: &Binary, node: Node) -> Result<usize> {
    let index = readable_item(binary, node)?;
    refuse_shared_library(binary, &[node.name(binary)])?;
    Ok(index)
}

/// Refuses a shared library, whose exported functions elfq doesn't yet
/// count as places where the program starts, so that it can't tell what
/// only the items called `names` lead to.
fn refuse_shared_library(binary: &Binary, names: &[&str]) -> Result<()> {
    if binary.kind == FileKind::SharedLibrary {
        let lead = match names {
            [_] => "leads",
            _ => "lead",
        };
        let what = subject(names);
        bail!("elfq can't yet tell what only {what} {lead} to in a shared library");
    }
    Ok(())
}

/// How the answer names the SYMBOLs called `names`: "a", "a and b", or
/// from three on, "these 6 items", so that its first line stays short
/// however many there are.
fn subject(names: &[&str]) -> String {
    match names {
        [one] => one.to_string(),
        [first, second] => format!("{first} and {second}"),
        _ => format!("these {} items", names.len()),
    }
}

/// What removing the SYMBOLs called `subject` would save, as the answer's
/// first line says it before the colon that leads to a table or the
/// period that ends it: `saved` bytes, or with a `range`, those in it, as
/// in "Removing crc32 would save 1083 bytes", or "Removing crc32 would
/// save none of the bytes in RANGE" when it saves none of them.
fn would_save(subject: &str, saved: u64, range: Option<&str>) -> String {
    let bytes = text::bytes_text(saved);
    match range {
        Some(range) if saved == 0 => {
            format!("Removing {subject} would save none of the bytes in {range}")
        }
        Some(range) => format!("Removing {subject} would save {bytes} in {range}"),
        None => format!("Removing {subject} would save {bytes}"),
    }
}

/// What `retained SYMBOL` writes as text: what removing SYMBOL would save,
/// and the tree of the items that only it leads to, the largest 30 or with
/// `all`, every one, in the shape that `format` asks for, then what it
/// shares with other code and the calls that elfq can't follow.
pub(super) fn answer(symbol: &SymbolInTree, all: bool, format: Format) -> TreeAnswer {
    if !symbol.is_reachable() {
        return TreeAnswer::sentences(vec![text::nothing_reaches(symbol.lead_name)]);
    }
    let alone = Alone::new(*symbol);
    alone.tree_answer(&alone.shown(all), all, format)
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
    let alone = Alone::new(*symbol);
    let (every, largest) = (alone.shown(true), alone.shown(false));
    let answer = alone.tree_answer(&every, true, Format::Text);
    let brief = alone.parts.notes(&Naming::of(false));
    // Without --all, the tree leaves out items, or the notes names.
    let needs_all = largest.len() < every.len() || brief != answer.notes;
    TabAnswer {
        briefest: alone.parts.notes(&Naming::first(shorten)),
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
    let parts = Parts::new(retained, &[symbol.index]);
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

/// SYMBOL, and what removing it alone loses, which every form of the
/// answer about it shares, worked out once.
struct Alone<'t, 'g, 'a> {
    symbol: SymbolInTree<'t, 'g, 'a>,
    parts: Parts<'t, 'g, 'a>,
}

impl<'t, 'g, 'a> Alone<'t, 'g, 'a> {
    /// Works out what removing `symbol` alone loses.
    fn new(symbol: SymbolInTree<'t, 'g, 'a>) -> Alone<'t, 'g, 'a> {
        let parts = Parts::new(symbol.retained, &[symbol.index]);
        Alone { symbol, parts }
    }

    /// The items that the tree shows: the largest 30 of the lost items
    /// under the tops and the items that keep them, or with `all`, every
    /// one. Only items with bytes of their own compete for rows; an item
    /// that keeps bytes in a range but has none there shows only between
    /// others and SYMBOL, unlike in `top`. The tops don't compete for
    /// rows: a top shows only as the item that keeps rows that show. So
    /// SYMBOL, the only top, shows whenever the tree has rows.
    fn shown(&self, all: bool) -> HashSet<usize> {
        let tops = self.parts.removal.tops();
        (self.symbol.retained).largest(tops, &self.parts.under_tops, text::shown(all))
    }

    /// The answer about removing SYMBOL alone, with the rows of the
    /// `shown` items, in the shape that `format` asks for, and with `all`,
    /// every name in the notes.
    fn tree_answer(&self, shown: &HashSet<usize>, all: bool, format: Format) -> TreeAnswer {
        let symbol = &self.symbol;
        let name = symbol.lead_name;
        let retained = symbol.retained;
        let binary = retained.binary;
        let index = symbol.index;
        let own = retained.own(index);
        let own_bytes = text::bytes_text(own);
        let range = (retained.range).map(|range| text::range_text(binary, range, all));
        let removal = &self.parts.removal;
        let notes = self.parts.notes(&Naming::of(all));
        if self.parts.under_tops.is_empty() {
            // How many of them are in its load image, if any are.
            let load_image = match retained.load_image(index) {
                0 => String::new(),
                bytes if bytes == own => ", all in its load image".to_string(),
                bytes => format!(", {bytes} of them in its load image"),
            };
            let lead = match &range {
                Some(_) if own == 0 => format!("{}.", would_save(name, own, range.as_deref())),
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
        let saves = would_save(name, removal.retained(), range.as_deref());
        TreeAnswer {
            lead: vec![format!("{saves}:")],
            table: Some(retained.table(shown, removal.tops(), &HashMap::new(), format)),
            notes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::subject;

    #[test]
    fn the_lead_names_one_or_two_symbols_and_counts_more() {
        assert_eq!(subject(&["crc32"]), "crc32");
        assert_eq!(
            subject(&["process_frame", "report"]),
            "process_frame and report"
        );
        assert_eq!(
            subject(&["crc32", "apply_filter", "filter_tap"]),
            "these 3 items"
        );
    }
}
