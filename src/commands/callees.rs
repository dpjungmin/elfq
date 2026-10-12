//! `elfq FILE callees SYMBOL`: what SYMBOL calls or uses.

use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::json;
use crate::binary::{Binary, ItemKind};
use crate::cli::Format;
use crate::graph::{Node, Unresolved};
use crate::render::{Align, Table};

#[derive(Serialize)]
struct Callees<'a> {
    symbol: json::Item<'a>,
    callees: Vec<json::Use<'a>>,
    /// The calls and jumps that elfq can't follow.
    unresolved: Vec<UnresolvedJson>,
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

pub fn run(binary: &Binary, symbol: &str, format: Format, out: &mut dyn Write) -> Result<()> {
    let graph = super::reference_graph(binary)?;
    let node = super::symbol::find(binary, symbol)?;
    let name = node.name(binary);
    let Node::Item(index) = node else {
        bail!("{name} is imported from a shared library, so its code isn't in this file");
    };
    let item = &binary.items[index];
    if item.kind == ItemKind::Function && item.size == 0 {
        bail!("elfq doesn't know where {name} ends, so it can't read its code");
    }

    let rows = super::rows(graph.references_from(index), |reference| reference.to);
    let unresolved = graph.unresolved_in(index);
    if format == Format::Json {
        let callees = Callees {
            symbol: json::Item::new(binary, node),
            callees: json::uses(binary, &rows),
            unresolved: unresolved
                .iter()
                .map(|u| UnresolvedJson {
                    site: json::address(u.site),
                    how: if u.is_call { "call" } else { "jump" },
                    target: u.target.map(json::address),
                })
                .collect(),
        };
        return json::write(&callees, out);
    }
    if rows.is_empty() && unresolved.is_empty() {
        writeln!(out, "{name} doesn't call or use anything.")?;
        return Ok(());
    }

    if !rows.is_empty() {
        let callees = super::distinct_nodes(&rows);
        writeln!(out, "{name} uses {}:", super::count_text(binary, &callees))?;
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
                super::addresses(&row.sites),
            ]);
        }
        table.write(out)?;
    }
    if !unresolved.is_empty() {
        if !rows.is_empty() {
            writeln!(out)?;
        }
        for line in unresolved_lines(unresolved) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

/// Sentences about the calls and jumps that elfq can't follow.
fn unresolved_lines(unresolved: &[Unresolved]) -> Vec<String> {
    let sites = |is_call: bool, known_target: bool| -> Vec<u64> {
        unresolved
            .iter()
            .filter(|u| u.is_call == is_call && u.target.is_some() == known_target)
            .map(|u| u.site)
            .collect()
    };
    let mut lines = Vec::new();
    for (is_call, one, many) in [
        (true, "indirect call", "indirect calls"),
        (false, "indirect jump", "indirect jumps"),
    ] {
        let at = sites(is_call, false);
        if !at.is_empty() {
            lines.push(format!(
                "{} at {} can go anywhere: the target is only known at run time.",
                capitalize(&super::plural(at.len(), one, many)),
                super::addresses(&at)
            ));
        }
    }
    for (is_call, one, many) in [(true, "call", "calls"), (false, "jump", "jumps")] {
        let at = sites(is_call, true);
        if !at.is_empty() {
            lines.push(format!(
                "{} at {} {} to code outside every function elfq knows.",
                capitalize(&super::plural(at.len(), one, many)),
                super::addresses(&at),
                if at.len() == 1 { "goes" } else { "go" }
            ));
        }
    }
    lines
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
