//! `elfq FILE`: what the file is, what elfq found in it, where in memory
//! its sections are, which items keep the most bytes, and what to ask next.
//! With `--range`, only which items keep the most bytes in the range.

use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};
use serde::Serialize;

use super::symbol::{self, Names};
use super::tree::RetainedTree;
use super::{json, top};
use crate::arch;
use crate::binary::{AddressRange, Binary, FileKind, ItemKind, MemoryBlock, Source, memory_blocks};
use crate::cli::Format;
use crate::graph::{Graph, Node};
use crate::render::{Align, Table};

/// How many items the tree of the largest items shows.
const SHOWN: usize = 5;

/// What the file is and what elfq found in it. The largest items are in
/// `top`'s JSON, and the commands to try next are only for people.
#[derive(Serialize)]
struct OverviewJson<'a> {
    file: String,
    /// As the text names it, such as `x86-64` or `32-bit Arm`.
    architecture: String,
    /// `executable`, `position-independent executable` or `shared library`.
    kind: String,
    has_symbols: bool,
    has_dwarf: bool,
    /// The blocks of addresses that the file's sections occupy, such as
    /// flash and RAM, in address order.
    memory: Vec<json::Block<'a>>,
    /// The range of memory that `--range` picked, or `null`. With a range,
    /// the counts and bytes below are of the items that have bytes in it,
    /// and the rest are `null`.
    range: Option<json::Range>,
    functions: usize,
    data_objects: usize,
    /// The bytes of all the functions and data objects.
    bytes: u64,
    /// `null` with a range, since imports take no memory in the file.
    imports: Option<usize>,
    /// How many references elfq found between the items, or `null` when it
    /// can't read the file's code, or with a range.
    references: Option<usize>,
    /// How many calls and jumps go through pointers set at run time, which
    /// elfq can't follow, or `null` when it can't read the file's code, or
    /// with a range.
    indirect_calls: Option<usize>,
}

pub fn run(
    path: &Path,
    binary: &Binary,
    range: Option<AddressRange>,
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    if format == Format::Json {
        return write_json(path, binary, range, out);
    }
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
    if let Some(range) = range {
        return write_range(path, binary, range, out);
    }

    let count = |kind| count(binary, kind);
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
    let indirect = indirect_calls(&graph);
    if indirect > 0 {
        writeln!(
            out,
            "{} through pointers set at run time, which elfq can't follow.",
            super::plural(indirect, "call or jump goes", "calls or jumps go")
        )?;
    }
    let blocks = write_memory(binary, out)?;
    let ranges: Vec<String> = blocks
        .iter()
        .map(|block| format!("--range {}", super::range_argument(block.range)))
        .collect();

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
            inlined(binary),
        ]
        .into_iter()
        .flatten()
        .collect();
        return write_try(path, &examples, out);
    }
    let retained = RetainedTree::from_graph(binary, graph, None);
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
    let examples: Vec<String> = ranges
        .into_iter()
        .chain(
            [
                (kept > 0).then(|| "top".to_string()),
                starting_point(binary).map(|item| about("retained", item)),
                largest_kept(&retained).map(|item| about("why", item)),
                inlined(binary),
            ]
            .into_iter()
            .flatten(),
        )
        .collect();
    write_try(path, &examples, out)
}

/// The overview of one range of memory: which items keep the most bytes
/// in it.
fn write_range(
    path: &Path,
    binary: &Binary,
    range: AddressRange,
    out: &mut dyn Write,
) -> Result<()> {
    if binary.kind == FileKind::SharedLibrary {
        bail!(
            "elfq doesn't know yet which functions a shared library offers to other files, so \
             it can't tell what keeps the bytes in a range of it"
        );
    }
    let retained = RetainedTree::build(binary, Some(range))?;
    top::check_range_has_bytes(&retained)?;
    let shown = top::write_largest(&retained, SHOWN, out)?;
    retained.write_pointer_note(&shown, out)?;
    let queries = Names::new(binary);
    let examples: Vec<String> = [
        Some(format!("top --range {}", super::range_argument(range))),
        largest_kept(&retained).map(|item| {
            let symbol = queries.query_for(Node::Item(item));
            format!("why {}", super::shell_quote(&symbol))
        }),
    ]
    .into_iter()
    .flatten()
    .collect();
    write_try(path, &examples, out)
}

/// Lists the blocks of addresses that the sections occupy, when there are
/// several, as in firmware with flash and RAM, and returns them.
fn write_memory(binary: &Binary, out: &mut dyn Write) -> Result<Vec<MemoryBlock>> {
    let blocks = memory_blocks(&binary.sections);
    if blocks.len() < 2 {
        return Ok(Vec::new());
    }
    let mut table = Table::new(&[
        ("MEMORY", Align::Left),
        ("SIZE", Align::Right),
        ("SECTIONS", Align::Left),
    ]);
    for block in &blocks {
        let sections: Vec<&str> = block
            .sections
            .iter()
            .map(|&index| binary.sections[index].name.as_str())
            .collect();
        table.row(vec![
            super::range_argument(block.range),
            (block.range.end - block.range.start).to_string(),
            sections.join(" "),
        ]);
    }
    writeln!(out)?;
    table.write(out)?;
    Ok(blocks)
}

fn write_json(
    path: &Path,
    binary: &Binary,
    range: Option<AddressRange>,
    out: &mut dyn Write,
) -> Result<()> {
    let readable = arch::is_supported(binary.architecture, binary.thumb);
    let graph = (readable && range.is_none()).then(|| Graph::build(binary));
    let in_range = |index: usize| {
        let item = &binary.items[index];
        range.map_or(item.size, |range| range.overlap(item.address, item.size))
    };
    let counted = |kind: ItemKind| {
        (0..binary.items.len())
            .filter(|&index| binary.items[index].kind == kind)
            .filter(|&index| range.is_none() || in_range(index) > 0)
            .count()
    };
    let answer = OverviewJson {
        file: path.display().to_string(),
        architecture: super::architecture_name(binary.architecture),
        kind: binary.kind.to_string(),
        has_symbols: binary.has_symbols,
        has_dwarf: binary.has_dwarf,
        memory: memory_blocks(&binary.sections)
            .iter()
            .map(|block| json::Block::new(binary, block))
            .collect(),
        range: range.map(json::Range::new),
        functions: counted(ItemKind::Function),
        data_objects: counted(ItemKind::Data),
        bytes: (0..binary.items.len()).map(in_range).sum(),
        imports: range.is_none().then_some(binary.imports.len()),
        references: graph.as_ref().map(|graph| graph.references.len()),
        indirect_calls: graph.as_ref().map(indirect_calls),
    };
    json::write(&answer, out)
}

/// How many items of `kind` the file has.
fn count(binary: &Binary, kind: ItemKind) -> usize {
    binary.items.iter().filter(|item| item.kind == kind).count()
}

/// How many calls and jumps go through pointers set at run time.
fn indirect_calls(graph: &Graph) -> usize {
    graph
        .unresolved
        .iter()
        .filter(|unresolved| unresolved.is_indirect())
        .count()
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

/// `inlined`, when the file has the debug info it needs.
fn inlined(binary: &Binary) -> Option<String> {
    binary.has_dwarf.then(|| "inlined".to_string())
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
        .filter(|&index| retained.own(index) > 0)
        .max_by_key(|&index| (retained.own(index), std::cmp::Reverse(index)))
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
