//! `elfq FILE callees SYMBOL`: what SYMBOL calls or uses. For ranges of
//! memory in place of SYMBOL, or SYMBOL with `--to`, see `ranges.rs`.

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::ranges::{self, Direction, RangeCheck};
use super::symbol::{self, Subject};
use super::{Kept, Outcome, Row, json, text};
use crate::binary::{AddressRange, Binary};
use crate::cli::Format;
use crate::graph::{Graph, Node, Unresolved};

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
    super::check_code_is_readable(binary)?;
    match symbol::subject(binary, symbols)? {
        Subject::Symbol(symbol) => {
            let index = super::readable_item(binary, symbol.node)?;
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
        let rows = super::rows(
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
        let callees = super::distinct_nodes(&answer.rows);
        writeln!(out, "{name} uses {}:", text::count_text(binary, &callees))?;
        writeln!(out)?;
        super::write_uses_table(binary, answer.rows, "CALLEE", text::shown(all), out)?;
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
    let rows = super::rows(
        graph.references_from(index),
        |reference| reference.to,
        |reference| text::how_column(binary, reference.to, reference.kind),
    );
    let unresolved = graph.unresolved_in(index);
    let notes = if rows.is_empty() && unresolved.is_empty() {
        vec![format!("{name} doesn't call or use anything.")]
    } else {
        unresolved_lines(name, unresolved, !rows.is_empty(), all)
    };
    Answer { rows, notes }
}

/// Sentences about the calls and jumps in `name` that elfq can't follow,
/// with the first few sites, or with `all`, every one. `listed` is whether
/// the table above lists what it does call or use.
pub(super) fn unresolved_lines(
    name: &str,
    unresolved: &[Unresolved],
    listed: bool,
    all: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    // Through a pointer, so that only the running program knows where
    // they go, or to an address the code names but no item holds.
    for (known_target, end) in [
        (false, "which elfq can't follow"),
        (true, "to code outside any function"),
    ] {
        let these: Vec<&Unresolved> = unresolved
            .iter()
            .filter(|u| u.target.is_some() == known_target)
            .collect();
        if these.is_empty() {
            continue;
        }
        let what = text::calls_and_jumps(these.iter().copied());
        let through = match (known_target, these.len()) {
            (true, _) => "",
            (false, 1) => " through a pointer",
            (false, _) => " through pointers",
        };
        let sites: Vec<u64> = these.iter().map(|u| u.site).collect();
        let start = if listed || !lines.is_empty() {
            "Also".to_string()
        } else {
            format!("{name} makes")
        };
        lines.push(format!(
            "{start} {what}{through}, at {}, {end}.",
            text::addresses_text(&sites, all)
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unresolved(site: u64, is_call: bool, target: Option<u64>) -> Unresolved {
        Unresolved {
            from: 0,
            site,
            is_call,
            target,
        }
    }

    #[test]
    fn what_elfq_cant_follow_is_said_in_one_line_each() {
        let calls = [
            unresolved(0x10, true, None),
            unresolved(0x20, false, Some(0x9000)),
        ];
        assert_eq!(
            unresolved_lines("f", &calls, false, false),
            [
                "f makes 1 call through a pointer, at 0x10, which elfq can't follow.",
                "Also 1 jump, at 0x20, to code outside any function.",
            ]
        );
        let jumps = [
            unresolved(0x10, false, Some(0x9000)),
            unresolved(0x18, false, Some(0x9100)),
        ];
        assert_eq!(
            unresolved_lines("f", &jumps, true, false),
            ["Also 2 jumps, at 0x10 and 0x18, to code outside any function."]
        );
    }
}
