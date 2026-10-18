//! `elfq FILE callers SYMBOL`: what calls or uses SYMBOL. For ranges of
//! memory in place of SYMBOL, see `ranges.rs`.

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::ranges::{self, Direction};
use super::symbol::{self, Subject};
use super::{Outcome, json};
use crate::binary::{Binary, ItemKind};
use crate::cli::Format;
use crate::graph::{Graph, Node, Reference, ReferenceKind};
use crate::render::{Align, Table};

#[derive(Serialize)]
struct Callers<'a> {
    symbol: json::Item<'a>,
    callers: Vec<json::Use<'a>>,
    /// Whether the list has every caller, as far as elfq can tell: `false`
    /// when SYMBOL is a function whose address is taken or stored, so that
    /// calls through a pointer to it don't show.
    complete: bool,
}

pub fn run(
    binary: &Binary,
    symbols: &[String],
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    super::check_code_is_readable(binary)?;
    let subject = symbol::subject(binary, symbols)?;
    let graph = Graph::build(binary);
    match subject {
        Subject::Node(node) => {
            callers(binary, &graph, node, format, out)?;
            Ok(Outcome::Answered)
        }
        Subject::Ranges(ranges) => ranges::run(binary, &graph, &ranges, Direction::In, format, out),
    }
}

/// What calls or uses `node`.
fn callers(
    binary: &Binary,
    graph: &Graph,
    node: Node,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    let name = node.name(binary);

    // Code can call the file's own functions through PLT stubs, such as
    // `log_msg@plt` in a shared library, which pass each call on to the
    // function. Calls to the stubs count as calls to the function, and the
    // stubs aren't callers.
    let stubs = stubs_of(binary, node);
    let references: Vec<&Reference> = graph
        .references
        .iter()
        .filter(|r| {
            let to_stub = stubs.contains(&r.to);
            let from_stub = stubs.contains(&Node::Item(r.from));
            to_stub || (r.to == node && !from_stub)
        })
        .collect();

    // The callers by name, with `how` saying how each uses SYMBOL. The text
    // and JSON word some uses differently ("uses" for both "access" and
    // "address taken" on data), so each gets its own rows.
    let rows = |how: &dyn Fn(&Reference) -> &'static str| {
        let mut rows = super::rows(
            references.iter().copied(),
            |reference| Node::Item(reference.from),
            how,
        );
        rows.sort_by(|a, b| a.node.name(binary).cmp(b.node.name(binary)));
        rows
    };
    let through_pointer = can_be_called_through_pointer(binary, node, &references);
    if format == Format::Json {
        let callers = Callers {
            symbol: json::Item::new(binary, node),
            callers: json::uses(binary, &rows(&|reference| json::how(reference.kind))),
            complete: !through_pointer,
        };
        return json::write(&callers, out);
    }
    let rows = rows(&|reference| super::how(binary, reference.to, reference.kind));
    let mut notes = Vec::new();
    // A function's stubs all have its name, as `log_msg@plt`: glibc's
    // `libc.so.6` has two for `wmemset`. Code that only takes a stub's
    // address, as for an ifunc, takes the function's, so that needs no note.
    let called_stub = stubs.iter().find(|&&stub| {
        references.iter().any(|r| {
            r.to == stub && matches!(r.kind, ReferenceKind::Call | ReferenceKind::TailCall)
        })
    });
    if let Some(stub) = called_stub {
        notes.push(format!(
            "This includes calls through {}.",
            stub.name(binary)
        ));
    }
    // Some calls can't be listed: the system's, to where the program starts,
    // and those through a pointer.
    let root = graph.roots.iter().find(|root| root.node == node);
    match root {
        Some(root) => notes.push(super::why::root_sentence(binary, root)),
        None if through_pointer => notes.push(format!(
            "Calls to {name} through a pointer aren't listed, since elfq can't see them."
        )),
        None => {}
    }
    if rows.is_empty() {
        writeln!(out, "Nothing in the file calls or uses {name}.")?;
        for note in notes {
            writeln!(out, "{note}")?;
        }
        return Ok(());
    }

    let callers = super::distinct_nodes(&rows);
    writeln!(
        out,
        "{name} is used by {}:",
        super::count_text(binary, &callers)
    )?;
    writeln!(out)?;
    let mut table = Table::new(&[
        ("CALLER", Align::Left),
        ("HOW", Align::Left),
        ("AT", Align::Left),
    ]);
    for row in &rows {
        table.row(vec![
            row.node.name(binary).to_string(),
            row.how.to_string(),
            super::addresses(&row.sites),
        ]);
    }
    table.write(out)?;
    if !notes.is_empty() {
        writeln!(out)?;
        for note in notes {
            writeln!(out, "{note}")?;
        }
    }
    Ok(())
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
