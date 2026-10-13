//! `elfq FILE callers SYMBOL`: what calls or uses SYMBOL.

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::json;
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

pub fn run(binary: &Binary, symbol: &str, format: Format, out: &mut dyn Write) -> Result<()> {
    let graph = super::reference_graph(binary)?;
    let node = super::symbol::find(binary, symbol)?;
    let name = node.name(binary);

    // The callers by name, with `how` saying how each uses SYMBOL. The text
    // and JSON word some uses differently ("uses" for both "access" and
    // "address taken" on data), so each gets its own rows.
    let rows = |how: &dyn Fn(&Reference) -> &'static str| {
        let mut rows = super::rows(
            graph.references_to(node),
            |reference| Node::Item(reference.from),
            how,
        );
        rows.sort_by(|a, b| a.node.name(binary).cmp(b.node.name(binary)));
        rows
    };
    let through_pointer = can_be_called_through_pointer(binary, &graph, node);
    if format == Format::Json {
        let callers = Callers {
            symbol: json::Item::new(binary, node),
            callers: json::uses(binary, &rows(&|reference| json::how(reference.kind))),
            complete: !through_pointer,
        };
        return json::write(&callers, out);
    }
    let rows = rows(&|reference| super::how(binary, reference));
    // Some calls can't be listed: the system's, to where the program starts,
    // and those through a pointer.
    let root = graph.roots.iter().find(|root| root.node == node);
    let note = match root {
        Some(root) => Some(super::why::root_sentence(binary, root)),
        None if through_pointer => Some(format!(
            "Calls to {name} through a pointer aren't listed, since elfq can't see them."
        )),
        None => None,
    };
    if rows.is_empty() {
        writeln!(out, "Nothing in the file calls or uses {name}.")?;
        if let Some(note) = note {
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
    if let Some(note) = note {
        writeln!(out)?;
        writeln!(out, "{note}")?;
    }
    Ok(())
}

/// Whether `node` is a function whose address is taken or stored, so that
/// code, a library or the processor can call it through a pointer.
fn can_be_called_through_pointer(binary: &Binary, graph: &Graph, node: Node) -> bool {
    let is_function = match node {
        Node::Item(index) => binary.items[index].kind == ItemKind::Function,
        Node::Import(_) => false,
    };
    is_function
        && graph
            .references_to(node)
            .any(|r| matches!(r.kind, ReferenceKind::Address | ReferenceKind::Pointer))
}
