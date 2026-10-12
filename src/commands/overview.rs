//! `elfq FILE`: what the file is, what elfq found in it, which items keep
//! the most bytes, and what to ask next.

use std::io::Write;
use std::path::Path;

use anyhow::Result;

use super::symbol::{self, Names};
use super::top;
use super::tree::RetainedTree;
use crate::arch;
use crate::binary::{Binary, FileKind, ItemKind, Source};
use crate::graph::{Graph, Node};

/// How many items the tree of the largest items shows.
const SHOWN: usize = 5;

pub fn run(path: &Path, binary: &Binary, out: &mut dyn Write) -> Result<()> {
    let names = match (binary.has_symbols, binary.has_dwarf) {
        (true, true) => "with symbols and DWARF debug info",
        (true, false) => "with symbols",
        (false, true) => "with DWARF debug info but no symbol table",
        (false, false) => "stripped",
    };
    writeln!(
        out,
        "{}: {} {}, {names}",
        path.display(),
        super::architecture_name(binary.architecture),
        binary.kind
    )?;
    writeln!(out)?;

    let count = |kind| binary.items.iter().filter(|i| i.kind == kind).count();
    let functions = super::plural(count(ItemKind::Function), "function", "functions");
    let data = super::plural(count(ItemKind::Data), "data object", "data objects");
    let mut found = match binary.imports.len() {
        0 => format!("{functions} and {data}"),
        n => format!(
            "{functions}, {data} and {}",
            super::plural(n, "import", "imports")
        ),
    };
    if !arch::is_supported(binary.architecture, binary.thumb) {
        writeln!(out, "{found}.")?;
        writeln!(out, "{}.", super::cannot_read_code(binary))?;
        return Ok(());
    }
    let graph = Graph::build(binary);
    found += &format!(
        ", with {} between them",
        super::plural(graph.references.len(), "reference", "references")
    );
    writeln!(out, "{found}.")?;
    let has_function_lists = binary.items.iter().any(|i| {
        matches!(
            i.source,
            Source::SymbolTable | Source::DynamicSymbolTable | Source::EhFrame
        )
    });
    if !has_function_lists {
        writeln!(
            out,
            "The file has no symbols or unwind table, so elfq can't find its functions yet."
        )?;
    }
    // Unnamed data is common even with symbols, such as string literals, so
    // explain its names only in files that have unnamed functions too.
    let unnamed = |source| binary.items.iter().find(|i| i.source == source);
    match (unnamed(Source::EhFrame), unnamed(Source::Reference)) {
        (Some(function), Some(data)) => writeln!(
            out,
            "Functions and data without symbols are named by address, like {} and {}.",
            function.name, data.name
        )?,
        (Some(function), None) => writeln!(
            out,
            "Functions without symbols are named by address, like {}.",
            function.name
        )?,
        _ => {}
    }
    let indirect = graph
        .unresolved
        .iter()
        .filter(|u| u.target.is_none())
        .count();
    if indirect > 0 {
        writeln!(
            out,
            "{} through pointers set at run time, which elfq can't follow.",
            super::plural(indirect, "call or jump goes", "calls or jumps go")
        )?;
    }

    // `retained` and `why` need roots, which a shared library doesn't have
    // yet.
    let queries = Names::new(binary);
    let about = |command: &str, item: usize| {
        let symbol = queries.query_for(Node::Item(item));
        format!("{command} {}", super::shell_quote(&symbol))
    };
    if binary.kind == FileKind::SharedLibrary {
        let examples: Vec<String> = [
            most_used(binary, &graph).map(|item| about("callers", item)),
            starting_point(binary).map(|item| about("callees", item)),
        ]
        .into_iter()
        .flatten()
        .collect();
        return write_try(path, &examples, out);
    }
    let retained = RetainedTree::from_graph(binary, graph);
    // Without functions, as in stripped firmware, there is nothing to rank.
    let kept: u64 = retained
        .tree
        .top()
        .iter()
        .map(|&node| retained.tree.retained(node))
        .sum();
    if kept > 0 {
        writeln!(out)?;
        let shown = top::write_largest(&retained, SHOWN, out)?;
        retained.write_pointer_note(&shown, out)?;
    }
    let examples: Vec<String> = [
        (kept > 0).then(|| "top".to_string()),
        starting_point(binary).map(|item| about("retained", item)),
        largest_kept(&retained).map(|item| about("why", item)),
    ]
    .into_iter()
    .flatten()
    .collect();
    write_try(path, &examples, out)
}

/// Writes the commands to try next, each given without `elfq FILE`.
fn write_try(path: &Path, commands: &[String], out: &mut dyn Write) -> Result<()> {
    if commands.is_empty() {
        return Ok(());
    }
    let file = super::shell_quote(&path.display().to_string());
    writeln!(out)?;
    writeln!(out, "Try:")?;
    for command in commands {
        writeln!(out, "  elfq {file} {command}")?;
    }
    Ok(())
}

/// The largest item that something keeps in the file, as opposed to one
/// where the program starts: a good question for `why`.
fn largest_kept(retained: &RetainedTree) -> Option<usize> {
    let binary = retained.binary;
    let is_root = |index| {
        retained
            .graph
            .roots
            .iter()
            .any(|root| root.node == Node::Item(index))
    };
    (0..binary.items.len())
        .filter(|&index| retained.tree.is_reachable(Node::Item(index)) && !is_root(index))
        .max_by_key(|&index| (binary.items[index].size, std::cmp::Reverse(index)))
}

/// The function with the most distinct callers, leaving out PLT stubs,
/// whose code is a library's.
fn most_used(binary: &Binary, graph: &Graph) -> Option<usize> {
    let mut callers: Vec<(usize, usize)> = graph
        .references
        .iter()
        .filter_map(|reference| match reference.to {
            Node::Item(to) if is_own_function(binary, to) => Some((to, reference.from)),
            _ => None,
        })
        .collect();
    callers.sort_unstable();
    callers.dedup();
    let mut best: Option<(usize, usize)> = None;
    for chunk in callers.chunk_by(|a, b| a.0 == b.0) {
        if best.is_none_or(|(_, count)| chunk.len() > count) {
            best = Some((chunk[0].0, chunk.len()));
        }
    }
    best.map(|(index, _)| index)
}

/// `main` if there is one, or else the function where the program starts,
/// such as firmware's reset handler, since every reader knows where those
/// fit; otherwise the largest function.
fn starting_point(binary: &Binary) -> Option<usize> {
    // `main` as SYMBOL would find it, so also `app::main` in Rust.
    let main = match symbol::find(binary, "main") {
        Ok(Node::Item(index)) => Some(index),
        _ => None,
    };
    let entry = binary.item_at(binary.entry);
    let largest = (0..binary.items.len())
        .filter(|&index| is_own_function(binary, index))
        .max_by_key(|&index| binary.items[index].size);
    [main, entry]
        .into_iter()
        .flatten()
        .find(|&index| is_own_function(binary, index))
        .or(largest)
}

/// A function whose code is in this file and readable: not a PLT stub,
/// whose code is a library's, nor a function of unknown size.
fn is_own_function(binary: &Binary, index: usize) -> bool {
    let item = &binary.items[index];
    item.kind == ItemKind::Function && item.source != Source::Plt && item.size > 0
}
