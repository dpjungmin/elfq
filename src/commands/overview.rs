//! `elfq FILE`: what the file is, what elfq found in it, where in memory
//! its sections are, which items keep the most bytes, and what to ask next:
//! for firmware with several blocks of memory, that includes `reach` for
//! the interrupt handler that reaches the most code, to show which blocks
//! its code and data are in, and for Cortex-M firmware with a load image
//! or a section without contents, `startup`.
//! With `--range`, what elfq found in the range and which items keep the
//! most of its bytes.

use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};
use serde::Serialize;

use super::checks::{check_has_bytes, found_functions, reference_graph};
use super::kept::Kept;
use super::retained_tree::RetainedTree;
use super::symbol::Names;
use super::{Outcome, json, startup, text};
use crate::binary::{AddressRange, Binary, FileKind, ItemKind, MemoryBlock, memory_blocks, total};
use crate::cli::Format;
use crate::graph::{Graph, Node, Reached, RootKind};
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
pub(super) fn run(
    path: &Path,
    binary: &Binary,
    range: Option<AddressRange>,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    match (format, range) {
        (Format::Json, _) => write_json(path, binary, range, out)?,
        (_, Some(range)) => write_range(path, binary, range, format, out)?,
        (_, None) => write_text(path, binary, format, out)?,
    }
    Ok(Outcome::Answered)
}

/// Writes the overview of the whole file as text.
fn write_text(path: &Path, binary: &Binary, format: Format, out: &mut dyn Write) -> Result<()> {
    writeln!(out, "{}", file_text(path, binary))?;
    writeln!(out, "{}", found_text(binary))?;
    if binary.instruction_set().is_none() {
        writeln!(out, "{}.", text::cannot_read_code(binary))?;
        return Ok(());
    }
    let graph = Kept::new(Graph::build(binary));
    for line in found_notes(binary) {
        writeln!(out, "{line}")?;
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
        let names = Kept::new(Names::new(binary));
        let examples: Vec<String> = [
            most_used(binary, &graph).map(|item| about(&names, "callers", item)),
            names
                .starting_point()
                .map(|item| about(&names, "callees", item)),
            inlined(binary),
        ]
        .into_iter()
        .flatten()
        .collect();
        return text::write_try(path, &examples, out);
    }
    let retained = Kept::new(RetainedTree::new(binary, &graph, None));
    let names = retained.names();
    // Without functions, as in a stripped file whose functions elfq can't
    // find, there is nothing to rank.
    let kept = retained.kept();
    if kept > 0 {
        writeln!(out)?;
        retained.largest_table(SHOWN, format).write(out)?;
    }
    // `reach` shows a handler's code and data by block of memory.
    let handler = match blocks.len() {
        0 | 1 => None,
        _ => handler_reaching_most_code(binary, &graph),
    };
    let examples: Vec<String> = ranges
        .into_iter()
        .chain(
            [
                startup::reads(path, binary).then(|| "startup".to_string()),
                (kept > 0).then(|| "top".to_string()),
                (kept > 0).then(|| "top --tree".to_string()),
                names
                    .starting_point()
                    .map(|item| about(names, "retained", item)),
                handler.map(|item| about(names, "reach", item)),
                largest_kept(&retained).map(|item| about(names, "why", item)),
                inlined(binary),
            ]
            .into_iter()
            .flatten(),
        )
        .collect();
    text::write_try(path, &examples, out)
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
    let graph = Kept::new(reference_graph(binary)?);
    let retained = Kept::new(RetainedTree::new(binary, &graph, Some(range)));
    check_has_bytes(binary, Some(range))?;
    writeln!(out, "{}", file_text(path, binary))?;
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
        retained.largest_table(SHOWN, format).write(out)?;
    }
    let examples: Vec<String> = [
        Some(format!("top --range {}", text::range_argument(range))),
        largest_kept(&retained).map(|item| about(retained.names(), "why", item)),
    ]
    .into_iter()
    .flatten()
    .collect();
    text::write_try(path, &examples, out)
}

/// What the file is, such as "app.elf: x86-64 executable, stripped".
pub(super) fn file_text(path: &Path, binary: &Binary) -> String {
    let names = match (binary.has_symbols, binary.has_dwarf) {
        (true, true) => "with symbols and DWARF debug info",
        (true, false) => "with symbols",
        (false, true) => "with DWARF debug info but no symbol table",
        (false, false) => "stripped",
    };
    format!(
        "{}: {} {}, {names}",
        path.display(),
        text::architecture_name(binary.architecture),
        binary.kind
    )
}

/// What elfq found in the file, such as "29 functions and 34 data objects,
/// 7835 bytes of code and data, plus 2 imports".
pub(super) fn found_text(binary: &Binary) -> String {
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
    found
}

/// What the overview says under [`found_text`] about what elfq found, in a
/// file whose code it can read: that it found no functions, or how it
/// names the items that have no symbols.
pub(super) fn found_notes(binary: &Binary) -> Vec<String> {
    let mut notes = Vec::new();
    if !found_functions(binary) {
        notes.push(format!("{}.", text::NO_FUNCTIONS));
    }
    match text::unnamed_items(binary) {
        Some((function, Some(data))) => notes.push(format!(
            "Items without symbols are named by address, like {} and {}.",
            function.name, data.name
        )),
        Some((function, None)) => notes.push(format!(
            "Functions without symbols are named by address, like {}.",
            function.name
        )),
        None => {}
    }
    notes
}

/// Lists the blocks of addresses that the sections and their load images
/// occupy, when there are several, as in firmware with flash and RAM, and
/// returns them.
fn write_memory(binary: &Binary, out: &mut dyn Write) -> Result<Vec<MemoryBlock>> {
    let blocks = memory_blocks(&binary.sections);
    let Some(table) = memory_table(binary, &blocks) else {
        return Ok(Vec::new());
    };
    writeln!(out)?;
    table.write(out)?;
    Ok(blocks)
}

/// The table of `blocks`, the blocks of addresses that the sections and
/// their load images occupy, or `None` when there is only one, or none.
pub(super) fn memory_table(binary: &Binary, blocks: &[MemoryBlock]) -> Option<Table> {
    if blocks.len() < 2 {
        return None;
    }
    let mut table = Table::new(&[
        ("MEMORY", Align::Left),
        ("SIZE", Align::Right),
        ("SECTIONS", Align::Left),
    ]);
    for block in blocks {
        table.row(vec![
            text::range_argument(block.range),
            (block.range.end - block.range.start).to_string(),
            text::sections_text(
                &json::section_names(binary, &block.sections),
                &json::section_names(binary, &block.load_images),
            ),
        ]);
    }
    Some(table)
}

fn write_json(
    path: &Path,
    binary: &Binary,
    range: Option<AddressRange>,
    out: &mut dyn Write,
) -> Result<()> {
    // As the text does, refuse a range with nothing in it.
    if range.is_some() {
        check_has_bytes(binary, range)?;
    }
    let readable = binary.instruction_set().is_some();
    let graph = Kept::new((readable && range.is_none()).then(|| Graph::build(binary)));
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
    total(binary.items.iter().map(|item| binary.bytes_in(item, range)))
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
            Node::Item(index) if binary.items[index].is_own_function() => Some(index),
            _ => None,
        })
        .filter(|&index| table.runs_in_normal_operation(binary.items[index].address))
        .filter_map(|handler| {
            // Only the code counts here, not where it is.
            let reached = Reached::find(binary, graph, handler, &[]);
            (reached.count > 0).then_some((reached.bytes.code, handler))
        })
        .max_by_key(|&(code, handler)| (code, std::cmp::Reverse(handler)))
        .map(|(_, handler)| handler)
}

/// The function with the most distinct callers, leaving out PLT stubs,
/// whose code is a library's. A call through the stub for one of the file's
/// own functions counts as a call to the function.
fn most_used(binary: &Binary, graph: &Graph) -> Option<usize> {
    let items = binary.items.len();
    // For each PLT stub of one of the file's own functions, the function.
    let mut function_of_stub = vec![None; items];
    for (stub, function) in binary.own_function_stubs() {
        function_of_stub[stub] = Some(function);
    }
    let mut callers = vec![0; items];
    let mut last_caller = vec![None; items];
    // The references are sorted by the item that makes them, so all of a
    // caller's uses of a function come before the next caller's: a caller
    // is counted the first time it uses the function, when it isn't yet
    // the function's last caller.
    for reference in &graph.references {
        let Node::Item(to) = reference.to else {
            continue;
        };
        if function_of_stub[reference.from].is_some() {
            continue;
        }
        let to = function_of_stub[to].unwrap_or(to);
        if binary.items[to].is_own_function() && last_caller[to] != Some(reference.from) {
            last_caller[to] = Some(reference.from);
            callers[to] += 1;
        }
    }
    // Of the functions with the most callers, the first.
    let mut best: Option<(usize, usize)> = None;
    for (function, &count) in callers.iter().enumerate() {
        if count > 0 && best.is_none_or(|(_, most)| count > most) {
            best = Some((function, count));
        }
    }
    best.map(|(function, _)| function)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binary::{Item, Source};
    use crate::graph::{Reference, ReferenceKind};

    /// `most_used` in a file of four functions, items 0 to 3, and a
    /// variable, item 4, whose graph has `uses`, as (from, to) pairs of
    /// items, in the graph's order: by the item that makes them.
    fn most_used_with(uses: &[(usize, usize)]) -> Option<usize> {
        let item = |index: usize, kind| Item {
            name: format!("item{index}"),
            aliases: Vec::new(),
            kind,
            address: 0x1000 + 0x10 * index as u64,
            size: 0x10,
            size_estimated: false,
            section: 0,
            source: Source::SymbolTable,
        };
        let mut items: Vec<Item> = (0..4)
            .map(|index| item(index, ItemKind::Function))
            .collect();
        items.push(item(4, ItemKind::Data));
        let binary = Binary::for_tests(Vec::new(), items, Vec::new());
        let references = (uses.iter().enumerate())
            .map(|(site, &(from, to))| Reference {
                from,
                to: Node::Item(to),
                kind: ReferenceKind::Call,
                site: site as u64,
            })
            .collect();
        let graph = Graph {
            references,
            ..Graph::default()
        };
        most_used(&binary, &graph)
    }

    #[test]
    fn the_most_used_function_has_the_most_callers() {
        // Item 3 uses item 0 three times, but items 0 and 2 both use item 1.
        let uses = [(0, 1), (2, 1), (3, 0), (3, 0), (3, 0)];
        assert_eq!(most_used_with(&uses), Some(1));
        // Items 0 and 1 have two callers each, and item 0 comes first, though
        // item 1 has its second caller first.
        let uses = [(0, 1), (2, 0), (2, 1), (2, 0), (3, 0)];
        assert_eq!(most_used_with(&uses), Some(0));
        // A function with more callers than one before it wins.
        let uses = [(0, 2), (1, 0), (1, 2), (3, 2)];
        assert_eq!(most_used_with(&uses), Some(2));
        // Uses of a variable don't count.
        let uses = [(0, 4), (1, 4), (2, 4), (3, 0)];
        assert_eq!(most_used_with(&uses), Some(0));
        assert_eq!(most_used_with(&[(0, 4), (1, 4)]), None);
    }
}
