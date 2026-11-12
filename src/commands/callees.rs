//! `elfq FILE callees SYMBOL`: what SYMBOL calls or uses. For ranges of
//! memory in place of SYMBOL, patterns, several SYMBOLs, or SYMBOL with
//! `--to`, see `range_check/`.

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::checks::{check_code_is_readable, item_in_file, readable_item};
use super::flat_answer::FlatAnswer;
use super::kept::Kept;
use super::range_check::{self, Direction, Inside, RangeCheck};
use super::symbol::{self, Subject};
use super::text::Detail;
use super::uses::{self, End};
use super::{Outcome, json, text};
use crate::binary::{AddressRange, Binary};
use crate::cli::Format;
use crate::graph::{Graph, Node};

#[derive(Serialize)]
struct Callees<'a> {
    symbol: json::Symbol<'a>,
    callees: Vec<json::Use<'a>>,
    /// The calls and jumps that elfq can't follow.
    unresolved: Vec<json::Unresolved>,
    /// Whether the list has everything SYMBOL calls, as far as elfq can
    /// tell: `false` when `unresolved` isn't empty.
    complete: bool,
}

/// Answers `elfq FILE callees SYMBOL`, or with ranges, patterns or several
/// SYMBOLs, `callees SYMBOL|PATTERN|RANGE...` (see [`symbol::subject`]).
/// `to`, the ranges of `--to`, limits either to the callees in them, and
/// then SYMBOL is asked about as the range it occupies. For SYMBOL alone,
/// the text lists the 30 largest callees, or with `all`, every one; for
/// the rest, see [`range_check::run`]. An import among several SYMBOLs
/// gets the error it gets alone, since its code isn't in the file.
pub(super) fn run(
    binary: &Binary,
    symbols: &[String],
    to: &[AddressRange],
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    check_code_is_readable(binary)?;
    let inside = match symbol::subject(binary, symbols, item_in_file)? {
        Subject::Symbol(symbol) => {
            let index = readable_item(binary, symbol.node)?;
            let lead_name = symbol.lead_name(binary);
            if to.is_empty() {
                let graph = Kept::new(Graph::build(binary));
                callees(binary, &graph, index, &lead_name, all, format, out)?;
                return Ok(Outcome::Answered);
            }
            Inside::Symbol {
                range: binary.items[index].range(),
                lead_name,
            }
        }
        Subject::Ranges(ranges) => Inside::Ranges(ranges),
        Subject::Set(set) => Inside::Set(set),
    };
    let check = RangeCheck {
        inside,
        far_side: to,
        direction: Direction::Out,
    };
    let graph = Kept::new(Graph::build(binary));
    range_check::run(binary, &graph, &check, all, format, out)
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
            unresolved: unresolved.iter().map(json::Unresolved::new).collect(),
            complete: unresolved.is_empty(),
        };
        return json::write(&callees, out);
    }
    answer(binary, graph, index, name, all).write(out)
}

/// What the item at `index`, `name` as the answer's first line gives it,
/// calls or uses, as the text of `callees SYMBOL` lists it: the rows of
/// the 30 largest callees and the first few sites of what elfq can't
/// follow, or with `all`, every one.
pub(super) fn answer(
    binary: &Binary,
    graph: &Graph,
    index: usize,
    name: &str,
    all: bool,
) -> FlatAnswer {
    let rows = uses::rows(
        graph.references_from(index),
        |reference| reference.to,
        |reference| text::how_column(binary, reference.to, reference.kind),
    );
    let unresolved = graph.unresolved_in(index);
    // With no rows, what elfq can't follow, or that the item uses nothing,
    // is said in place of the table.
    let (lead, notes) = match (rows.is_empty(), unresolved.is_empty()) {
        (true, true) => (
            vec![format!("{name} doesn't call or use anything.")],
            Vec::new(),
        ),
        (true, false) => (
            text::unresolved_lines(name, unresolved, false, Detail::from_flag(all)),
            Vec::new(),
        ),
        (false, _) => {
            let callees = text::count_text(binary, &uses::distinct_nodes(&rows));
            let lead = format!("{name} uses {callees}:");
            (
                vec![lead],
                text::unresolved_lines(name, unresolved, true, Detail::from_flag(all)),
            )
        }
    };
    let (columns, rows) = uses::table(
        binary,
        rows,
        End::Callees,
        text::shown(Detail::from_flag(all)),
    );
    FlatAnswer {
        lead,
        memory: None,
        columns,
        rows,
        tree: None,
        notes,
    }
}
