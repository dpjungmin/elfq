//! `elfq FILE callees SYMBOL`: what SYMBOL calls or uses. For ranges of
//! memory in place of SYMBOL, or SYMBOL with `--to`, see `ranges.rs`.

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::checks::{check_code_is_readable, readable_item};
use super::kept::Kept;
use super::ranges::{self, Direction, RangeCheck};
use super::symbol::{self, Subject};
use super::uses::{self, Row};
use super::{Outcome, json, text};
use crate::binary::{AddressRange, Binary};
use crate::cli::Format;
use crate::graph::{Graph, Node};

#[derive(Serialize)]
struct Callees<'a> {
    symbol: json::Symbol<'a>,
    callees: Vec<json::Use<'a>>,
    /// The calls and jumps that elfq can't follow.
    unresolved: Vec<UnresolvedJson>,
    /// Whether the list has everything SYMBOL calls, as far as elfq can
    /// tell: `false` when `unresolved` isn't empty.
    complete: bool,
}

#[derive(Serialize)]
struct UnresolvedJson {
    /// The instruction's address.
    site: String,
    /// `call` or `jump`.
    how: &'static str,
    /// Where it goes, when the code says so but no item is there, or
    /// `null` when only the running program knows.
    target: Option<String>,
}

/// Answers `elfq FILE callees SYMBOL`, or with ranges in place of SYMBOL,
/// `callees RANGE...`. `to`, the ranges of `--to`, limits either to the
/// callees in them, and then SYMBOL is asked about as the range it
/// occupies. For SYMBOL alone, the text lists the 30 largest callees, or
/// with `all`, every one; for ranges, see [`ranges::run`].
pub fn run(
    binary: &Binary,
    symbols: &[String],
    to: &[AddressRange],
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    check_code_is_readable(binary)?;
    match symbol::subject(binary, symbols)? {
        Subject::Symbol(symbol) => {
            let index = readable_item(binary, symbol.node)?;
            let graph = Kept::new(Graph::build(binary));
            let lead_name = symbol.lead_name(binary);
            if to.is_empty() {
                callees(binary, &graph, index, &lead_name, all, format, out)?;
                return Ok(Outcome::Answered);
            }
            let check = RangeCheck {
                ranges: &[ranges::item_range(binary, index)],
                name: Some(lead_name),
                other: to,
                direction: Direction::Out,
            };
            ranges::run(binary, &graph, &check, all, format, out)
        }
        Subject::Ranges(ranges) => {
            let check = RangeCheck {
                ranges: &ranges,
                name: None,
                other: to,
                direction: Direction::Out,
            };
            let graph = Kept::new(Graph::build(binary));
            ranges::run(binary, &graph, &check, all, format, out)
        }
    }
}

/// What the item at `index` calls or uses. `name` is its name as the
/// answer's first line gives it (see [`Symbol::lead_name`]).
///
/// [`Symbol::lead_name`]: symbol::Symbol::lead_name
pub(super) fn callees(
    binary: &Binary,
    graph: &Graph,
    index: usize,
    name: &str,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    let node = Node::Item(index);
    if format == Format::Json {
        // The JSON words some uses differently from the text ("uses" for
        // both "access" and "address taken" on data).
        let rows = uses::rows(
            graph.references_from(index),
            |reference| reference.to,
            |reference| json::how_field(reference.kind),
        );
        let unresolved = graph.unresolved_in(index);
        let callees = Callees {
            symbol: json::Symbol::new(binary, node),
            callees: json::uses(binary, &rows),
            unresolved: unresolved
                .iter()
                .map(|u| UnresolvedJson {
                    site: json::address(u.site),
                    how: text::call_or_jump(u),
                    target: u.target.map(json::address),
                })
                .collect(),
            complete: unresolved.is_empty(),
        };
        return json::write(&callees, out);
    }
    let answer = answer(binary, graph, index, name, all);
    if !answer.rows.is_empty() {
        let callees = uses::distinct_nodes(&answer.rows);
        writeln!(out, "{name} uses {}:", text::count_text(binary, &callees))?;
        writeln!(out)?;
        uses::write_uses_table(binary, answer.rows, "CALLEE", text::shown(all), out)?;
        if !answer.notes.is_empty() {
            writeln!(out)?;
        }
    }
    for line in answer.notes {
        writeln!(out, "{line}")?;
    }
    Ok(())
}

/// What `callees SYMBOL` lists about an item, as its text says it, but for
/// its first line.
pub(super) struct Answer {
    /// Each callee's uses, with HOW in the text's words, every one.
    pub rows: Vec<Row>,
    /// What the text says under the table, or in its place when there are
    /// no rows: the calls and jumps that elfq can't follow, or that the
    /// item calls or uses nothing.
    pub notes: Vec<String>,
}

/// What the item at `index`, `name` as the answer's first line gives it,
/// calls or uses, as the text of `callees SYMBOL` lists it, with the first
/// few sites of what elfq can't follow, or with `all`, every one.
pub(super) fn answer(
    binary: &Binary,
    graph: &Graph,
    index: usize,
    name: &str,
    all: bool,
) -> Answer {
    let rows = uses::rows(
        graph.references_from(index),
        |reference| reference.to,
        |reference| text::how_column(binary, reference.to, reference.kind),
    );
    let unresolved = graph.unresolved_in(index);
    let notes = if rows.is_empty() && unresolved.is_empty() {
        vec![format!("{name} doesn't call or use anything.")]
    } else {
        text::unresolved_lines(name, unresolved, !rows.is_empty(), all)
    };
    Answer { rows, notes }
}
