//! `elfq FILE callees SYMBOL`: what SYMBOL calls or uses. For ranges of
//! memory in place of SYMBOL, see `ranges.rs`.

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::ranges::{self, Direction};
use super::symbol::{self, Subject};
use super::{Outcome, json, text};
use crate::binary::Binary;
use crate::cli::Format;
use crate::graph::{Graph, Node, Reference, Unresolved};

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
/// `callees RANGE...`. For SYMBOL, the text lists the 30 largest callees,
/// or with `all`, every one; for ranges, see [`ranges::run`].
pub fn run(
    binary: &Binary,
    symbols: &[String],
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    super::check_code_is_readable(binary)?;
    match symbol::subject(binary, symbols)? {
        Subject::Symbol(symbol) => {
            let index = super::readable_item(binary, symbol.node)?;
            let graph = Graph::build(binary);
            let lead_name = symbol.lead_name(binary);
            callees(binary, &graph, index, &lead_name, all, format, out)?;
            Ok(Outcome::Answered)
        }
        Subject::Ranges(ranges) => {
            let graph = Graph::build(binary);
            ranges::run(binary, &graph, &ranges, Direction::Out, all, format, out)
        }
    }
}

/// What the item at `index` calls or uses. `name` is its name as the
/// answer's first line gives it (see [`Symbol::lead_name`]).
///
/// [`Symbol::lead_name`]: symbol::Symbol::lead_name
fn callees(
    binary: &Binary,
    graph: &Graph,
    index: usize,
    name: &str,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    let node = Node::Item(index);

    // The text and JSON word some uses differently ("uses" for both
    // "access" and "address taken" on data), so each gets its own rows.
    let rows = |how: &dyn Fn(&Reference) -> &'static str| {
        super::rows(graph.references_from(index), |reference| reference.to, how)
    };
    let unresolved = graph.unresolved_in(index);
    if format == Format::Json {
        let callees = Callees {
            symbol: json::Symbol::new(binary, node),
            callees: json::uses(binary, &rows(&|reference| json::how_field(reference.kind))),
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
    let rows = rows(&|reference| text::how_column(binary, reference.to, reference.kind));
    if rows.is_empty() && unresolved.is_empty() {
        writeln!(out, "{name} doesn't call or use anything.")?;
        return Ok(());
    }

    let listed = !rows.is_empty();
    if listed {
        let callees = super::distinct_nodes(&rows);
        writeln!(out, "{name} uses {}:", text::count_text(binary, &callees))?;
        writeln!(out)?;
        super::write_uses_table(binary, rows, "CALLEE", text::shown(all), out)?;
    }
    if !unresolved.is_empty() {
        if listed {
            writeln!(out)?;
        }
        for line in unresolved_lines(name, unresolved, listed, all) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

/// Sentences about the calls and jumps in `name` that elfq can't follow,
/// with the first few sites, or with `all`, every one. `listed` is whether
/// the table above lists what it does call or use.
fn unresolved_lines(name: &str, unresolved: &[Unresolved], listed: bool, all: bool) -> Vec<String> {
    let mut lines = Vec::new();
    // Through a pointer, so that only the running program knows where
    // they go, or to an address the code names but no item holds.
    for (known_target, end) in [
        (false, "which elfq can't follow"),
        (true, "to code outside every function elfq knows"),
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
                "Also 1 jump, at 0x20, to code outside every function elfq knows.",
            ]
        );
        let jumps = [
            unresolved(0x10, false, Some(0x9000)),
            unresolved(0x18, false, Some(0x9100)),
        ];
        assert_eq!(
            unresolved_lines("f", &jumps, true, false),
            ["Also 2 jumps, at 0x10 and 0x18, to code outside every function elfq knows."]
        );
    }
}
