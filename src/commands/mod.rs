//! One module per command. A command loads what it needs, asks the library
//! a question and renders the answer; the analysis itself never lives here.

mod callees;
mod callers;
mod debug_items;
mod inlined;
mod json;
mod overview;
mod retained;
pub mod symbol;
mod top;
mod tree;
mod why;

use std::io::Write;

use anyhow::{Result, bail};
use object::Architecture;

use crate::arch;
use crate::binary::{AddressRange, Binary, Item, ItemKind, Source};
use crate::cli::{Cli, Command, DebugCommand, Format};
use crate::graph::{Graph, Node, Reference, ReferenceKind};

/// Runs the command in `cli`, writing its answer to `out`.
pub fn run(cli: &Cli, out: &mut dyn Write) -> Result<()> {
    let binary = Binary::load(&cli.file)?;
    for warning in &binary.warnings {
        eprintln!("warning: {warning}");
    }
    let format = cli.format();
    let range = cli.range;
    match &cli.command {
        None => overview::run(&cli.file, &binary, range, format, out),
        Some(Command::Top(args)) => top::run(&binary, range, args.all, format, out),
        Some(Command::Retained(args)) => {
            retained::run(&binary, &args.symbol, range, args.all, format, out)
        }
        Some(_) if range.is_some() => {
            bail!("--range limits sizes, so only the overview, top and retained take it")
        }
        Some(Command::Why(args)) => why::run(&binary, &args.symbol, format, out),
        Some(Command::Inlined(args)) => inlined::run(
            &cli.file,
            &binary,
            args.symbol.as_deref(),
            args.all,
            format,
            out,
        ),
        Some(Command::Callers(args)) => callers::run(&binary, &args.symbol, format, out),
        Some(Command::Callees(args)) => callees::run(&binary, &args.symbol, format, out),
        Some(Command::Debug(_)) if format == Format::Json => {
            bail!("the debug commands print only text")
        }
        Some(Command::Debug(DebugCommand::Items)) => debug_items::run(&binary, out),
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
        bail!("{}", cannot_read_code(binary));
    }
    Ok(())
}

fn cannot_read_code(binary: &Binary) -> String {
    let code = match binary.architecture {
        // Thumb code is readable; this is the older instruction set.
        Architecture::Arm => "32-bit Arm (A32)".to_string(),
        other => architecture_name(other),
    };
    format!("elfq can't read {code} code yet, so it can't tell what calls what")
}

fn architecture_name(architecture: Architecture) -> String {
    match architecture {
        Architecture::X86_64 => "x86-64".to_string(),
        Architecture::Arm => "32-bit Arm".to_string(),
        other => format!("{other:?}"),
    }
}

/// An item's size, `~N` when it is an estimate, or `?` when it is unknown.
fn size_text(item: &Item) -> String {
    match (item.size, item.size_estimated) {
        (0, _) => "?".to_string(),
        (size, true) => format!("~{size}"),
        (size, false) => size.to_string(),
    }
}

/// "1 function", "2 functions".
fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "1 byte", "2 bytes".
fn bytes_text(n: u64) -> String {
    format!("{n} {}", if n == 1 { "byte" } else { "bytes" })
}

/// How many of the largest items the trees of `top` and `retained`, and
/// the list and tree of `inlined`, show. Lines such as "└─ 6 more" sum up
/// the rest, and an item that would be left out on its own is shown too.
const SHOWN: usize = 30;

/// How many of the largest items to show: [`SHOWN`], or with `--all`, every
/// one.
fn shown(all: bool) -> usize {
    if all { usize::MAX } else { SHOWN }
}

/// How many of `count` things to show where `room` fit: all of them, or
/// `room` and a summary of the rest, such as "and 5 more". A summary never
/// stands for one thing, since naming the thing takes no more room.
fn shown_count(count: usize, room: usize) -> usize {
    if count > room.saturating_add(1) {
        room
    } else {
        count
    }
}

/// Names joined into a phrase: "a", "a and b", "a, b and c", or for more
/// than eleven, "a, b, … and 5 more".
fn names_text(names: &[&str]) -> String {
    phrase(names, 10)
}

/// Addresses for a sentence: "0x1795", "0x1795 and 0x17d4", or for more
/// than four, "0x1795, 0x17d4, 0x17e0 and 3 more".
fn addresses_text(sites: &[u64]) -> String {
    let sites: Vec<String> = sites.iter().map(|site| format!("{site:#x}")).collect();
    let sites: Vec<&str> = sites.iter().map(String::as_str).collect();
    phrase(&sites, 3)
}

/// Words joined into a phrase: "a", "a and b", "a, b and c", or for more
/// than `room` and one, the first `room` of them and "and 5 more".
fn phrase(words: &[&str], room: usize) -> String {
    let shown = shown_count(words.len(), room);
    match words {
        _ if shown < words.len() => format!(
            "{} and {} more",
            words[..shown].join(", "),
            words.len() - shown
        ),
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Says that nothing reaches the item called `name` from where the program
/// starts.
fn nothing_reaches(name: &str) -> String {
    format!(
        "Nothing reaches {name} from where the program starts: it is dead code, or used in a \
         way elfq can't see."
    )
}

/// "3 functions", "2 data objects" or "4 items", whichever fits `nodes`.
fn count_text(binary: &Binary, nodes: &[Node]) -> String {
    let kind = |node: &Node| match node {
        Node::Item(index) => Some(binary.items[*index].kind),
        Node::Import(_) => None,
    };
    match nodes.first().map(kind) {
        Some(first) if nodes.iter().all(|node| kind(node) == first) => match first {
            Some(ItemKind::Function) => plural(nodes.len(), "function", "functions"),
            Some(ItemKind::Data) => plural(nodes.len(), "data object", "data objects"),
            None => plural(nodes.len(), "import", "imports"),
        },
        _ => plural(nodes.len(), "item", "items"),
    }
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
    for reference in references {
        let node = other_end(reference);
        let how = how(reference);
        match rows
            .iter_mut()
            .find(|row| row.node == node && row.how == how)
        {
            Some(row) => row.sites.push(reference.site),
            None => rows.push(Row {
                node,
                how,
                sites: vec![reference.site],
            }),
        }
    }
    rows
}

/// The distinct nodes in `rows`, in order.
fn distinct_nodes(rows: &[Row]) -> Vec<Node> {
    let mut nodes: Vec<Node> = Vec::new();
    for row in rows {
        if !nodes.contains(&row.node) {
            nodes.push(row.node);
        }
    }
    nodes
}

/// How a reference uses its target, for the HOW column. Code that reads,
/// writes or loads the address of a data object "uses" it, on every
/// architecture, while "address taken" means a pointer to a function. An
/// import keeps "address taken", since elfq doesn't record yet whether it
/// is a function or data.
fn how(binary: &Binary, reference: &Reference) -> &'static str {
    let is_data = match reference.to {
        Node::Item(index) => binary.items[index].kind == ItemKind::Data,
        Node::Import(_) => false,
    };
    match reference.kind {
        ReferenceKind::Access => "uses",
        ReferenceKind::Address if is_data => "uses",
        kind => json::how(kind),
    }
}

/// Addresses for the AT column, the first few of them: "0x1795, 0x17d4",
/// or "0x1795, 0x17d4, 0x17e0 and 3 more".
fn addresses(sites: &[u64]) -> String {
    let shown: Vec<String> = sites
        .iter()
        .take(shown_count(sites.len(), 3))
        .map(|site| format!("{site:#x}"))
        .collect();
    match sites.len() - shown.len() {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
    }
}

/// The names of the sections that have bytes in `range`.
fn sections_in(binary: &Binary, range: AddressRange) -> Vec<&str> {
    binary
        .sections
        .iter()
        .filter(|section| section.allocated && range.overlap(section.address, section.size) > 0)
        .map(|section| section.name.as_str())
        .collect()
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

/// A range as `--range` takes it, with the sections that have bytes in it,
/// such as `0x00000000..0x000000e0 (.itcm)`.
fn range_text(binary: &Binary, range: AddressRange) -> String {
    let sections = sections_in(binary, range);
    match sections[..] {
        [] => range_argument(range),
        _ => format!("{} ({})", range_argument(range), names_text(&sections)),
    }
}

/// A range as `--range` takes it, its addresses padded to 8 hex digits so
/// that a column of ranges lines up, such as `0x00000000..0x000000e0`.
fn range_argument(range: AddressRange) -> String {
    range.text(format!("{:x}", range.end).len().max(8))
}

/// `text` as one shell word: quoted when it holds anything but letters,
/// digits and `_./@:-`.
fn shell_quote(text: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_./@:-".contains(c);
    if !text.is_empty() && text.chars().all(plain) {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn a_summary_never_stands_for_one_thing() {
        let names: Vec<String> = (1..=12).map(|n| format!("f{n}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(
            names_text(&names[..11]),
            "f1, f2, f3, f4, f5, f6, f7, f8, f9, f10 and f11"
        );
        assert_eq!(
            names_text(&names),
            "f1, f2, f3, f4, f5, f6, f7, f8, f9, f10 and 2 more"
        );
        assert_eq!(addresses(&[1, 2, 3, 4]), "0x1, 0x2, 0x3, 0x4");
        assert_eq!(addresses(&[1, 2, 3, 4, 5]), "0x1, 0x2, 0x3 and 2 more");
        assert_eq!(addresses_text(&[1, 2, 3, 4]), "0x1, 0x2, 0x3 and 0x4");
        assert_eq!(addresses_text(&[1, 2, 3, 4, 5]), "0x1, 0x2, 0x3 and 2 more");
    }

    #[test]
    fn a_sentence_joins_its_last_two_words_with_and() {
        assert_eq!(addresses_text(&[0x10]), "0x10");
        assert_eq!(addresses_text(&[0x10, 0x20]), "0x10 and 0x20");
    }

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

    #[test]
    fn one_byte_is_a_byte() {
        assert_eq!(bytes_text(0), "0 bytes");
        assert_eq!(bytes_text(1), "1 byte");
        assert_eq!(bytes_text(2), "2 bytes");
    }
}
