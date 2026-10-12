//! `elfq FILE callers SYMBOL`: what calls or uses SYMBOL.

use std::io::Write;

use anyhow::Result;

use crate::binary::Binary;
use crate::graph::Node;
use crate::render::{Align, Table};

pub fn run(binary: &Binary, symbol: &str, out: &mut dyn Write) -> Result<()> {
    let graph = super::reference_graph(binary)?;
    let node = super::symbol::find(binary, symbol)?;
    let name = node.name(binary);

    let mut rows = super::rows(graph.references_to(node), |reference| {
        Node::Item(reference.from)
    });
    rows.sort_by(|a, b| a.node.name(binary).cmp(b.node.name(binary)));
    if rows.is_empty() {
        let entry = binary.item_at(binary.entry).map(Node::Item);
        if entry == Some(node) {
            writeln!(
                out,
                "Nothing in the file calls or uses {name}: it is the entry point, where the \
                 program starts."
            )?;
        } else {
            writeln!(out, "Nothing in the file calls or uses {name}.")?;
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
    Ok(())
}
