//! `elfq FILE`: what the file is, what elfq found in it, where in memory
//! its sections are, which items keep the most bytes, and what to ask next.
//! With `--range`, what elfq found in the range and which items keep the
//! most of its bytes.

use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};
use serde::Serialize;

use super::retained_tree::RetainedTree;
use super::symbol::Names;
use super::{json, text};
use crate::arch;
use crate::binary::{
    AddressRange, Binary, FileKind, Item, ItemKind, MemoryBlock, Source, memory_blocks,
};
use crate::cli::Format;
use crate::graph::{Graph, Node};
use crate::render::{Align, Table};

/// How many of the largest items the tree shows, as `top`'s tree does.
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
    if let Some(range) = range {
        return write_range(path, binary, range, out);
    }
    write_file(path, binary, out)?;

    let mut found = items_text(binary, None);
    // A stripped file may have only its entry point, of unknown size.
    let bytes = total_bytes(binary, None);
    if bytes > 0 {
        found += &format!(", {} of code and data", text::bytes_text(bytes));
    }
    if !binary.imports.is_empty() {
        found += &format!(
            ", plus {}",
            text::plural(binary.imports.len(), "import", "imports")
        );
    }
    writeln!(out, "{found}")?;
    if !arch::is_supported(binary.architecture, binary.thumb) {
        writeln!(out, "{}.", text::cannot_read_code(binary))?;
        return Ok(());
    }
    let graph = Graph::build(binary);
    if !super::found_functions(binary) {
        writeln!(
            out,
            "The file has no symbols or unwind table, so elfq can't find its functions yet."
        )?;
    }
    // Unnamed data is common even with symbols, such as string literals, so
    // explain its names only in files that have unnamed functions too.
    let unnamed = |sources: &[Source]| {
        binary
            .items
            .iter()
            .find(|item| sources.contains(&item.source))
    };
    match (
        unnamed(&[Source::EhFrame, Source::Reached]),
        unnamed(&[Source::Reference]),
    ) {
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
    let blocks = write_memory(binary, out)?;
    // Only ranges that hold something: in a stripped file whose functions
    // elfq can't find, the sections hold nothing it knows of.
    let ranges: Vec<String> = blocks
        .iter()
        .filter(|block| total_bytes(binary, Some(block.range)) > 0)
        .map(|block| format!("--range {}", text::range_argument(block.range)))
        .collect();

    // `retained` and `why` need roots, which a shared library doesn't have
    // yet.
    let queries = Names::new(binary);
    let about = |command: &str, item: usize| {
        let symbol = queries.query_for(Node::Item(item));
        format!("{command} {}", text::shell_quote(&symbol))
    };
    if binary.kind == FileKind::SharedLibrary {
        let examples: Vec<String> = [
            most_used(binary, &graph).map(|item| about("callers", item)),
            starting_point(binary, &queries).map(|item| about("callees", item)),
            inlined(binary),
        ]
        .into_iter()
        .flatten()
        .collect();
        return write_try(path, &examples, out);
    }
    let retained = RetainedTree::from_graph(binary, graph, None);
    // Without functions, as in a stripped file whose functions elfq can't
    // find, there is nothing to rank.
    let kept = retained.kept();
    if kept > 0 {
        writeln!(out)?;
        retained.write_largest(SHOWN, out)?;
    }
    let examples: Vec<String> = ranges
        .into_iter()
        .chain(
            [
                (kept > 0).then(|| "top".to_string()),
                starting_point(binary, &queries).map(|item| about("retained", item)),
                largest_kept(&retained).map(|item| about("why", item)),
                inlined(binary),
            ]
            .into_iter()
            .flatten(),
        )
        .collect();
    write_try(path, &examples, out)
}

/// The overview of one range of memory: what elfq found in it, and which
/// items keep the most of its bytes.
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
    retained.check_has_bytes()?;
    write_file(path, binary, out)?;
    writeln!(
        out,
        "{} in {}, {} of code and data",
        items_text(binary, Some(range)),
        text::range_text(binary, range),
        text::bytes_text(total_bytes(binary, Some(range)))
    )?;
    // When nothing reaches the items in the range, there is nothing to rank.
    if retained.kept() > 0 {
        writeln!(out)?;
        retained.write_largest(SHOWN, out)?;
    }
    let queries = Names::new(binary);
    let examples: Vec<String> = [
        Some(format!("top --range {}", text::range_argument(range))),
        largest_kept(&retained).map(|item| {
            let symbol = queries.query_for(Node::Item(item));
            format!("why {}", text::shell_quote(&symbol))
        }),
    ]
    .into_iter()
    .flatten()
    .collect();
    write_try(path, &examples, out)
}

/// Writes what the file is, such as "app.elf: x86-64 executable, stripped".
fn write_file(path: &Path, binary: &Binary, out: &mut dyn Write) -> Result<()> {
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
        text::architecture_name(binary.architecture),
        binary.kind
    )?;
    Ok(())
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
            text::range_argument(block.range),
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
    let answer = OverviewJson {
        file: path.display().to_string(),
        architecture: text::architecture_name(binary.architecture),
        kind: binary.kind.to_string(),
        has_symbols: binary.has_symbols,
        has_dwarf: binary.has_dwarf,
        memory: memory_blocks(&binary.sections)
            .iter()
            .map(|block| json::Block::new(binary, block))
            .collect(),
        range: range.map(json::Range::new),
        functions: count(binary, ItemKind::Function, range),
        data_objects: count(binary, ItemKind::Data, range),
        bytes: total_bytes(binary, range),
        imports: range.is_none().then_some(binary.imports.len()),
        references: graph.as_ref().map(|graph| graph.references.len()),
        indirect_calls: graph.as_ref().map(indirect_calls),
    };
    json::write(&answer, out)
}

/// The bytes of an item that are in `range`, or all of them without one.
fn in_range(item: &Item, range: Option<AddressRange>) -> u64 {
    range.map_or(item.size, |range| range.overlap(item.address, item.size))
}

/// How many items of `kind` the file has, or with a range, how many have
/// bytes in it.
fn count(binary: &Binary, kind: ItemKind, range: Option<AddressRange>) -> usize {
    binary
        .items
        .iter()
        .filter(|item| item.kind == kind)
        .filter(|item| range.is_none() || in_range(item, range) > 0)
        .count()
}

/// The bytes of all the items, or with a range, those in it.
fn total_bytes(binary: &Binary, range: Option<AddressRange>) -> u64 {
    super::total(binary.items.iter().map(|item| in_range(item, range)))
}

/// What elfq found, such as "25 functions and 31 data objects", leaving
/// out a count of none unless both are.
fn items_text(binary: &Binary, range: Option<AddressRange>) -> String {
    let functions = count(binary, ItemKind::Function, range);
    let data = count(binary, ItemKind::Data, range);
    let mut counts = Vec::new();
    if functions > 0 || data == 0 {
        counts.push(text::plural(functions, "function", "functions"));
    }
    if data > 0 || functions == 0 {
        counts.push(text::plural(data, "data object", "data objects"));
    }
    counts.join(" and ")
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
    let file = text::shell_quote(&path.display().to_string());
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
/// whose code is a library's. A call through the stub for one of the file's
/// own functions counts as a call to the function.
fn most_used(binary: &Binary, graph: &Graph) -> Option<usize> {
    let stubs = binary.own_function_stubs();
    let mut callers: Vec<(usize, usize)> = graph
        .references
        .iter()
        .filter(|reference| !stubs.contains_key(&reference.from))
        .filter_map(|reference| match reference.to {
            Node::Item(to) => Some((*stubs.get(&to).unwrap_or(&to), reference.from)),
            Node::Import(_) => None,
        })
        .filter(|&(to, _)| is_own_function(binary, to))
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
fn starting_point(binary: &Binary, names: &Names) -> Option<usize> {
    // `main` as SYMBOL would find it, so also `app::main` in Rust.
    let main = match names.nodes("main") {
        [Node::Item(index)] => Some(*index),
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
