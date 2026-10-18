//! One module per command, and what they share. This file runs the command
//! that the arguments name, and holds the checks that commands make before
//! they answer and the rows that `callers` and `callees` list. How elfq
//! words things is in `text.rs`, and the shapes of its JSON in `json.rs`.

mod callees;
mod callers;
mod debug_items;
mod inlined;
mod json;
mod overview;
mod ranges;
mod retained;
mod retained_tree;
pub mod symbol;
mod text;
mod top;
mod why;

use std::collections::{HashMap, HashSet};
use std::io::Write;

use anyhow::{Result, bail};

use crate::arch;
use crate::binary::{AddressRange, Binary, ItemKind, Source};
use crate::cli::{Cli, Command, DebugCommand, Format};
use crate::graph::{Graph, Node, Reference};

/// What an answer means for elfq's exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Exit status 0.
    Answered,
    /// Something crosses the boundary of the ranges that `callers` or
    /// `callees` asked about, or may: exit status 1, so that a script can
    /// check that code in a range stays in it.
    Crossed,
}

/// Runs the command in `cli`, writing its answer to `out`.
pub fn run(cli: &Cli, out: &mut dyn Write) -> Result<Outcome> {
    check_arguments(cli)?;
    let binary = Binary::load(&cli.file)?;
    for warning in &binary.warnings {
        eprintln!("warning: {warning}");
    }
    let format = cli.format();
    let range = cli.range;
    let answered = |result: Result<()>| result.map(|()| Outcome::Answered);
    match &cli.command {
        None => answered(overview::run(&cli.file, &binary, range, format, out)),
        Some(Command::Top(args)) => answered(top::run(&binary, range, args.all, format, out)),
        Some(Command::Retained(args)) => answered(retained::run(
            &binary,
            &args.symbol,
            range,
            args.all,
            format,
            out,
        )),
        Some(Command::Why(args)) => answered(why::run(&binary, &args.symbol, format, out)),
        Some(Command::Inlined(args)) => answered(inlined::run(
            &cli.file,
            &binary,
            args.symbol.as_deref(),
            args.all,
            format,
            out,
        )),
        Some(Command::Callers(args)) => callers::run(&binary, &args.symbols, format, out),
        Some(Command::Callees(args)) => callees::run(&binary, &args.symbols, format, out),
        Some(Command::Debug(DebugCommand::Items)) => answered(debug_items::run(&binary, out)),
    }
}

/// Refuses arguments that don't go together, before the file is loaded, so
/// that a mistake is reported at once, even for a large file.
fn check_arguments(cli: &Cli) -> Result<()> {
    match &cli.command {
        None | Some(Command::Top(_) | Command::Retained(_)) => Ok(()),
        Some(command @ (Command::Callers(args) | Command::Callees(args)))
            if cli.range.is_some() && args.symbols.iter().all(|query| symbol::is_range(query)) =>
        {
            bail!("{}", range_in_place_of_symbol(cli, command, &args.symbols))
        }
        Some(command @ (Command::Callers(args) | Command::Callees(args)))
            if args.symbols.is_empty() =>
        {
            bail!(
                "{} needs a SYMBOL, or ranges such as 0x0+256K",
                command_name(command)
            )
        }
        Some(_) if cli.range.is_some() => {
            bail!("--range limits sizes, so only the overview, top and retained take it")
        }
        Some(Command::Debug(_)) if cli.format() == Format::Json => {
            bail!("the debug commands print only text")
        }
        Some(_) => Ok(()),
    }
}

/// The error for `--range` with `callers` or `callees`, when it adds to the
/// ranges in place of SYMBOL, or there is no SYMBOL: they take ranges there
/// instead. Says the command to type.
fn range_in_place_of_symbol(cli: &Cli, command: &Command, ranges: &[String]) -> String {
    let name = command_name(command);
    let ranges: Vec<String> = ranges
        .iter()
        .map(|range| text::shell_quote(range))
        .chain(cli.range.map(text::range_argument))
        .collect();
    format!(
        "{name} takes ranges in place of SYMBOL, not with --range: elfq {} {name} {}{}",
        text::shell_quote(&cli.file.display().to_string()),
        ranges.join(" "),
        if cli.json { " --json" } else { "" }
    )
}

/// The name of `command` as typed: `callers` for [`Command::Callers`].
fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Top(_) => "top",
        Command::Retained(_) => "retained",
        Command::Why(_) => "why",
        Command::Inlined(_) => "inlined",
        Command::Callers(_) => "callers",
        Command::Callees(_) => "callees",
        Command::Debug(_) => "debug",
    }
}

/// Builds the reference graph, or explains why elfq can't yet.
fn reference_graph(binary: &Binary) -> Result<Graph> {
    check_code_is_readable(binary)?;
    Ok(Graph::build(binary))
}

/// Explains why elfq can't build the reference graph yet, if it can't.
fn check_code_is_readable(binary: &Binary) -> Result<()> {
    if !arch::is_supported(binary.architecture, binary.thumb) {
        bail!("{}", text::cannot_read_code(binary));
    }
    Ok(())
}

/// The index of `node`'s item, or an error that says it is an import,
/// whose code is in another file.
fn item_in_file(binary: &Binary, node: Node) -> Result<usize> {
    match node {
        Node::Item(index) => Ok(index),
        Node::Import(_) => bail!(
            "{} is imported from a shared library, so its code isn't in this file",
            node.name(binary)
        ),
    }
}

/// The index of `node`'s item, or why elfq can't read its code: it is an
/// import, or a function whose end elfq doesn't know.
fn readable_item(binary: &Binary, node: Node) -> Result<usize> {
    let index = item_in_file(binary, node)?;
    let item = &binary.items[index];
    if item.kind == ItemKind::Function && item.size == 0 {
        bail!(
            "elfq doesn't know where {} ends, so it can't read its code",
            item.name
        );
    }
    Ok(index)
}

/// Where an answer that found nothing looked, for its error: `range`, such
/// as `0x00000000..0x000000e0 (.itcm)`, or without one, "the file". Fails
/// instead when no section is in `range`, which is then most likely
/// mistyped, or when elfq can't find the file's functions yet.
fn empty_place(binary: &Binary, range: Option<AddressRange>) -> Result<String> {
    let place = match range {
        Some(range) if text::sections_in(binary, range).is_empty() => bail!(
            "nothing in the file is in {}; the overview lists the ranges that the file's \
             sections occupy",
            text::range_argument(range)
        ),
        Some(range) => text::range_text(binary, range),
        None => "the file".to_string(),
    };
    if !found_functions(binary) {
        bail!("the file has no symbols or unwind table, so elfq can't find its functions yet");
    }
    Ok(place)
}

/// Whether elfq found the file's functions: a symbol table or an unwind
/// table lists them, or in firmware elfq followed the code (see
/// `unnamed_functions.rs`). Otherwise it has only the entry point, and
/// doesn't know its size.
fn found_functions(binary: &Binary) -> bool {
    binary.items.iter().any(|item| match item.source {
        Source::EntryPoint | Source::Reached => item.size > 0,
        source => source.lists_functions(),
    })
}

/// One line of a callers or callees table: the references between one pair
/// of nodes that are worded the same way.
struct Row {
    /// The node on the other end: the caller, or the callee.
    node: Node,
    /// How the node is used, in the words that [`rows`] was given: the HOW
    /// column's, or JSON's.
    how: &'static str,
    sites: Vec<u64>,
}

/// Groups references into rows, in the order each row first appears.
/// `other_end` picks the node that the table lists for each reference, and
/// `how` says how it is used: references with the same words share a row.
fn rows<'a>(
    references: impl IntoIterator<Item = &'a Reference>,
    other_end: impl Fn(&Reference) -> Node,
    how: impl Fn(&Reference) -> &'static str,
) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    // Where each row is in `rows`, since a function can have thousands.
    let mut index: HashMap<(Node, &'static str), usize> = HashMap::new();
    for reference in references {
        let node = other_end(reference);
        let how = how(reference);
        let row = *index.entry((node, how)).or_insert_with(|| {
            rows.push(Row {
                node,
                how,
                sites: Vec::new(),
            });
            rows.len() - 1
        });
        rows[row].sites.push(reference.site);
    }
    rows
}

/// The distinct nodes in `rows`, in order.
fn distinct_nodes(rows: &[Row]) -> Vec<Node> {
    let mut seen = HashSet::new();
    rows.iter()
        .map(|row| row.node)
        .filter(|&node| seen.insert(node))
        .collect()
}

/// The sum of `sizes`, which stops at the largest `u64` rather than
/// overflow: in a damaged file, items or inlined copies can overlap, and
/// their sizes add up to more than a `u64` holds.
fn total(sizes: impl Iterator<Item = u64>) -> u64 {
    sizes.fold(0, u64::saturating_add)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn a_file_whose_functions_elfq_cant_find_says_so() {
        // Such as stripped firmware for a processor whose code elfq can
        // read but not follow yet: it has only its entry point.
        let path = Path::new("testdata/bin/c-cortex-m4-stripped.elf");
        let mut binary = Binary::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap();
        binary
            .items
            .retain(|item| item.source == Source::EntryPoint);
        binary.items[0].size = 0;
        let mut out = Vec::new();
        overview::run(path, &binary, None, Format::Text, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
testdata/bin/c-cortex-m4-stripped.elf: 32-bit Arm executable, stripped
1 function
The file has no symbols or unwind table, so elfq can't find its functions yet.

MEMORY                  SIZE  SECTIONS
0x08000000..0x08000dec  3564  .isr_vector .text .rodata .ARM.exidx
0x20000000..0x20000048    72  .data .bss
"
        );
        let error = top::run(&binary, None, false, Format::Text, &mut Vec::new()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "the file has no symbols or unwind table, so elfq can't find its functions yet"
        );
    }
}
