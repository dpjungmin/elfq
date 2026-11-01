//! `elfq FILE reach SYMBOL`: everything that SYMBOL calls or uses, at any
//! depth, and where in memory it is.
//!
//! One search from SYMBOL follows every chain of references, as `retained`
//! and a linker's `--gc-sections` follow them, and keeps one shortest chain
//! to each item, as `why` does (see `graph/paths.rs`). Each item hangs under
//! the item that uses it on its chain, so the answer is a tree with SYMBOL
//! at the top, which the JSON has whole.
//!
//! The text lists the items by the overview's blocks of memory, with code
//! and data apart, and names the item that uses each. A call through a
//! linker's veneer is listed as a call from the code that calls the veneer,
//! as `callees` lists it for ranges, and the veneer has a row of its own,
//! since its bytes are in memory too. `--tree` draws the tree instead, with
//! the veneer as a step of its own, as the JSON has it.
//!
//! With `--to`, the answer is what SYMBOL reaches in some ranges of memory,
//! and the exit status says whether it reaches anything there. Each row
//! names the code to change: an item that code outside the ranges uses
//! gets the row that `callees USER --to RANGE` lists for USER, that code. A
//! veneer whose target is in the ranges too gets a row of its own, though
//! `callees` steps over it. The calls and jumps that elfq can't follow stay
//! when they may go to the ranges, by the rule of `callees --to` (see
//! `Unresolved::may_go_to` in `graph`). The tree and the JSON keep only the
//! chains to what is in the ranges and to those calls and jumps.
//!
//! Without SYMBOL, `elfq FILE reach` lists where the program starts, and
//! the bytes of code and data that each start reaches in each block, as
//! `reach START` counts them. Those come from one search from each start
//! that only adds up bytes (see [`Reached`]), since a large program has many
//! starts, and the tree of each would take a while.
//!
//! The words the code uses:
//!
//! - A *step* is a row of the tree: SYMBOL, or an item or import that it
//!   reaches ([`Step`]).
//! - A step's *chain* is the shortest chain of references from SYMBOL to
//!   it. Its *via* is the last reference on the chain, from its *parent*.
//! - The *use* that a row lists ([`Use`]) is that reference, but for the
//!   target of a veneer, which is listed as called through the veneer.
//! - The *counted* steps are those whose bytes the answer adds up: every
//!   one, or with `--to`, those in its ranges ([`Reach::counted`]).
//! - The *kept* steps are those that the tree and the JSON have: every one,
//!   or with `--to`, the chains to what is listed ([`Reach::kept`]).
//! - The *shown* steps are those that get rows: the 30 largest, or with
//!   `--all`, every one ([`Reach::shown`]).
//!
//! Here, `mod.rs` holds the search, [`Reach::find`], and what every form of
//! the answer asks of it; `rows.rs` the text without `--to`, by block of
//! memory; `to.rs` the text with `--to`; `tree.rs` the tree of `--tree`;
//! `json.rs` the JSON; and `starts.rs` the answer without SYMBOL.
//!
//! [`Reached`]: crate::graph::Reached

mod json;
mod rows;
mod starts;
mod to;
mod tree;

pub(super) use starts::starts;

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};

use super::checks::{check_code_is_readable, readable_item};
#[cfg(feature = "tui")]
use super::flat_answer::FlatAnswer;
use super::json::section_names;
use super::kept::Kept;
use super::symbol::{Names, Symbol};
use super::text::CommandLine;
use super::{Outcome, range_check};
use crate::binary::{AddressRange, Binary, CodeAndData, ItemKind, MemoryBlock, memory_blocks};
use crate::cli::Format;
use crate::graph::{
    Graph, ItemsInRanges, Node, Reference, ReferenceKind, RootKind, ShortestPaths, Unfollowed,
    Unresolved, VeneerTarget, Veneers,
};
use tree::Tree;

/// Answers `elfq FILE reach SYMBOL`. The text lists SYMBOL and the 30
/// largest items it reaches, or with `all`, every one, by block, or with
/// `--tree`, as a tree. `to`, the ranges of `--to`, limits the answer to
/// what SYMBOL reaches in them, with every row, and then the answer is
/// [`Outcome::Crossed`] when it lists something.
pub fn run(
    binary: &Binary,
    symbol: &str,
    to: &[AddressRange],
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    check_code_is_readable(binary)?;
    let names = Kept::new(Names::new(binary));
    let symbol = names.find(symbol)?;
    let index = readable_item(binary, symbol.node)?;
    let graph = Kept::new(Graph::build(binary));
    reach(binary, &graph, &names, &symbol, index, to, all, format, out)
}

/// Answers `reach SYMBOL` about `symbol`, the item at `index`, whose code
/// elfq can read, in `graph`, the file's reference graph, as [`run`] does.
#[expect(clippy::too_many_arguments)]
pub(super) fn reach<'a>(
    binary: &'a Binary,
    graph: &Graph,
    names: &Names<'a>,
    symbol: &Symbol,
    index: usize,
    to: &[AddressRange],
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    let lead_name = symbol.lead_name(binary);
    let reach = Kept::new(Reach::find(
        binary, graph, names, index, lead_name, to, all,
    )?);
    match format {
        Format::Json => reach.write_json(out)?,
        Format::Text => reach.write_text(false, out)?,
        Format::Tree => reach.write_text(true, out)?,
    }
    Ok(reach.outcome())
}

/// Answers `elfq FILE reach` without SYMBOL, for the file at `path`: where
/// the program starts, and the bytes of code and data that each start
/// reaches, in each block of memory when there are several. `all` lists
/// every start, and names every function that elfq can't read. `to`, the
/// ranges of `--to`, `range`, that of `--range`, and `--tree` need SYMBOL,
/// and are refused.
pub fn run_starts(
    path: &Path,
    binary: &Binary,
    to: &[AddressRange],
    range: Option<AddressRange>,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    check_code_is_readable(binary)?;
    let names = Kept::new(Names::new(binary));
    if !to.is_empty() || range.is_some() || format == Format::Tree {
        bail!("{}", needs_symbol(path, &names, to, range, all, format));
    }
    let graph = Kept::new(Graph::build(binary));
    let starts = starts(binary, &graph, &names);
    match format {
        Format::Json => starts.write_json(out)?,
        Format::Text | Format::Tree => starts.answer(path, all).write(out)?,
    }
    Ok(Outcome::Answered)
}

/// The error for `--to`, `--range` or `--tree` without SYMBOL, with a
/// command to type instead that answers: about the function that the
/// overview starts from (see [`Names::starting_point`]), such as `main`,
/// and with the other options given.
fn needs_symbol(
    path: &Path,
    names: &Names,
    to: &[AddressRange],
    range: Option<AddressRange>,
    all: bool,
    format: Format,
) -> String {
    let refused = if range.is_some() {
        "reach takes --to, not --range, to list only what a SYMBOL reaches in a range"
    } else if !to.is_empty() {
        "reach --to needs a SYMBOL"
    } else {
        "reach --tree needs a SYMBOL"
    };
    let Some(start) = names.starting_point() else {
        return refused.to_string();
    };
    let command = CommandLine::new(path, "reach")
        .word(&names.query_for(Node::Item(start)))
        .ranges("--to", to.iter().copied().chain(range))
        .flag("--all", all)
        .flag("--tree", format == Format::Tree)
        .flag("--json", format == Format::Json);
    format!("{refused}, such as {command}")
}

/// What `reach SYMBOL` writes as text about the item at `index`, which
/// its first line names `lead_name` (see [`Symbol::lead_name`]): with every
/// row and every name, as browse shows it, and as the command writes it
/// without `--all`.
///
/// [`Symbol::lead_name`]: super::symbol::Symbol::lead_name
#[cfg(feature = "tui")]
pub(super) fn answer<'a>(
    binary: &'a Binary,
    graph: &Graph,
    names: &Names<'a>,
    index: usize,
    lead_name: String,
) -> (FlatAnswer, FlatAnswer) {
    let mut reach = Reach::find(binary, graph, names, index, lead_name, &[], true)
        .expect("without --to, there is no range for reach to refuse");
    let all = reach.answer(Body::Rows);
    // The same, but with the rows and names that the command lists.
    reach.all = false;
    (all, reach.answer(Body::Rows))
}

/// What the text answer of `reach SYMBOL` shows under its first lines.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Body {
    /// The rows by block of memory.
    Rows,
    /// A tree, with `--tree`, which the answer leaves out for its writer
    /// to draw.
    Tree,
}

/// What SYMBOL reaches.
struct Reach<'n, 'a> {
    binary: &'a Binary,
    graph: &'n Graph,
    /// The names that find each item, for the short names of the rows.
    names: &'n Names<'a>,
    /// SYMBOL's item, as an index into [`Binary::items`].
    index: usize,
    /// SYMBOL's name as the answer's first line gives it (see
    /// [`Symbol::lead_name`]).
    ///
    /// [`Symbol::lead_name`]: super::symbol::Symbol::lead_name
    lead_name: String,
    /// How SYMBOL uses itself, as a recursive function calls itself:
    /// "calls" or "uses", or `None` when it doesn't. No row shows it.
    self_use: Option<&'static str>,
    /// Whether SYMBOL is a PLT's header, whose one jump goes to the dynamic
    /// loader.
    plt_header: bool,
    /// SYMBOL and what it reaches, as the rows of the tree: SYMBOL first,
    /// then depth first, each item's children in the order it first uses
    /// them.
    steps: Vec<Step>,
    /// The calls and jumps in what SYMBOL reaches that elfq can't follow,
    /// but for the jump in a PLT's header, which goes to the dynamic
    /// loader, and with `--to`, for those that can't go to its ranges. In
    /// the order of the steps they are in, then by site.
    unresolved: Vec<Unresolved>,
    /// The same, as the text lists them: the jump in a veneer to code
    /// outside every item as the call to the veneer, as `callees` lists it
    /// for ranges.
    unfollowed: Vec<Unfollowed>,
    /// The functions SYMBOL reaches whose end elfq doesn't know, in the
    /// order of the steps.
    unreadable: Vec<usize>,
    /// The file's blocks of memory, as the overview lists them.
    blocks: Vec<MemoryBlock>,
    /// The ranges of `--to`: when there are any, the answer is what SYMBOL
    /// reaches in them.
    to: Vec<AddressRange>,
    /// Whether the table shows every row, and the sentences every name.
    all: bool,
}

/// One row of the tree: SYMBOL, or an item or import that it reaches.
struct Step {
    node: Node,
    /// The step of the item that uses it on its chain from SYMBOL, or
    /// `None` for SYMBOL.
    parent: Option<usize>,
    /// The reference on the chain that reaches it, from its parent.
    via: Option<Reference>,
    /// Where its parent uses it as `via` does.
    sites: Vec<u64>,
    /// How the text lists the use that reaches it.
    used: Option<Use>,
    /// The block of memory that holds it, as an index into
    /// [`Reach::blocks`].
    block: Option<usize>,
    /// Whether it is a function that nothing SYMBOL reaches calls (see
    /// `ItemJson::through_pointer` in `json.rs`).
    through_pointer: bool,
}

/// How the text lists the use of an item that reaches it: the use on its
/// chain, or for the target of a veneer, the call to the veneer.
struct Use {
    /// The item that uses it, as an index into [`Binary::items`].
    by: usize,
    kind: ReferenceKind,
    /// The veneer or PLT stub that the call goes through.
    veneer: Option<usize>,
}

impl<'n, 'a> Reach<'n, 'a> {
    /// Finds everything that SYMBOL, the item at `index`, reaches in
    /// `graph`, the file's reference graph, keeping only the calls and
    /// jumps that elfq can't follow that may go to `to`, the ranges of
    /// `--to`, if there are any. `lead_name` is SYMBOL's name as the
    /// answer's first line gives it. SYMBOL must be an item whose code elfq
    /// can read (see [`readable_item`]): of a function whose end elfq
    /// doesn't know, the answer would be that it reaches nothing.
    ///
    /// [`readable_item`]: super::checks::readable_item
    fn find(
        binary: &'a Binary,
        graph: &'n Graph,
        names: &'n Names<'a>,
        index: usize,
        lead_name: String,
        to: &[AddressRange],
        all: bool,
    ) -> Result<Reach<'n, 'a>> {
        let code_outside = graph.code_outside_items();
        for &range in to {
            range_check::check_to_range(binary, range, &code_outside)?;
        }
        let start = Node::Item(index);
        let paths = ShortestPaths::from(binary, graph, start);
        let veneers = Veneers::new(binary, graph);
        let blocks = memory_blocks(&binary.sections);
        let to_itself: Vec<&Reference> = graph
            .references_from(index)
            .iter()
            .filter(|reference| reference.to == start)
            .collect();
        let self_use = match &to_itself[..] {
            [] => None,
            uses if uses.iter().all(|reference| reference.kind.is_call()) => Some("calls"),
            _ => Some("uses"),
        };
        let nodes = (0..binary.items.len())
            .map(Node::Item)
            .chain((0..binary.imports.len()).map(Node::Import))
            .filter(|&node| paths.is_reached(node));
        // Each item's children, in the order it first uses them.
        let mut children: HashMap<usize, Vec<Node>> = HashMap::new();
        for node in nodes {
            if let Some(via) = paths.via(node) {
                children.entry(via.from).or_default().push(node);
            }
        }
        for (&parent, nodes) in &mut children {
            let mut first_use: HashMap<Node, usize> = HashMap::new();
            for (position, reference) in graph.references_from(parent).iter().enumerate() {
                first_use.entry(reference.to).or_insert(position);
            }
            nodes.sort_by_key(|node| first_use[node]);
        }

        let mut steps: Vec<Step> = Vec::new();
        // The steps to add, each with its parent's step, last first.
        let mut next = vec![(start, None)];
        while let Some((node, parent)) = next.pop() {
            let via = paths.via(node);
            let sites = via.map_or_else(Vec::new, |via| {
                graph
                    .references_from(via.from)
                    .iter()
                    .filter(|r| r.to == node && r.kind == via.kind)
                    .map(|r| r.site)
                    .collect()
            });
            let used = via.map(|via| used(&paths, &veneers, node, via));
            let block = match node {
                Node::Item(index) => {
                    let address = binary.items[index].address;
                    blocks
                        .iter()
                        .position(|block| block.range.contains(address))
                }
                Node::Import(_) => None,
            };
            let step = steps.len();
            steps.push(Step {
                node,
                parent,
                via,
                sites,
                used,
                block,
                through_pointer: false,
            });
            if let Node::Item(index) = node
                && let Some(nodes) = children.get(&index)
            {
                next.extend(nodes.iter().rev().map(|&child| (child, Some(step))));
            }
        }
        let called = called(graph, &steps);
        for step in &mut steps[1..] {
            step.through_pointer = match step.node {
                Node::Item(index) => {
                    binary.items[index].kind == ItemKind::Function && !called.contains(&step.node)
                }
                Node::Import(_) => false,
            };
        }

        let mut unresolved = Vec::new();
        let mut unfollowed = Vec::new();
        let mut unreadable = Vec::new();
        for step in &steps {
            let Node::Item(index) = step.node else {
                continue;
            };
            let item = &binary.items[index];
            if item.end_unknown() {
                unreadable.push(index);
            }
            if graph.is_plt_header(step.node) {
                continue;
            }
            for &jump in graph.unresolved_in(index) {
                unresolved.push(jump);
                unfollowed.push(through_veneer(&paths, &veneers, jump));
            }
        }
        let plt_header = graph.is_plt_header(start);
        if !to.is_empty() {
            let through_pointer = !ItemsInRanges::new(binary, to)
                .callable_through_pointer(graph)
                .is_empty();
            unresolved.retain(|jump| jump.may_go_to(to, through_pointer));
            unfollowed.retain(|row| row.unresolved.may_go_to(to, through_pointer));
        }
        Ok(Reach {
            binary,
            graph,
            names,
            index,
            lead_name,
            self_use,
            plt_header,
            steps,
            unresolved,
            unfollowed,
            unreadable,
            blocks,
            to: to.to_vec(),
            all,
        })
    }

    /// Whether `step` is an item in the ranges of `--to`.
    fn in_to(&self, step: &Step) -> bool {
        match step.node {
            Node::Item(index) => self.binary.items[index].is_in(&self.to),
            Node::Import(_) => false,
        }
    }

    /// The steps whose bytes the answer counts: SYMBOL and everything it
    /// reaches, or with `--to`, those in its ranges.
    fn counted(&self) -> Vec<&Step> {
        self.steps
            .iter()
            .filter(|step| self.to.is_empty() || self.in_to(step))
            .collect()
    }

    /// The exit status: with `--to`, [`Outcome::Crossed`] when the answer
    /// lists something in its ranges, a call or jump that may go there, or
    /// code that elfq can't read, which may.
    fn outcome(&self) -> Outcome {
        let found = !self.counted().is_empty()
            || !self.unresolved.is_empty()
            || !self.unreadable.is_empty();
        if !self.to.is_empty() && found {
            Outcome::Crossed
        } else {
            Outcome::Answered
        }
    }

    /// The bytes of code and of data that the steps in `steps` take.
    fn bytes<'s>(&self, steps: impl Iterator<Item = &'s Step>) -> CodeAndData {
        let mut bytes = CodeAndData::default();
        for step in steps {
            if let Node::Item(index) = step.node {
                bytes.add(&self.binary.items[index]);
            }
        }
        bytes
    }

    /// The bytes of code and of data that `steps` take in each block, and
    /// the sections that hold them, in address order.
    fn blocks_reached(&self, steps: &[&Step]) -> Vec<(CodeAndData, Vec<&'a str>)> {
        (0..self.blocks.len())
            .map(|block| {
                let steps: Vec<&Step> = steps
                    .iter()
                    .copied()
                    .filter(|step| step.block == Some(block))
                    .collect();
                let bytes = self.bytes(steps.iter().copied());
                let mut sections: Vec<usize> = steps
                    .iter()
                    .filter_map(|step| match step.node {
                        Node::Item(index) => Some(self.binary.items[index].section),
                        Node::Import(_) => None,
                    })
                    .collect();
                sections.sort_by_key(|&section| (self.binary.sections[section].address, section));
                sections.dedup();
                (bytes, section_names(self.binary, &sections))
            })
            .collect()
    }

    /// Which steps the answer has: every one, or with `--to`, SYMBOL, the
    /// steps in its ranges, those with calls and jumps that elfq can't
    /// follow that may go there, and every step on their chains from
    /// SYMBOL.
    fn kept(&self) -> Vec<bool> {
        let unfollowed: HashSet<Node> = self
            .unresolved
            .iter()
            .map(|unresolved| Node::Item(unresolved.from))
            .collect();
        let mut kept = vec![self.to.is_empty(); self.steps.len()];
        kept[0] = true;
        // A step comes after its parent, so its children are kept first.
        for (step, row) in self.steps.iter().enumerate().rev() {
            if kept[step] || self.in_to(row) || unfollowed.contains(&row.node) {
                kept[step] = true;
                if let Some(parent) = row.parent {
                    kept[parent] = true;
                }
            }
        }
        kept
    }

    /// Writes the answer as text: the items by block, or with `tree`, as
    /// a tree drawn with branches.
    fn write_text(&self, tree: bool, out: &mut dyn Write) -> Result<()> {
        if !self.to.is_empty() {
            return self.write_to_text(tree, out);
        }
        let answer = self.answer(if tree { Body::Tree } else { Body::Rows });
        if !tree || self.reaches_nothing() {
            return answer.write(out);
        }
        answer.write_with(Some(&|out: &mut dyn Write| Tree::new(self).write(out)), out)
    }

    /// The calls and jumps to code outside every item, which the rows list.
    fn outside(&self) -> Vec<Unfollowed> {
        self.unfollowed
            .iter()
            .filter(|unfollowed| unfollowed.unresolved.target.is_some())
            .copied()
            .collect()
    }

    /// Whether SYMBOL reaches nothing that a row would list, so that the
    /// answer is a few sentences.
    fn reaches_nothing(&self) -> bool {
        self.steps.len() == 1 && self.outside().is_empty()
    }
}

/// How the text lists the use that reaches `node`, from `via`: as the call
/// to a veneer when `via` is the veneer's jump to its target and the veneer
/// is reached by a call.
fn used(paths: &ShortestPaths, veneers: &Veneers, node: Node, via: Reference) -> Use {
    match call_to_veneer(paths, veneers, via.from, VeneerTarget::Node(node)) {
        Some(call) => Use {
            by: call.from,
            kind: call.kind,
            veneer: Some(via.from),
        },
        None => Use {
            by: via.from,
            kind: via.kind,
            veneer: None,
        },
    }
}

/// `jump` as the text lists it: when it is a veneer's jump to code outside
/// every item, and a call reaches the veneer, as that call, through the
/// veneer.
fn through_veneer(paths: &ShortestPaths, veneers: &Veneers, jump: Unresolved) -> Unfollowed {
    let call = jump.target.and_then(|target| {
        call_to_veneer(paths, veneers, jump.from, VeneerTarget::Outside(target))
    });
    match call {
        Some(call) => Unfollowed::through_veneer(&call, jump.from, jump.target),
        None => Unfollowed {
            unresolved: jump,
            veneer: None,
        },
    }
}

/// When `veneer` is a veneer that jumps to `target`, and the chain from
/// SYMBOL reaches it by a call, that call.
fn call_to_veneer(
    paths: &ShortestPaths,
    veneers: &Veneers,
    veneer: usize,
    target: VeneerTarget,
) -> Option<Reference> {
    if veneers.target(veneer) != Some(target) {
        return None;
    }
    paths
        .via(Node::Item(veneer))
        .filter(|call| call.kind.is_call())
}

/// The nodes that something in `steps` calls, or that a vector table among
/// them points to, since only the processor calls through it.
fn called(graph: &Graph, steps: &[Step]) -> HashSet<Node> {
    let mut called = HashSet::new();
    for step in steps {
        let Node::Item(index) = step.node else {
            continue;
        };
        let vector_table = graph
            .root(step.node)
            .is_some_and(|root| root.kind == RootKind::VectorTable);
        for reference in graph.references_from(index) {
            if reference.kind.is_call()
                || (vector_table && reference.kind == ReferenceKind::Pointer)
            {
                called.insert(reference.to);
            }
        }
    }
    called
}
