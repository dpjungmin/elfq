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
//! line and picks its rows. `symbols.rs` finds the SYMBOLs, and the items
//! of a pattern or range given in place of SYMBOL, and puts them together
//! in a `symbol::Set`, whose words name them in the lead line; it leaves
//! out the functions whose end elfq doesn't know, and says why it can't
//! use those it can't. This file answers about one, and `several.rs` about
//! several, from the same parts and with the same words for the lead line,
//! and adds the notes on how the patterns matched to both. `json.rs` gives
//! the answer as JSON, in one shape for one SYMBOL or several.

mod json;
mod parts;
mod several;
mod symbols;

pub(super) use json::write_json;

use std::collections::{HashMap, HashSet};
use std::io::Write;

use anyhow::{Result, bail};

use super::checks::check_code_is_readable;
#[cfg(feature = "tui")]
use super::checks::readable_item;
use super::kept::Kept;
use super::retained_tree::RetainedTree;
#[cfg(feature = "tui")]
use super::symbol::Pick;
use super::symbol::{self, Names, Set};
use super::text::{Detail, Naming};
use super::tree_answer::TreeAnswer;
#[cfg(feature = "tui")]
use super::tree_answer::TreeTab;
use super::{Outcome, text};
use crate::binary::{AddressRange, Binary, FileKind};
use crate::cli::Format;
use crate::graph::{Graph, Node};
use parts::Parts;
use symbols::{Symbols, find_symbols, left_out_note};

/// Answers `elfq FILE retained SYMBOL...`: what removing the SYMBOLs in
/// `queries` at once would save, where a pattern stands for the items it
/// matches, and a range for the items in it. An item given twice, or by a
/// name and by an address, counts once, a function whose end elfq doesn't
/// know is left out, with a note, and when only one is left, the answer is
/// the one about it alone. The notes on how the patterns matched come
/// first, since they are the matches to check.
pub(super) fn run(
    binary: &Binary,
    queries: &[String],
    range: Option<AddressRange>,
    detail: Detail,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    check_code_is_readable(binary)?;
    let names = Kept::new(Names::new(binary));
    let symbols = find_symbols(binary, &names, queries, detail)?;
    let graph = Kept::new(Graph::build(binary));
    let retained = Kept::new(RetainedTree::naming(
        binary,
        &graph,
        range,
        names.into_inner(),
    ));
    let naming = Naming::of(detail);
    let set = &symbols.set;
    let mut answer = match (format, set.items()) {
        (Format::Json, found) => {
            let indexes: Vec<usize> = found.iter().map(|found| found.index).collect();
            write_json(&retained, &indexes, &symbols.left_out, set.patterns(), out)?;
            return Ok(Outcome::Answered);
        }
        (Format::Text | Format::Tree, [one]) => {
            // The one item of patterns or ranges given alone is named with
            // them.
            let lead_name = set.lead_name(binary, detail).to_string();
            let symbol = SymbolInTree {
                retained: &retained,
                index: one.index,
                lead_name: &lead_name,
            };
            let mut answer = text(&symbol, detail, format);
            if let Some(note) = left_out_note(binary, &symbols.left_out, &naming) {
                // Before the notes that every answer about removing items
                // has.
                answer.notes.insert(0, note);
            }
            answer
        }
        (Format::Text | Format::Tree, _) => several::text(&retained, &symbols, detail, format),
    };
    if let Some(patterns) = set.patterns() {
        answer
            .notes
            .splice(0..0, symbol::pattern_notes(binary, patterns, &naming));
    }
    answer.write(out)?;
    Ok(Outcome::Answered)
}

/// Why `retained` can't answer about `node`, for browse's tab 6, if it
/// can't: it is an import, or a function whose end elfq doesn't know, or
/// the file is a shared library. Otherwise, the index of `node`'s item.
#[cfg(feature = "tui")]
pub(super) fn refusal(binary: &Binary, node: Node) -> Result<usize> {
    let index = readable_item(binary, node)?;
    let lead_name = node.name(binary).to_string();
    let set = Set::new(binary, [Pick::Symbol { index, lead_name }]);
    refuse_shared_library(binary, &set, Detail::Brief)?;
    Ok(index)
}

/// Refuses a shared library, whose exported functions elfq doesn't yet
/// count as places where the program starts, so that it can't tell what
/// only the items of `set` lead to. It names them as the answer's first
/// line would (see [`Set::lead_name`]).
fn refuse_shared_library(binary: &Binary, set: &Set, detail: Detail) -> Result<()> {
    if binary.kind == FileKind::SharedLibrary {
        let lead = match set.items() {
            [_] => "leads",
            _ => "lead",
        };
        let what = set.lead_name(binary, detail);
        bail!("elfq can't yet tell what only {what} {lead} to in a shared library");
    }
    Ok(())
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
/// [`Detail::All`], every one, in the shape that `format` asks for, then
/// what it shares with other code and the calls that elfq can't follow.
pub(super) fn text(symbol: &SymbolInTree, detail: Detail, format: Format) -> TreeAnswer {
    if !symbol.is_reachable() {
        return TreeAnswer::sentences(vec![text::nothing_reaches(symbol.lead_name)]);
    }
    let alone = Alone::new(*symbol);
    alone.tree_text(&alone.shown(detail), detail, format)
}

/// What `retained SYMBOL` writes as text, as tab 6 of browse shows it, for
/// a tree without a range: with every item, as with `--all`; its notes as
/// the command writes them without `--all`, and naming only the first item
/// of each list, as `shorten` writes it; and whether the command needs
/// `--all` to print it all. What every form shares is worked out once.
#[cfg(feature = "tui")]
pub(super) fn tab(symbol: &SymbolInTree, shorten: &dyn Fn(&str) -> String) -> TreeTab {
    if !symbol.is_reachable() {
        let sentence = text::nothing_reaches(symbol.lead_name);
        return TreeTab::sentences(vec![sentence]);
    }
    let alone = Alone::new(*symbol);
    let (every, largest) = (alone.shown(Detail::All), alone.shown(Detail::Brief));
    let answer = alone.tree_text(&every, Detail::All, Format::Text);
    let brief = alone.parts.notes(&Naming::of(Detail::Brief));
    // Without --all, the tree leaves out items, or the notes names.
    let needs_all = largest.len() < every.len() || brief != answer.notes;
    TreeTab {
        briefest: alone.parts.notes(&Naming::first(shorten)),
        brief,
        needs_all,
        answer,
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
    /// under the tops and the items that keep them, or with
    /// [`Detail::All`], every one. Only items with bytes of their own
    /// compete for rows; an item that keeps bytes in a range but has none
    /// there shows only between others and SYMBOL, unlike in `top`. The
    /// tops don't compete for rows: a top shows only as the item that keeps
    /// rows that show. So SYMBOL, the only top, shows whenever the tree has
    /// rows.
    fn shown(&self, detail: Detail) -> HashSet<usize> {
        let tops = self.parts.removal.tops();
        (self.symbol.retained).largest(tops, &self.parts.under_tops, text::shown(detail))
    }

    /// The answer about removing SYMBOL alone, with the rows of the
    /// `shown` items, in the shape that `format` asks for, and with
    /// [`Detail::All`], every name in the notes.
    fn tree_text(&self, shown: &HashSet<usize>, detail: Detail, format: Format) -> TreeAnswer {
        let symbol = &self.symbol;
        let name = symbol.lead_name;
        let retained = symbol.retained;
        let binary = retained.binary;
        let index = symbol.index;
        let own = retained.own(index);
        let own_bytes = text::bytes_text(own);
        let range = (retained.range).map(|range| text::range_text(binary, range, detail));
        let removal = &self.parts.removal;
        let notes = self.parts.notes(&Naming::of(detail));
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
