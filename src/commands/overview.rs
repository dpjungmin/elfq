//! `elfq FILE`: what the file is, what elfq found in it, where in memory
//! its sections are, which items keep the most bytes, and what to ask next:
//! for firmware with several blocks of memory, that includes `reach` for
//! the interrupt handler that reaches the most code, to show which blocks
//! its code and data are in.
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
use crate::binary::{AddressRange, Binary, FileKind, ItemKind, MemoryBlock, Source, memory_blocks};
use crate::cli::Format;
use crate::graph::{Graph, Node, RootKind, ShortestPaths};
use crate::render::{Align, Table};

/// How many of the largest items the overview's table shows. It is made as
/// `top`'s is, which shows 30.
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
    /// The blocks of addresses that the file's sections and their load
    /// images occupy, such as flash and RAM, in address order.
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

/// Answers `elfq FILE`, for the whole file or only the bytes in `range`.
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
        return write_range(path, binary, range, format, out);
    }
    write_file(path, binary, out)?;

    let mut found = items_text(binary, None);
    // A stripped file may have only its entry point, of unknown size.
    let bytes = total_bytes(binary, None);
    if bytes > 0 {
        found += &format!(", {}", text::bytes_of_items(binary, None));
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
        writeln!(out, "{}.", text::NO_FUNCTIONS)?;
    }
    match text::unnamed_items(binary) {
        Some((function, Some(data))) => writeln!(
            out,
            "Items without symbols are named by address, like {} and {}.",
            function.name, data.name
        )?,
        Some((function, None)) => writeln!(
            out,
            "Functions without symbols are named by address, like {}.",
            function.name
        )?,
        None => {}
    }
    let blocks = write_memory(binary, out)?;
    // Only ranges that hold something: in a stripped file whose functions
    // elfq can't find, the sections hold nothing it knows of.
    let ranges: Vec<String> = blocks
        .iter()
        .filter(|block| total_bytes(binary, Some(block.range)) > 0)
        .map(|block| format!("--range {}", text::range_argument(block.range)))
        .collect();

    // elfq doesn't treat a shared library's exported functions as roots
    // yet, so most of its graph is unreachable: there is no tree, and no
    // `top`, `retained` or `why` to suggest.
    if binary.kind == FileKind::SharedLibrary {
        let names = Names::new(binary);
        let examples: Vec<String> = [
            most_used(binary, &graph).map(|item| about(&names, "callers", item)),
            starting_point(binary, &names).map(|item| about(&names, "callees", item)),
            inlined(binary),
        ]
        .into_iter()
        .flatten()
        .collect();
        return write_try(path, &examples, out);
    }
    let retained = RetainedTree::from_graph(binary, graph, None);
    let names = retained.names();
    // Without functions, as in a stripped file whose functions elfq can't
    // find, there is nothing to rank.
    let kept = retained.kept();
    if kept > 0 {
        writeln!(out)?;
        retained.write_largest(SHOWN, format, out)?;
    }
    // `reach` shows a handler's code and data by block of memory.
    let handler = match blocks.len() {
        0 | 1 => None,
        _ => handler_reaching_most_code(binary, &retained.graph),
    };
    let examples: Vec<String> = ranges
        .into_iter()
        .chain(
            [
                (kept > 0).then(|| "top".to_string()),
                (kept > 0).then(|| "top --tree".to_string()),
                starting_point(binary, names).map(|item| about(names, "retained", item)),
                handler.map(|item| about(names, "reach", item)),
                largest_kept(&retained).map(|item| about(names, "why", item)),
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
    format: Format,
    out: &mut dyn Write,
) -> Result<()> {
    if binary.kind == FileKind::SharedLibrary {
        bail!("elfq can't yet tell what keeps the bytes of a range in a shared library");
    }
    let retained = RetainedTree::build(binary, Some(range))?;
    super::check_has_bytes(binary, Some(range))?;
    write_file(path, binary, out)?;
    writeln!(
        out,
        "{} in {}, {}",
        items_text(binary, Some(range)),
        text::range_text(binary, range, false),
        text::bytes_of_items(binary, Some(range))
    )?;
    // When nothing reaches the items in the range, there is nothing to rank.
    if retained.kept() > 0 {
        writeln!(out)?;
        retained.write_largest(SHOWN, format, out)?;
    }
    let examples: Vec<String> = [
        Some(format!("top --range {}", text::range_argument(range))),
        largest_kept(&retained).map(|item| about(retained.names(), "why", item)),
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

/// Lists the blocks of addresses that the sections and their load images
/// occupy, when there are several, as in firmware with flash and RAM, and
/// returns them.
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
        table.row(vec![
            text::range_argument(block.range),
            (block.range.end - block.range.start).to_string(),
            sections_text(
                &json::section_names(binary, &block.sections),
                &json::section_names(binary, &block.load_images),
            ),
        ]);
    }
    writeln!(out)?;
    table.write(out)?;
    Ok(blocks)
}

/// The sections in a block, then the sections whose load images are in
/// it, such as ".isr_vector .text .rodata, load image of .data".
fn sections_text(sections: &[&str], load_images: &[&str]) -> String {
    let mut text = sections.join(" ");
    if !load_images.is_empty() {
        if !text.is_empty() {
            text += ", ";
        }
        text += match load_images.len() {
            1 => "load image of ",
            _ => "load images of ",
        };
        text += &load_images.join(" ");
    }
    text
}

fn write_json(
    path: &Path,
    binary: &Binary,
    range: Option<AddressRange>,
    out: &mut dyn Write,
) -> Result<()> {
    // As the text does, refuse a range with nothing in it.
    if range.is_some() {
        super::check_has_bytes(binary, range)?;
    }
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

/// How many items of `kind` the file has, or with a range, how many have
/// bytes in it.
fn count(binary: &Binary, kind: ItemKind, range: Option<AddressRange>) -> usize {
    binary
        .items
        .iter()
        .filter(|item| item.kind == kind)
        .filter(|item| range.is_none() || binary.bytes_in(item, range) > 0)
        .count()
}

/// The bytes of all the items, or with a range, those in it.
fn total_bytes(binary: &Binary, range: Option<AddressRange>) -> u64 {
    super::total(binary.items.iter().map(|item| binary.bytes_in(item, range)))
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

/// `command` asked about `item`, by the shortest name that finds it.
fn about(names: &Names, command: &str, item: usize) -> String {
    let symbol = names.query_for(Node::Item(item));
    format!("{command} {}", text::shell_quote(&symbol))
}

/// `inlined`, when the file has the debug info it needs.
fn inlined(binary: &Binary) -> Option<String> {
    binary.has_dwarf.then(|| "inlined".to_string())
}

/// The largest item that something keeps in the file, as opposed to one
/// where the program starts: a good question for `why`.
fn largest_kept(retained: &RetainedTree) -> Option<usize> {
    let binary = retained.binary;
    let is_root = |index| retained.graph.root(Node::Item(index)).is_some();
    (0..binary.items.len())
        .filter(|&index| retained.tree.is_reachable(Node::Item(index)) && !is_root(index))
        .filter(|&index| retained.own(index) > 0)
        .max_by_key(|&index| (retained.own(index), std::cmp::Reverse(index)))
}

/// The handler in the vector table that reaches the most code, itself
/// included, of those that run in normal operation and reach something
/// else: not the entry point, which reaches nearly everything; nor a
/// HardFault handler, which may print a report but runs only when
/// something has gone wrong; nor a handler that only loops forever, as a
/// default handler does.
fn handler_reaching_most_code(binary: &Binary, graph: &Graph) -> Option<usize> {
    let table = binary.vector_table.as_ref()?;
    graph
        .roots
        .iter()
        .filter(|root| root.kind == RootKind::Handler)
        .filter_map(|root| match root.node {
            Node::Item(index) if is_own_function(binary, index) => Some(index),
            _ => None,
        })
        .filter(|&index| table.runs_in_normal_operation(binary.items[index].address))
        .filter_map(|handler| {
            let paths = ShortestPaths::from(binary, graph, Node::Item(handler));
            let reached: Vec<usize> = (0..binary.items.len())
                .filter(|&index| index != handler && paths.is_reached(Node::Item(index)))
                .collect();
            let code = reached
                .iter()
                .map(|&index| &binary.items[index])
                .filter(|item| item.kind == ItemKind::Function)
                .map(|item| item.size);
            let code = super::total(code.chain([binary.items[handler].size]));
            (!reached.is_empty()).then_some((code, handler))
        })
        .max_by_key(|&(code, handler)| (code, std::cmp::Reverse(handler)))
        .map(|(_, handler)| handler)
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
pub(super) fn starting_point(binary: &Binary, names: &Names) -> Option<usize> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memory_row_names_its_sections_then_its_load_images() {
        assert_eq!(sections_text(&[".text", ".rodata"], &[]), ".text .rodata");
        assert_eq!(
            sections_text(&[".text"], &[".data"]),
            ".text, load image of .data"
        );
        assert_eq!(
            sections_text(&[], &[".itcm", ".data"]),
            "load images of .itcm .data"
        );
    }
}
