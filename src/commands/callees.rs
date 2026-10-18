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
use crate::render::{Align, Table};

#[derive(Serialize)]
struct Callees<'a> {
    symbol: json::Item<'a>,
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
/// `callees RANGE...`.
pub fn run(
    binary: &Binary,
    symbols: &[String],
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    super::check_code_is_readable(binary)?;
    match symbol::subject(binary, symbols)? {
        Subject::Node(node) => {
            let index = super::readable_item(binary, node)?;
            let graph = Graph::build(binary);
            callees(binary, &graph, index, format, out)?;
            Ok(Outcome::Answered)
        }
        Subject::Ranges(ranges) => {
            let graph = Graph::build(binary);
            ranges::run(binary, &graph, &ranges, Direction::Out, format, out)
        }
    }
}

/// What the item at `index` calls or uses.
fn callees(
    binary: &Binary,
    graph: &Graph,
    index: usize,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    let node = Node::Item(index);
    let name = node.name(binary);

    // The text and JSON word some uses differently ("uses" for both
    // "access" and "address taken" on data), so each gets its own rows.
    let rows = |how: &dyn Fn(&Reference) -> &'static str| {
        super::rows(graph.references_from(index), |reference| reference.to, how)
    };
    let unresolved = graph.unresolved_in(index);
    if format == Format::Json {
        let callees = Callees {
            symbol: json::Item::new(binary, node),
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

    if !rows.is_empty() {
        let callees = super::distinct_nodes(&rows);
        writeln!(out, "{name} uses {}:", text::count_text(binary, &callees))?;
        writeln!(out)?;
        let mut table = Table::new(&[
            ("CALLEE", Align::Left),
            ("HOW", Align::Left),
            ("AT", Align::Left),
        ]);
        for row in &rows {
            table.row(vec![
                row.node.name(binary).to_string(),
                row.how.to_string(),
                text::at_column(&row.sites),
            ]);
        }
        table.write(out)?;
    }
    if !unresolved.is_empty() {
        if !rows.is_empty() {
            writeln!(out)?;
        }
        for line in unresolved_lines(name, unresolved, !rows.is_empty()) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

/// Sentences about the calls and jumps in `name` that elfq can't follow.
/// `listed` is whether the table above lists what it does call or use.
fn unresolved_lines(name: &str, unresolved: &[Unresolved], listed: bool) -> Vec<String> {
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
            text::addresses_text(&sites)
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
            unresolved_lines("f", &calls, false),
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
            unresolved_lines("f", &jumps, true),
            ["Also 2 jumps, at 0x10 and 0x18, to code outside every function elfq knows."]
        );
    }
}
