//! One module per command. A command loads what it needs, asks the library
//! a question and renders the answer; the analysis itself never lives here.

mod callees;
mod callers;
mod debug_items;
mod overview;
pub mod symbol;

use std::io::Write;

use anyhow::{Result, bail};
use object::Architecture;

use crate::arch;
use crate::binary::{Binary, ItemKind};
use crate::cli::{Cli, Command, DebugCommand};
use crate::graph::{Graph, Node, Reference, ReferenceKind};

/// Runs the command in `cli`, writing its answer to `out`.
pub fn run(cli: &Cli, out: &mut dyn Write) -> Result<()> {
    let binary = Binary::load(&cli.file)?;
    for warning in &binary.warnings {
        eprintln!("warning: {warning}");
    }
    match &cli.command {
        None => overview::run(&cli.file, &binary, out),
        Some(Command::Callers(args)) => callers::run(&binary, &args.symbol, out),
        Some(Command::Callees(args)) => callees::run(&binary, &args.symbol, out),
        Some(Command::Debug(DebugCommand::Items)) => debug_items::run(&binary, out),
    }
}

/// Builds the reference graph, or explains why elfq can't yet.
fn reference_graph(binary: &Binary) -> Result<Graph> {
    if !arch::is_supported(binary.architecture, binary.thumb) {
        bail!("{}", cannot_read_code(binary));
    }
    Ok(Graph::build(binary))
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

/// "1 function", "2 functions".
fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
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

/// One line of a callers or callees table: all the references of one kind
/// between one pair of nodes.
struct Row {
    /// The node on the other end: the caller, or the callee.
    node: Node,
    how: &'static str,
    sites: Vec<u64>,
}

/// Groups references into rows, in the order each row first appears.
/// `other_end` picks the node that the table lists for each reference.
fn rows<'a>(
    references: impl IntoIterator<Item = &'a Reference>,
    other_end: impl Fn(&Reference) -> Node,
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

/// How a reference uses its target, in words.
fn how(reference: &Reference) -> &'static str {
    match reference.kind {
        ReferenceKind::Call => "call",
        ReferenceKind::TailCall => "tail call",
        ReferenceKind::Address => "address taken",
        ReferenceKind::Access => "access",
        ReferenceKind::Pointer => "pointer",
    }
}

/// Addresses, the first few of them: "0x1795, 0x17d4 and 3 more".
fn addresses(sites: &[u64]) -> String {
    const SHOWN: usize = 3;
    let shown: Vec<String> = sites
        .iter()
        .take(SHOWN)
        .map(|site| format!("{site:#x}"))
        .collect();
    match sites.len().checked_sub(SHOWN) {
        Some(more) if more > 0 => format!("{} and {more} more", shown.join(", ")),
        _ => shown.join(", "),
    }
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
