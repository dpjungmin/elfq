//! `retained`'s answer as JSON, in one shape for one SYMBOL or several:
//! the SYMBOLs, how the patterns given matched, if any were, what removing
//! them all at once would save, how many of those bytes go only when
//! several go, and the rows of the trees of what goes. The rows are the
//! text's, every one of them, the SYMBOLs' own rows among them, and a row
//! that several SYMBOLs keep only together names them.

use std::collections::HashMap;
use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::parts::Parts;
use crate::binary::total;
use crate::commands::json;
use crate::commands::retained_tree::{KeptItem, RetainedTree};
use crate::commands::symbol::PatternMatches;
use crate::graph::Node;

#[derive(Serialize)]
struct RetainedJson<'a> {
    /// The SYMBOLs' items, each once, in the order given, and for a
    /// pattern or range given in place of SYMBOL, the items it matched or
    /// that are in it, in address order.
    symbols: Vec<SymbolJson<'a>>,
    /// Only when a pattern was given in place of SYMBOL: each item that
    /// each pattern matched, in the order the patterns were given, and each
    /// pattern's in address order, with how it matched. An item that two
    /// patterns matched has a row for each. Each of these items is in
    /// `symbols`, or when elfq doesn't know where it ends, in `left_out`.
    #[serde(skip_serializing_if = "Option::is_none")]
    matches: Option<Vec<json::Match<'a>>>,
    /// The functions given, or in a range given, whose end elfq doesn't
    /// know, each once, in the order given: elfq can't read their code, so
    /// nothing here counts them, and `symbols` doesn't list them.
    left_out: Vec<json::Symbol<'a>>,
    /// The range of memory that sizes count, or `null` for all of it.
    range: Option<json::Range>,
    /// The SYMBOLs' own bytes, or with a range, those in it, their load
    /// images' too.
    own: u64,
    /// How many of `own` are in the SYMBOLs' load images: 0 without a
    /// range.
    load_image: u64,
    /// What removing the SYMBOLs all at once would save: the `retained` of
    /// each row whose `parent` is `null`, and the `own` of each SYMBOL that
    /// nothing reaches. With a range, only the bytes in it.
    retained: u64,
    /// How many of `retained` go only when two or more SYMBOLs go:
    /// removing any one of them alone saves none of these bytes. 0 for one
    /// SYMBOL.
    kept_together: u64,
    /// The rows of the trees of what goes, every one, in the text's order.
    /// A SYMBOL that nothing reaches has none, and with a range, neither
    /// has an item that keeps none of its bytes. Those with no `parent` are
    /// the tops of what goes: the items that no item that goes keeps, each
    /// a SYMBOL or an item that several SYMBOLs keep only together.
    items: Vec<RowJson<'a>>,
    /// What the items that go use that other code uses too, and so isn't
    /// counted, sorted by name. With a range, only items with bytes in it.
    shares: Vec<json::Item<'a>>,
    /// How many calls and jumps in the items that go are through pointers
    /// set at run time, which elfq can't follow. A function called that
    /// way counts for the code or data that holds its address.
    indirect_calls: usize,
}

/// A SYMBOL's item, as `symbols` lists it.
#[derive(Serialize)]
struct SymbolJson<'a> {
    #[serde(flatten)]
    symbol: json::Symbol<'a>,
    /// Whether anything reaches it from where the program starts. When
    /// nothing does, nothing is there only because of it, and it has no
    /// row.
    reachable: bool,
    /// Its own bytes, as in the OWN column: all of them, or with a range,
    /// those in it.
    own: u64,
    /// The `id` of its row in `items`, or `null` when it has none: nothing
    /// reaches it, or with a range, it keeps none of the range's bytes.
    row: Option<usize>,
    /// Whether the other SYMBOLs keep it, alone or together, so that it
    /// adds nothing to `retained`.
    kept_by_others: bool,
}

/// A row of the tree, as `items` lists it: the row as `top` writes it, and
/// for a row kept together, the SYMBOLs it needs.
#[derive(Serialize)]
struct RowJson<'a> {
    /// The row's item, as an index into [`Binary::items`], by which a
    /// SYMBOL finds its row. Not written.
    ///
    /// [`Binary::items`]: crate::binary::Binary::items
    #[serde(skip)]
    index: usize,
    /// The row as `top` writes it.
    #[serde(flatten)]
    kept: KeptItem<'a>,
    /// For a row at the top that isn't a SYMBOL's own, and so goes only
    /// because several SYMBOLs keep it together, the addresses of the
    /// SYMBOLs it needs, in the order given: removing just those removes
    /// it, and removing all of them but one keeps it. Otherwise `null`,
    /// even on the row of a SYMBOL that the others keep only together,
    /// whose `kept_by_others` says that it adds nothing (see
    /// [`Parts::keepers`]).
    kept_together_by: Option<Vec<String>>,
}

/// Writes what `retained SYMBOL... --json` writes about removing the items
/// at `symbols`, as indexes into [`Binary::items`], each once and in the
/// order given, all at once from the tree `retained`, the functions it
/// leaves out, at `left_out`, and when patterns were given in place of
/// SYMBOL, `patterns`, with the items they matched.
///
/// [`Binary::items`]: crate::binary::Binary::items
pub(in crate::commands) fn write_json(
    retained: &RetainedTree,
    symbols: &[usize],
    left_out: &[usize],
    patterns: Option<&[PatternMatches]>,
    out: &mut dyn Write,
) -> Result<()> {
    let binary = retained.binary;
    let parts = Parts::new(retained, symbols);
    let removal = &parts.removal;
    let keepers = parts.keepers();
    let items = retained.to_json_with(removal.tops(), |index, kept| RowJson {
        index,
        kept,
        kept_together_by: (keepers.get(&index)).map(|needed| {
            (needed.iter())
                .map(|&keeper| json::address(binary.items[keeper].address))
                .collect()
        }),
    });
    // A row's id is its place in `items`.
    let ids: HashMap<usize, usize> = (items.iter().enumerate())
        .map(|(id, row)| (row.index, id))
        .collect();
    let answer = RetainedJson {
        symbols: (symbols.iter())
            .map(|&index| SymbolJson {
                symbol: json::Symbol::new(binary, Node::Item(index)),
                reachable: retained.tree.is_reachable(Node::Item(index)),
                own: retained.own(index),
                row: ids.get(&index).copied(),
                kept_by_others: removal.is_kept_by_others(Node::Item(index)),
            })
            .collect(),
        matches: patterns.map(|patterns| {
            (patterns.iter())
                .flat_map(|matched| {
                    (matched.matches.iter())
                        .map(|found| json::Match::new(binary, &matched.pattern, found))
                })
                .collect()
        }),
        left_out: (left_out.iter())
            .map(|&index| json::Symbol::new(binary, Node::Item(index)))
            .collect(),
        range: retained.range.map(json::Range::new),
        own: total(symbols.iter().map(|&index| retained.own(index))),
        load_image: total(symbols.iter().map(|&index| retained.load_image(index))),
        retained: removal.retained(),
        kept_together: parts.bytes_kept_together(),
        items,
        shares: (parts.shared.iter())
            .map(|&node| json::Item::new(binary, node))
            .collect(),
        indirect_calls: parts.indirect.len(),
    };
    json::write(&answer, out)
}
