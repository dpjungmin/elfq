//! `elfq FILE callers SYMBOL`: what calls or uses SYMBOL. For ranges of
//! memory in place of SYMBOL, or SYMBOL with `--from`, see `ranges.rs`.

use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::ranges::{self, Direction, RangeCheck};
use super::symbol::{self, Subject, Symbol};
use super::{Kept, Outcome, Row, json, text};
use crate::binary::{AddressRange, Binary, ItemKind};
use crate::cli::Format;
use crate::graph::{Graph, Node, Reference, ReferenceKind};

#[derive(Serialize)]
struct Callers<'a> {
    symbol: json::Symbol<'a>,
    callers: Vec<json::Use<'a>>,
    /// Whether the list has every caller, as far as elfq can tell: `false`
    /// when SYMBOL is a function whose address is taken or stored, so that
    /// calls through a pointer to it don't show.
    complete: bool,
}

/// Answers `elfq FILE callers SYMBOL`, or with ranges in place of SYMBOL,
/// `callers RANGE...`. `from`, the ranges of `--from`, limits either to the
/// callers in them, and then SYMBOL is asked about as the range it
/// occupies. For SYMBOL alone, the text lists the 30 largest callers, or
/// with `all`, every one; for ranges, see [`ranges::run`].
pub fn run(
    binary: &Binary,
    symbols: &[String],
    from: &[AddressRange],
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    super::check_code_is_readable(binary)?;
    let subject = symbol::subject(binary, symbols)?;
    match subject {
        Subject::Symbol(symbol) if from.is_empty() => {
            let graph = Kept::new(Graph::build(binary));
            callers(binary, &graph, &symbol, all, format, out)?;
            Ok(Outcome::Answered)
        }
        Subject::Symbol(symbol) => {
            let Node::Item(index) = symbol.node else {
                bail!(
                    "{} is imported from a shared library, so it has no range in this file for \
                     --from",
                    symbol.node.name(binary)
                );
            };
            let range = ranges::item_range(binary, index);
            let check = RangeCheck {
                ranges: &[range],
                name: Some(symbol.lead_name(binary)),
                other: from,
                direction: Direction::In,
            };
            let graph = Kept::new(Graph::build(binary));
            ranges::run(binary, &graph, &check, all, format, out)
        }
        Subject::Ranges(ranges) => {
            let check = RangeCheck {
                ranges: &ranges,
                name: None,
                other: from,
                direction: Direction::In,
            };
            let graph = Kept::new(Graph::build(binary));
            ranges::run(binary, &graph, &check, all, format, out)
        }
    }
}

/// Writes what calls or uses SYMBOL, using `graph`, the file's reference
/// graph. `browse`'s tests run it on the graph that they build once for
/// every page of a file.
pub(super) fn callers(
    binary: &Binary,
    graph: &Graph,
    symbol: &Symbol,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    let node = symbol.node;
    if format == Format::Json {
        let references = references_with_stubs(graph, node, &stubs_of(binary, node));
        let callers = Callers {
            symbol: json::Symbol::new(binary, node),
            callers: json::uses(
                binary,
                &rows(binary, &references, |reference| {
                    json::how_field(reference.kind)
                }),
            ),
            complete: !can_be_called_through_pointer(binary, node, &references),
        };
        return json::write(&callers, out);
    }
    let Answer { rows, notes } = answer(binary, graph, node);
    let lead_name = symbol.lead_name(binary);
    if rows.is_empty() {
        writeln!(out, "Nothing in the file calls or uses {lead_name}.")?;
        for note in notes {
            writeln!(out, "{note}")?;
        }
        return Ok(());
    }

    let callers = super::distinct_nodes(&rows);
    writeln!(
        out,
        "{lead_name} is used by {}:",
        text::count_text(binary, &callers)
    )?;
    writeln!(out)?;
    super::write_uses_table(binary, rows, "CALLER", text::shown(all), out)?;
    if !notes.is_empty() {
        writeln!(out)?;
        for note in notes {
            writeln!(out, "{note}")?;
        }
    }
    Ok(())
}

/// The text answer of `callers SYMBOL` for `node`, before it is written:
/// the rows of its table and the notes under them. Every caller is in the
/// rows; the text leaves out all but the largest, unless with `--all`.
pub(super) struct Answer {
    /// Sorted by caller, with HOW in the text's words.
    pub rows: Vec<Row>,
    /// What the rows include or can't list, such as calls through the
    /// function's PLT stub or through a pointer.
    pub notes: Vec<String>,
}

/// What calls or uses `node`, as the text of `callers SYMBOL` lists it.
pub(super) fn answer(binary: &Binary, graph: &Graph, node: Node) -> Answer {
    let name = node.name(binary);
    let stubs = stubs_of(binary, node);
    let references = references_with_stubs(graph, node, &stubs);
    let rows = rows(binary, &references, |reference| {
        text::how_column(binary, reference.to, reference.kind)
    });
    let through_pointer = can_be_called_through_pointer(binary, node, &references);
    let mut notes = Vec::new();
    // A function's stubs all have its name, as `log_msg@plt`: glibc's
    // `libc.so.6` has two for `wmemset`. Code that only takes a stub's
    // address, as for an ifunc, takes the function's, so that needs no note.
    let called_stub = stubs
        .into_iter()
        .find(|&stub| references.iter().any(|r| r.to == stub && r.kind.is_call()));
    if let Some(stub) = called_stub {
        notes.push(format!(
            "This includes calls through {}.",
            stub.name(binary)
        ));
    }
    // Some calls can't be listed: the system's, to where the program starts,
    // and those through a pointer.
    match graph.root(node) {
        Some(root) => notes.push(text::root_sentence(binary, name, root.kind)),
        None if through_pointer => notes.push(format!(
            "Calls to {name} through a pointer aren't listed, since elfq can't see them."
        )),
        None => {}
    }
    Answer { rows, notes }
}

/// The references that `callers` lists for `node`, given its PLT `stubs`.
/// Code can call the file's own functions through PLT stubs, such as
/// `log_msg@plt` in a shared library, which pass each call on to the
/// function. Unlike in [`Graph::references_to`], calls to the stubs count
/// as calls to the function, and the stubs aren't callers.
fn references_with_stubs<'g>(graph: &'g Graph, node: Node, stubs: &[Node]) -> Vec<&'g Reference> {
    graph
        .references
        .iter()
        .filter(|r| {
            let to_stub = stubs.contains(&r.to);
            let from_stub = stubs.contains(&Node::Item(r.from));
            to_stub || (r.to == node && !from_stub)
        })
        .collect()
}

/// The callers in `references` by name, with `how` saying how each uses
/// SYMBOL. The text and JSON word some uses differently ("uses" for both
/// "access" and "address taken" on data), so each gets its own rows.
fn rows(
    binary: &Binary,
    references: &[&Reference],
    how: impl Fn(&Reference) -> &'static str,
) -> Vec<Row> {
    let mut rows = super::rows(
        references.iter().copied(),
        |reference| Node::Item(reference.from),
        how,
    );
    rows.sort_by(|a, b| a.node.name(binary).cmp(b.node.name(binary)));
    rows
}

/// The PLT stubs that pass calls on to `node`, when it is one of the file's
/// own functions (see [`Binary::own_function_stubs`]).
fn stubs_of(binary: &Binary, node: Node) -> Vec<Node> {
    let Node::Item(function) = node else {
        return Vec::new();
    };
    let mut stubs: Vec<Node> = binary
        .own_function_stubs()
        .into_iter()
        .filter(|&(_, to)| to == function)
        .map(|(stub, _)| Node::Item(stub))
        .collect();
    stubs.sort();
    stubs
}

/// Whether `node` is a function whose address is taken or stored, so that
/// code, a library or the processor can call it through a pointer, given
/// the references to it.
fn can_be_called_through_pointer(binary: &Binary, node: Node, references: &[&Reference]) -> bool {
    let is_function = match node {
        Node::Item(index) => binary.items[index].kind == ItemKind::Function,
        Node::Import(_) => false,
    };
    is_function
        && references
            .iter()
            .any(|r| matches!(r.kind, ReferenceKind::Address | ReferenceKind::Pointer))
}
