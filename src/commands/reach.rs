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
//! and the exit status says whether it reaches anything there. The row of
//! an item that code outside the ranges uses is then the row that `callees
//! USER --to RANGE` lists for that code, so that it names the code to
//! change, but for a veneer whose target is in the ranges too, which
//! `callees` steps over. The calls and jumps that elfq can't follow stay by
//! the rule that `callees --to` keeps them by (see `ranges.rs`), and the
//! tree and the JSON keep only the chains to what is in the ranges and to
//! those calls and jumps.
//!
//! Without SYMBOL, `elfq FILE reach` lists where the program starts, and
//! the bytes of code and data that each start reaches in each block, as
//! `reach START` counts them. Those come from one search from each start
//! that only adds up bytes (see [`Totals`]), since a large program has many
//! starts, and the tree of each would take a while.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};
use serde::Serialize;

use super::symbol::{Names, Symbol};
use super::{Kept, Outcome, callees, json, overview, ranges, text};
use crate::binary::{
    AddressRange, Binary, FileKind, ItemKind, MemoryBlock, STARTUP_ARRAYS, exception_name,
    is_reserved, memory_blocks,
};
use crate::cli::Format;
use crate::graph::{
    Boundary, Graph, Node, Reference, ReferenceKind, RootKind, ShortestPaths, Unfollowed,
    Unresolved, VeneerTarget, Veneers, listed_in,
};
use crate::render::{Align, Table, TreeRow, TreeShape};

#[derive(Serialize)]
struct ReachJson<'a> {
    symbol: json::Symbol<'a>,
    /// The ranges of `--to`, or `null`.
    to: Option<Vec<json::Range>>,
    /// The bytes of the functions among SYMBOL and what it reaches, or with
    /// `--to`, among those in its ranges.
    code_bytes: u64,
    /// The bytes of the data objects, counted the same way.
    data_bytes: u64,
    /// Every block of memory that the overview's `memory` lists, with the
    /// bytes in it that SYMBOL and what it reaches take, counted the same
    /// way.
    blocks: Vec<BlockJson<'a>>,
    /// SYMBOL, then what it reaches, as the rows of a tree: each item comes
    /// after the item that uses it on its chain from SYMBOL, its `parent`.
    /// With `--to`, only the items that the tree draws (see
    /// [`Reach::kept`]).
    items: Vec<ItemJson<'a>>,
    /// The calls and jumps in what SYMBOL reaches that elfq can't follow,
    /// or with `--to`, those that may go to its ranges.
    unresolved: Vec<UnresolvedJson<'a>>,
    /// The functions that SYMBOL reaches whose code elfq can't read, since
    /// it doesn't know where they end.
    unreadable: Vec<json::Item<'a>>,
    /// Whether `items` has everything SYMBOL reaches, or with `--to`, in its
    /// ranges, as far as elfq can tell: `false` when `unreadable` isn't
    /// empty, or `unresolved` is not, other than with `--to`, for calls to
    /// known addresses in its ranges.
    complete: bool,
}

#[derive(Serialize)]
struct BlockJson<'a> {
    #[serde(flatten)]
    range: json::Range,
    code_bytes: u64,
    data_bytes: u64,
    /// The sections in the block that hold SYMBOL or what it reaches, in
    /// address order.
    sections: Vec<&'a str>,
}

#[derive(Serialize)]
struct ItemJson<'a> {
    /// Which row this is, counting from 0, which is SYMBOL's.
    id: usize,
    #[serde(flatten)]
    item: json::Item<'a>,
    /// The `start` of the block that holds it, or `null` for an import, or
    /// for an item in no block, as a damaged file can have.
    block: Option<String>,
    /// How its parent uses it, as in `callees`, or `null` for SYMBOL.
    how: Option<&'static str>,
    /// Where its parent uses it that way.
    sites: Vec<String>,
    /// Whether it is a function that nothing SYMBOL reaches calls, so that
    /// it is there because something takes or stores its address. A pointer
    /// in a vector table counts as a call.
    through_pointer: bool,
    /// Whether it has a byte in the ranges of `--to`, or `null` without
    /// `--to`. The rows where it is `false` lead to those where it isn't,
    /// or to the calls and jumps in `unresolved`.
    in_to: Option<bool>,
    /// The `id` of the row of the item that uses it on its chain from
    /// SYMBOL, or `null` for SYMBOL.
    parent: Option<usize>,
}

#[derive(Serialize)]
struct UnresolvedJson<'a> {
    /// The function it is in.
    caller: json::Item<'a>,
    /// The instruction's address.
    site: String,
    /// `call` or `jump`.
    how: &'static str,
    /// Where it goes, when the code says so but no item is there, or
    /// `null` when only the running program knows.
    target: Option<String>,
}

/// What `elfq FILE reach` lists without SYMBOL: where the program starts,
/// and what each start reaches.
#[derive(Serialize)]
struct StartsJson<'a> {
    /// Each start, in the text's order (see [`starts`]).
    starts: Vec<StartJson<'a>>,
    /// The functions where the program starts whose end elfq doesn't know,
    /// so it can't read their code, such as `_init`.
    unreadable: Vec<json::Item<'a>>,
}

#[derive(Serialize)]
struct StartJson<'a> {
    #[serde(flatten)]
    item: json::Item<'a>,
    /// Why it is a root, as `root` in `why --json` says it, or `None` for
    /// `main`.
    root: Option<String>,
    /// The exceptions that it handles, as the WHY column names them, but
    /// every one.
    exceptions: Vec<String>,
    /// As `reach START --json` counts them.
    code_bytes: u64,
    data_bytes: u64,
    /// Every block of memory that the overview's `memory` lists, with the
    /// bytes in it that the start and what it reaches take.
    blocks: Vec<BlockBytesJson>,
    /// Whether elfq can follow everything that the start reaches, as in
    /// `reach START --json`.
    complete: bool,
}

#[derive(Serialize)]
struct BlockBytesJson {
    #[serde(flatten)]
    range: json::Range,
    code_bytes: u64,
    data_bytes: u64,
}

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
    super::check_code_is_readable(binary)?;
    let names = Kept::new(Names::new(binary));
    let symbol = names.find(symbol)?;
    let index = super::readable_item(binary, symbol.node)?;
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
    super::check_code_is_readable(binary)?;
    let names = Kept::new(Names::new(binary));
    if !to.is_empty() || range.is_some() || format == Format::Tree {
        bail!(
            "{}",
            needs_symbol(path, binary, &names, to, range, all, format)
        );
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
/// overview starts from (see [`overview::starting_point`]), such as `main`,
/// and with the other options given.
fn needs_symbol(
    path: &Path,
    binary: &Binary,
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
    let Some(start) = overview::starting_point(binary, names) else {
        return refused.to_string();
    };
    let file = text::shell_quote(&path.display().to_string());
    let symbol = text::shell_quote(&names.query_for(Node::Item(start)));
    let to: String = to
        .iter()
        .chain(&range)
        .map(|&range| format!(" --to {}", text::range_argument(range)))
        .collect();
    let all = if all { " --all" } else { "" };
    let format = match format {
        Format::Text => "",
        Format::Tree => " --tree",
        Format::Json => " --json",
    };
    format!("{refused}, such as elfq {file} reach {symbol}{to}{all}{format}")
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
) -> (Answer, Answer) {
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
    /// [`ItemJson::through_pointer`]).
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
    /// [`readable_item`]: super::readable_item
    fn find(
        binary: &'a Binary,
        graph: &'n Graph,
        names: &'n Names<'a>,
        index: usize,
        lead_name: String,
        to: &[AddressRange],
        all: bool,
    ) -> Result<Reach<'n, 'a>> {
        let code_outside = ranges::code_outside(graph);
        for &range in to {
            ranges::check_to_range(binary, range, &code_outside)?;
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
            if item.kind == ItemKind::Function && item.size == 0 {
                unreadable.push(index);
            }
            if is_plt_header(graph, step.node) {
                continue;
            }
            for &jump in graph.unresolved_in(index) {
                unresolved.push(jump);
                unfollowed.push(through_veneer(&paths, &veneers, jump));
            }
        }
        let plt_header = is_plt_header(graph, start);
        if !to.is_empty() {
            let through_pointer = !Boundary::new(binary, graph, to)
                .callable_through_pointer()
                .is_empty();
            let may_go_to = |jump: &Unresolved| ranges::may_go_to(jump, to, through_pointer);
            unresolved.retain(may_go_to);
            unfollowed.retain(|row| may_go_to(&row.unresolved));
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
    fn bytes<'s>(&self, steps: impl Iterator<Item = &'s Step>) -> (u64, u64) {
        let (mut code, mut data) = (0, 0);
        for step in steps {
            if let Node::Item(index) = step.node {
                let item = &self.binary.items[index];
                let bytes = match item.kind {
                    ItemKind::Function => &mut code,
                    ItemKind::Data => &mut data,
                };
                *bytes = u64::saturating_add(*bytes, item.size);
            }
        }
        (code, data)
    }

    /// The bytes of code and of data that `steps` take in each block, and
    /// the sections that hold them, in address order.
    fn blocks_reached(&self, steps: &[&Step]) -> Vec<(u64, u64, Vec<&'a str>)> {
        (0..self.blocks.len())
            .map(|block| {
                let steps: Vec<&Step> = steps
                    .iter()
                    .copied()
                    .filter(|step| step.block == Some(block))
                    .collect();
                let (code, data) = self.bytes(steps.iter().copied());
                let mut sections: Vec<usize> = steps
                    .iter()
                    .filter_map(|step| match step.node {
                        Node::Item(index) => Some(self.binary.items[index].section),
                        Node::Import(_) => None,
                    })
                    .collect();
                sections.sort_by_key(|&section| (self.binary.sections[section].address, section));
                sections.dedup();
                (code, data, json::section_names(self.binary, &sections))
            })
            .collect()
    }

    fn write_json(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let item = |node: Node| json::Item::new(binary, node);
        let counted = self.counted();
        let (code_bytes, data_bytes) = self.bytes(counted.iter().copied());
        let blocks = self
            .blocks
            .iter()
            .zip(self.blocks_reached(&counted))
            .map(|(block, (code_bytes, data_bytes, sections))| BlockJson {
                range: json::Range::new(block.range),
                code_bytes,
                data_bytes,
                sections,
            })
            .collect();
        let ids = self.json_ids();
        let items = self
            .steps
            .iter()
            .zip(&ids)
            .filter_map(|(step, &id)| {
                Some(ItemJson {
                    id: id?,
                    item: item(step.node),
                    block: step
                        .block
                        .map(|block| json::address(self.blocks[block].range.start)),
                    how: step.via.map(|via| json::how_field(via.kind)),
                    sites: json::addresses(&step.sites),
                    through_pointer: step.through_pointer,
                    in_to: (!self.to.is_empty()).then(|| self.in_to(step)),
                    parent: step
                        .parent
                        .map(|parent| ids[parent].expect("a row's parent has one")),
                })
            })
            .collect();
        // With `--to`, a call to a known address in its ranges, as to a
        // chip's ROM, is a whole answer, as `callees --to` counts it: only
        // a call through a pointer leaves it open.
        let followed = |unresolved: &Unresolved| !self.to.is_empty() && unresolved.target.is_some();
        let answer = ReachJson {
            symbol: json::Symbol::new(binary, Node::Item(self.index)),
            to: ranges::json_other(&self.to),
            code_bytes,
            data_bytes,
            blocks,
            items,
            unresolved: self
                .unresolved
                .iter()
                .map(|unresolved| UnresolvedJson {
                    caller: item(Node::Item(unresolved.from)),
                    site: json::address(unresolved.site),
                    how: text::call_or_jump(unresolved),
                    target: unresolved.target.map(json::address),
                })
                .collect(),
            unreadable: self
                .unreadable
                .iter()
                .map(|&index| item(Node::Item(index)))
                .collect(),
            complete: self.unresolved.iter().all(followed) && self.unreadable.is_empty(),
        };
        json::write(&answer, out)
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

    /// The `id` of each step's row in the JSON, for the steps the answer
    /// has (see [`Reach::kept`]).
    fn json_ids(&self) -> Vec<Option<usize>> {
        let mut ids = 0..;
        self.kept()
            .into_iter()
            .map(|kept| if kept { ids.next() } else { None })
            .collect()
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

    /// The answer as text, without `--to`: what SYMBOL and what it reaches
    /// take, the bytes in each block of memory when there are several, the
    /// rows by block, and the notes on what elfq can't follow or read. For
    /// a tree, it has no rows, since the tree takes their place, and its
    /// first line counts the calls and jumps as the tree shows them.
    fn answer(&self, body: Body) -> Answer {
        let tree = body == Body::Tree;
        let binary = self.binary;
        let name = &self.lead_name;
        if self.reaches_nothing() {
            // Any calls and jumps elfq can't follow are in SYMBOL, through
            // pointers.
            return Answer {
                lead: nothing_reached_lines(
                    name,
                    self.self_use,
                    self.plt_header,
                    &self.unresolved,
                    self.all,
                ),
                ..Answer::default()
            };
        }

        let reached = &self.steps[1..];
        let items: Vec<Node> = reached
            .iter()
            .map(|step| step.node)
            .filter(|node| matches!(node, Node::Item(_)))
            .collect();
        let imports = reached.len() - items.len();
        let (code, data) = self.bytes(self.steps.iter());
        let bytes = text::code_and_data(code, data);
        let mut lead = match &items[..] {
            [] => format!("{name} takes {bytes}"),
            [Node::Item(index)] => format!(
                "{name} and the {} it reaches take {bytes}",
                kind_name(binary.items[*index].kind)
            ),
            _ => format!(
                "{name} and the {} it reaches take {bytes}",
                text::count_text(binary, &items)
            ),
        };
        if imports > 0 {
            lead += &format!(", plus {}", text::plural(imports, "import", "imports"));
        }
        // Counted as the rows show them: a tree has the veneers that the
        // rows by block step over.
        let unfollowed: Vec<&Unresolved> = if tree {
            self.unresolved.iter().collect()
        } else {
            self.unfollowed.iter().map(|u| &u.unresolved).collect()
        };
        if !unfollowed.is_empty() {
            lead += &format!("; elfq can't follow {}", text::calls_and_jumps(unfollowed));
        }

        let pointers: Vec<&Unresolved> = self
            .unfollowed
            .iter()
            .map(|unfollowed| &unfollowed.unresolved)
            .filter(|unresolved| unresolved.target.is_none())
            .collect();
        let mut notes = Vec::new();
        if !pointers.is_empty() {
            notes.push(self.pointer_note(&pointers));
        }
        if !self.unreadable.is_empty() {
            notes.push(text::unreadable_text(binary, &self.unreadable, self.all));
        }
        let (columns, rows) = if tree {
            (Vec::new(), Vec::new())
        } else {
            let columns = [
                ("ADDRESS", Align::Left),
                ("SIZE", Align::Right),
                ("HOW", Align::Left),
                ("ITEM", Align::Left),
            ];
            let columns = columns.map(|(header, align)| (header.to_string(), align));
            (columns.into(), self.rows(&self.outside()))
        };
        Answer {
            lead: vec![format!("{lead}:")],
            memory: (self.blocks.len() > 1).then(|| self.memory_table()),
            columns,
            rows,
            notes,
        }
    }

    /// Writes the answer as text with `--to`: what SYMBOL reaches in its
    /// ranges, every row, or with `tree`, the chains from SYMBOL to them.
    fn write_to_text(&self, tree: bool, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let name = &self.lead_name;
        let counted = self.counted();
        let symbol_in_to = counted.first().is_some_and(|step| step.parent.is_none());
        let others: Vec<Node> = counted
            .iter()
            .filter(|step| step.parent.is_some())
            .map(|step| step.node)
            .collect();
        let (code, data) = self.bytes(counted.iter().copied());
        let bytes = text::code_and_data(code, data);
        let ranges = ranges::ranges_text(binary, &self.to, self.all);
        // Counted as the rows show them: a tree has the veneers that the
        // other rows step over.
        let unknown: Vec<&Unresolved> = if tree {
            self.unresolved.iter().collect()
        } else {
            self.unfollowed.iter().map(|u| &u.unresolved).collect()
        };
        let mut lead = match (symbol_in_to, &others[..]) {
            (true, []) => {
                format!("{name} takes {bytes} in {ranges}, and reaches nothing else there")
            }
            (true, [Node::Item(index)]) => format!(
                "{name} and the {} it reaches in {ranges} take {bytes}",
                kind_name(binary.items[*index].kind)
            ),
            (true, _) => format!(
                "{name} and the {} it reaches in {ranges} take {bytes}",
                text::count_text(binary, &others)
            ),
            (false, []) if !unknown.is_empty() => format!("{name} reaches no items in {ranges}"),
            (false, []) if self.unreadable.is_empty() => {
                format!("{name} reaches nothing in {ranges}.")
            }
            (false, []) => {
                format!("In the code elfq can read, {name} reaches nothing in {ranges}.")
            }
            (false, _) => format!(
                "{name} reaches {} in {ranges}, which {} {bytes}",
                text::count_text(binary, &others),
                if others.len() == 1 { "takes" } else { "take" }
            ),
        };
        if !unknown.is_empty() {
            // A call through a pointer may go anywhere, but one to an
            // address that the code names goes there.
            let go = if unknown.iter().any(|u| u.target.is_none()) {
                "may go"
            } else if unknown.len() == 1 {
                "goes"
            } else {
                "go"
            };
            let joint = if counted.is_empty() { ", but" } else { ";" };
            lead += &format!(
                "{joint} elfq can't follow {}, which {go} there",
                text::calls_and_jumps(unknown.iter().copied())
            );
        }
        let listed = !counted.is_empty() || !unknown.is_empty();
        if listed {
            lead += ":";
        }
        writeln!(out, "{lead}")?;
        if listed {
            writeln!(out)?;
            if tree {
                Tree::new(self).write(out)?;
            } else {
                self.write_to_rows(&counted, out)?;
            }
        }
        if !self.unreadable.is_empty() {
            if listed {
                writeln!(out)?;
            }
            writeln!(
                out,
                "{}",
                text::unreadable_text(binary, &self.unreadable, self.all)
            )?;
        }
        Ok(())
    }

    /// Writes the rows with `--to`: SYMBOL's, when it is among `counted`,
    /// the steps in its ranges; then the calls and jumps that elfq can't
    /// follow that may go there, first, as `callees --to` lists them, since
    /// they may hide more; then the rest of `counted`, in the order of
    /// [`Reach::rows`]. Every row is shown, with the sites of the use
    /// it names in the AT column.
    fn write_to_rows(&self, counted: &[&Step], out: &mut dyn Write) -> Result<()> {
        let mut table = Table::new(&[
            ("ADDRESS", Align::Left),
            ("SIZE", Align::Right),
            ("HOW", Align::Left),
            ("AT", Align::Left),
            ("ITEM", Align::Left),
        ]);
        let with_at = |mut cells: Vec<String>, sites: &[u64]| {
            cells.insert(3, text::at_column(sites));
            cells
        };
        let (symbol, reached) = match counted {
            [first, rest @ ..] if first.parent.is_none() => (true, rest),
            _ => (false, counted),
        };
        if symbol {
            table.row(with_at(self.symbol_row().into_cells(), &[]));
        }
        for (unfollowed, sites) in ranges::group_unfollowed(&self.unfollowed) {
            let sites = self.unfollowed_sites(&unfollowed, sites);
            table.row(with_at(
                self.unfollowed_row(&unfollowed).into_cells(),
                &sites,
            ));
        }
        for step in self.in_row_order(reached.to_vec()) {
            table.row(with_at(self.row(step).into_cells(), &self.use_sites(step)));
        }
        table.write(out)?;
        Ok(())
    }

    /// Where the item that uses `step` on its row uses it as the row's HOW
    /// says, for the AT column: every such site, as `callees` lists them,
    /// which may be more than the chain's, as when code both reads a
    /// variable and loads its address, which is "uses" either way.
    fn use_sites(&self, step: &Step) -> Vec<u64> {
        let binary = self.binary;
        let used = step.used.as_ref().expect("only SYMBOL has no user");
        // A call through a veneer is a call to the veneer.
        let to = used.veneer.map_or(step.node, Node::Item);
        let how = |kind| text::how_via(binary, step.node, kind, used.veneer);
        self.graph
            .references_from(used.by)
            .iter()
            .filter(|reference| reference.to == to && how(reference.kind) == how(used.kind))
            .map(|reference| reference.site)
            .collect()
    }

    /// Where the code that `unfollowed` names makes it, for the AT column:
    /// `sites`, or for a call through a veneer, every call of its kind from
    /// there to the veneer, as `callees` lists them, since the answer keeps
    /// only the one on the chain to the veneer.
    fn unfollowed_sites(&self, unfollowed: &Unfollowed, sites: Vec<u64>) -> Vec<u64> {
        let Some(veneer) = unfollowed.veneer else {
            return sites;
        };
        let unresolved = &unfollowed.unresolved;
        self.graph
            .references_from(unresolved.from)
            .iter()
            .filter(|reference| {
                reference.to == Node::Item(veneer)
                    && reference.kind.is_call()
                    && (reference.kind == ReferenceKind::Call) == unresolved.is_call
            })
            .map(|reference| reference.site)
            .collect()
    }

    /// The bytes of code and data in each block of memory.
    fn memory_table(&self) -> Table {
        let mut table = Table::new(&[
            ("MEMORY", Align::Left),
            ("CODE", Align::Right),
            ("DATA", Align::Right),
            ("SECTIONS", Align::Left),
        ]);
        let steps: Vec<&Step> = self.steps.iter().collect();
        for (block, (code, data, sections)) in self.blocks.iter().zip(self.blocks_reached(&steps)) {
            table.row(vec![
                text::range_argument(block.range),
                code.to_string(),
                data.to_string(),
                sections.join(" "),
            ]);
        }
        table
    }

    /// SYMBOL's row, then a row for each item it reaches, by block,
    /// functions then data objects, each largest first; then the calls and
    /// jumps to code outside every item in `outside`, and the imports.
    /// Unless the answer shows every row, only the 30 largest items are
    /// shown, and a row such as "9 more data objects in
    /// 0x20000000..0x20000450" sums up the rest of a block's.
    fn rows(&self, outside: &[Unfollowed]) -> Vec<AnswerRow> {
        let mut rows = vec![self.symbol_row()];
        let reached = &self.steps[1..];
        let shown = self.shown(reached);
        let steps = self.in_row_order(reached.iter().collect());
        let imports = steps.partition_point(|step| matches!(step.node, Node::Item(_)));
        let (items, imports) = steps.split_at(imports);
        for steps in items.chunk_by(|a, b| self.group(a) == self.group(b)) {
            self.add_group(steps, &shown, &mut rows);
        }
        for (unfollowed, _) in ranges::group_unfollowed(outside) {
            rows.push(self.unfollowed_row(&unfollowed));
        }
        if !imports.is_empty() {
            self.add_group(imports, &shown, &mut rows);
        }
        rows
    }

    /// SYMBOL's row.
    fn symbol_row(&self) -> AnswerRow {
        let item = &self.binary.items[self.index];
        let cells = vec![
            format!("{:#x}", item.address),
            text::size_text(item),
            String::new(),
        ];
        AnswerRow::new(cells, item.name.clone(), Some(Node::Item(self.index)))
    }

    /// Which group of the table `step`'s row is in: each block's functions,
    /// then its data objects; then the items in no block, which a damaged
    /// file can have; then the imports.
    fn group(&self, step: &Step) -> (usize, bool) {
        match step.node {
            Node::Item(index) => {
                let kind = self.binary.items[index].kind;
                (step.block.unwrap_or(usize::MAX), kind != ItemKind::Function)
            }
            Node::Import(_) => (usize::MAX, true),
        }
    }

    /// `steps` in the order of their rows: by group, each largest first.
    fn in_row_order<'s>(&self, mut steps: Vec<&'s Step>) -> Vec<&'s Step> {
        let binary = self.binary;
        steps.sort_by_key(|step| {
            let node = step.node;
            (
                matches!(node, Node::Import(_)),
                self.group(step),
                Reverse(node.size(binary)),
                node.name(binary),
                node,
            )
        });
        steps
    }

    /// The row of a call or jump that elfq can't follow, as `callees`
    /// words it, but for the AT column: to code outside every item, at its
    /// address, or through a pointer. It names no item, and is about the
    /// item that makes it.
    fn unfollowed_row(&self, unfollowed: &Unfollowed) -> AnswerRow {
        let unresolved = &unfollowed.unresolved;
        let (address, what) = match unresolved.target {
            Some(target) => (format!("{target:#x}"), "(outside any function)"),
            None => (String::new(), "(through a pointer)"),
        };
        let cells = vec![
            address,
            String::new(),
            ranges::unfollowed_how(unfollowed).to_string(),
        ];
        let mut row = AnswerRow::new(cells, String::new(), Some(Node::Item(unresolved.from)));
        row.rest = format!("{what}, used by {}", self.user_name(unresolved.from));
        row
    }

    /// Adds the rows of `steps`, one group of the table, such as a block's
    /// functions: those in `shown`, and a row that sums up the rest. One
    /// that would be left out on its own is shown too, since its row takes
    /// no more room than the sum's.
    fn add_group(&self, steps: &[&Step], shown: &HashSet<Node>, rows: &mut Vec<AnswerRow>) {
        let binary = self.binary;
        let hidden: Vec<&Step> = steps
            .iter()
            .copied()
            .filter(|step| !shown.contains(&step.node))
            .collect();
        for step in steps {
            if hidden.len() < 2 || shown.contains(&step.node) {
                rows.push(self.row(step));
            }
        }
        if hidden.len() < 2 {
            return;
        }
        let first = hidden[0];
        let (what, size) = match first.node {
            Node::Item(index) => {
                let kinds = match binary.items[index].kind {
                    ItemKind::Function => "functions",
                    ItemKind::Data => "data objects",
                };
                let size = super::total(hidden.iter().map(|step| step.node.size(binary)));
                (kinds, size.to_string())
            }
            Node::Import(_) => ("imports", String::new()),
        };
        let place = match first.block {
            Some(block) if self.blocks.len() > 1 => {
                format!(" in {}", text::range_argument(self.blocks[block].range))
            }
            _ => String::new(),
        };
        let cells = vec![String::new(), size, String::new()];
        let more = format!("{} more {what}{place}", hidden.len());
        rows.push(AnswerRow::new(cells, more, None));
    }

    /// An item's or import's row. With `--to`, the row is
    /// written as `callees --to` writes it, with the whole name of the item
    /// that uses it, and no mark for a function reached through a pointer,
    /// which HOW shows.
    fn row(&self, step: &Step) -> AnswerRow {
        let binary = self.binary;
        let (address, size) = match step.node {
            Node::Item(index) => {
                let item = &binary.items[index];
                (format!("{:#x}", item.address), text::size_text(item))
            }
            Node::Import(_) => (String::new(), String::new()),
        };
        let used = step.used.as_ref().expect("only SYMBOL has no user");
        let mark = if step.through_pointer && self.to.is_empty() {
            " (through a pointer)"
        } else {
            ""
        };
        let how = text::how_via(binary, step.node, used.kind, used.veneer);
        let cells = vec![address, size, how.to_string()];
        let mut row = AnswerRow::new(cells, step.node.name(binary).to_string(), Some(step.node));
        row.rest = format!("{mark}, used by {}", self.user_name(used.by));
        row
    }

    /// The name of `index`, an item that uses another, on that item's row:
    /// the shortest that finds it, or with `--to`, its whole name, as
    /// `callees --to` names it.
    fn user_name(&self, index: usize) -> String {
        match self.to[..] {
            [] => self.names.row_name(index),
            _ => self.binary.items[index].name.clone(),
        }
    }

    /// The items and imports in `reached` that get rows: the 30 largest,
    /// or every one.
    fn shown(&self, reached: &[Step]) -> HashSet<Node> {
        let binary = self.binary;
        let mut nodes: Vec<Node> = reached.iter().map(|step| step.node).collect();
        nodes.sort_by_key(|&node| (Reverse(node.size(binary)), node.name(binary), node));
        nodes.into_iter().take(text::shown(self.all)).collect()
    }

    /// The note on the calls and jumps through pointers in `pointers`, all
    /// in what SYMBOL reaches, since SYMBOL may reach more through them.
    fn pointer_note(&self, pointers: &[&Unresolved]) -> String {
        let binary = self.binary;
        let mut callers: Vec<usize> = Vec::new();
        let mut seen: HashSet<usize> = HashSet::new();
        for unresolved in pointers {
            if seen.insert(unresolved.from) {
                callers.push(unresolved.from);
            }
        }
        let names: Vec<&str> = callers
            .iter()
            .map(|&index| binary.items[index].name.as_str())
            .collect();
        let (one, several) = text::call_and_jump_verbs(pointers.iter().copied());
        let through = if pointers.len() == 1 {
            "a pointer"
        } else {
            "pointers"
        };
        let (verb, at) = match callers[..] {
            [_] => {
                let sites: Vec<u64> = pointers.iter().map(|u| u.site).collect();
                (
                    one,
                    format!(", at {}", text::addresses_text(&sites, self.all)),
                )
            }
            _ => (several, String::new()),
        };
        let who = if callers[..] == [self.index] {
            "it"
        } else {
            &binary.items[self.index].name
        };
        format!(
            "{} also {verb} through {through}{at}, which elfq can't follow, so {who} may reach \
             more.",
            text::names_text(&names, self.all)
        )
    }
}

/// What SYMBOL reaches, as `--tree` draws it: each item under the item that
/// uses it on its chain from SYMBOL, in the order that item first uses
/// them, with a veneer as a step of its own; and each call or jump to code
/// outside every item under the item it is in. With `--to`, only the chains
/// to the items in its ranges, and to the calls and jumps that may go
/// there, through pointers too.
struct Tree<'r, 'a> {
    reach: &'r Reach<'r, 'a>,
    /// The steps under each step, in the order that it first uses them.
    children: Vec<Vec<usize>>,
    /// The calls and jumps that elfq can't follow in each step that get
    /// rows, one per kind and target: those to code outside every item, and
    /// with `--to`, those through pointers too.
    outside: Vec<Vec<Unresolved>>,
    /// Whether each step gets a row.
    shown: Vec<bool>,
}

impl<'r, 'a> Tree<'r, 'a> {
    fn new(reach: &'r Reach<'r, 'a>) -> Tree<'r, 'a> {
        let steps = &reach.steps;
        let mut children = vec![Vec::new(); steps.len()];
        for (step, row) in steps.iter().enumerate().skip(1) {
            children[row.parent.expect("only SYMBOL has no parent")].push(step);
        }
        let step_of: HashMap<usize, usize> = steps
            .iter()
            .enumerate()
            .filter_map(|(step, row)| match row.node {
                Node::Item(index) => Some((index, step)),
                Node::Import(_) => None,
            })
            .collect();
        let mut outside: Vec<Vec<Unresolved>> = vec![Vec::new(); steps.len()];
        for &unresolved in &reach.unresolved {
            // Without `--to`, a note below the tree names the calls and
            // jumps through pointers.
            if unresolved.target.is_none() && reach.to.is_empty() {
                continue;
            }
            let rows = &mut outside[step_of[&unresolved.from]];
            let same = |row: &Unresolved| {
                (row.is_call, row.target) == (unresolved.is_call, unresolved.target)
            };
            if !rows.iter().any(same) {
                rows.push(unresolved);
            }
        }
        let mut tree = Tree {
            reach,
            children,
            outside,
            shown: Vec::new(),
        };
        tree.shown = tree.shown();
        tree
    }

    /// Which steps get rows: with `--to`, the steps the answer has (see
    /// [`Reach::kept`]). Without it, SYMBOL; the largest items, or every
    /// one (see [`Reach::shown`]); the steps with rows for calls or jumps
    /// to code outside every item, since rows by block show every one; and
    /// every step on their chains from SYMBOL, so that the tree holds
    /// together. A step that would be the only one left out under a row,
    /// with nothing under it, is shown too, since its row takes no more
    /// room than "1 more" would.
    fn shown(&self) -> Vec<bool> {
        let reach = self.reach;
        if !reach.to.is_empty() {
            return reach.kept();
        }
        let steps = &reach.steps;
        let largest = reach.shown(&steps[1..]);
        let wanted = |step: usize| {
            step == 0 || largest.contains(&steps[step].node) || !self.outside[step].is_empty()
        };
        let mut shown = vec![false; steps.len()];
        for step in 0..steps.len() {
            if wanted(step) {
                let mut on_chain = Some(step);
                while let Some(step) = on_chain
                    && !shown[step]
                {
                    shown[step] = true;
                    on_chain = steps[step].parent;
                }
            }
        }
        let lone: Vec<usize> = (0..steps.len())
            .filter(|&step| shown[step])
            .filter_map(|step| {
                let mut hidden = self.children[step].iter().filter(|&&child| !shown[child]);
                match (hidden.next(), hidden.next()) {
                    (Some(&lone), None) if self.children[lone].is_empty() => Some(lone),
                    _ => None,
                }
            })
            .collect();
        for step in lone {
            shown[step] = true;
        }
        shown
    }

    fn write(&self, out: &mut dyn Write) -> Result<()> {
        let mut table = Table::new(&[
            ("ADDRESS", Align::Left),
            ("SIZE", Align::Right),
            ("HOW", Align::Left),
            ("ITEM", Align::Left),
        ]);
        table.tree(&[self.row(0)], TreeShape::Drawn);
        table.write(out)?;
        Ok(())
    }

    /// The row of `step`, with the rows under it: the steps under it that
    /// are shown, its calls and jumps to code outside every item, and a row
    /// that sums up the steps under it that aren't, and what is under them.
    fn row(&self, step: usize) -> TreeRow {
        let binary = self.reach.binary;
        let Step {
            node,
            via,
            through_pointer,
            ..
        } = self.reach.steps[step];
        let (address, size) = match node {
            Node::Item(index) => {
                let item = &binary.items[index];
                (format!("{:#x}", item.address), text::size_text(item))
            }
            Node::Import(_) => (String::new(), String::new()),
        };
        let how = via.map_or("", |via| text::how_column(binary, node, via.kind));
        let (shown, hidden): (Vec<usize>, Vec<usize>) = self.children[step]
            .iter()
            .partition(|&&child| self.shown[child]);
        let mut children: Vec<TreeRow> = shown.into_iter().map(|child| self.row(child)).collect();
        for unresolved in &self.outside[step] {
            let (address, name) = match unresolved.target {
                Some(target) => (format!("{target:#x}"), "(outside any function)"),
                None => (String::new(), "(through a pointer)"),
            };
            let cells = vec![
                address,
                String::new(),
                text::call_or_jump(unresolved).to_string(),
            ];
            children.push(TreeRow::new(cells, name, Vec::new()));
        }
        // With `--to`, the steps left out aren't part of the answer.
        if !hidden.is_empty() && self.reach.to.is_empty() {
            children.push(self.more(&hidden));
        }
        let mut row = TreeRow::new(
            vec![address, size, how.to_string()],
            node.name(binary),
            children,
        );
        row.mark = through_pointer.then(|| "(through a pointer)".to_string());
        row
    }

    /// The row that sums up the steps in `hidden`, and every step under
    /// them, which no row shows.
    fn more(&self, hidden: &[usize]) -> TreeRow {
        let binary = self.reach.binary;
        let mut nodes: Vec<Node> = Vec::new();
        let mut next = hidden.to_vec();
        while let Some(step) = next.pop() {
            nodes.push(self.reach.steps[step].node);
            next.extend(&self.children[step]);
        }
        // Imports have no bytes in the file, as their own rows show.
        let size = if nodes.iter().all(|node| matches!(node, Node::Import(_))) {
            String::new()
        } else {
            super::total(nodes.iter().map(|node| node.size(binary))).to_string()
        };
        let cells = vec![String::new(), size, String::new()];
        TreeRow::new(cells, format!("{} more", nodes.len()), Vec::new())
    }
}

/// The text answer of `reach`, before it is written, which browse shows
/// too: the lines before its tables, the bytes in each block of
/// memory, the table of rows, with the item that each row is about, and
/// the notes under them. An empty line comes before each table, and before
/// the notes when there is a table of rows.
#[derive(Default)]
pub(super) struct Answer {
    /// The first line, or what the answer says in place of its tables.
    pub lead: Vec<String>,
    pub memory: Option<Table>,
    /// The columns of the table of rows, the last of which names an item.
    pub columns: Vec<(String, Align)>,
    /// Empty when the answer has no table of rows.
    pub rows: Vec<AnswerRow>,
    pub notes: Vec<String>,
}

/// A row of [`Answer`]'s table.
pub(super) struct AnswerRow {
    /// Every cell but the last.
    pub cells: Vec<String>,
    /// The last cell: a name, which may be empty, then what the row says
    /// about it, such as " (may reach more)" or ", used by main".
    pub name: String,
    pub rest: String,
    /// The item or import that the row is about, if any, which browse
    /// opens.
    #[cfg_attr(not(feature = "tui"), expect(dead_code))]
    pub node: Option<Node>,
}

impl AnswerRow {
    fn new(cells: Vec<String>, name: String, node: Option<Node>) -> AnswerRow {
        AnswerRow {
            cells,
            name,
            rest: String::new(),
            node,
        }
    }

    /// Its cells as the table writes them.
    fn into_cells(self) -> Vec<String> {
        let mut cells = self.cells;
        cells.push(self.name + &self.rest);
        cells
    }
}

/// Writes a table, or what takes its place in an [`Answer`].
type WriteTable<'a> = dyn Fn(&mut dyn Write) -> Result<()> + 'a;

impl Answer {
    pub(super) fn write(&self, out: &mut dyn Write) -> Result<()> {
        if self.rows.is_empty() {
            return self.write_with(None, out);
        }
        let table = self.table();
        self.write_with(Some(&|out: &mut dyn Write| Ok(table.write(out)?)), out)
    }

    /// Writes it with what `table` writes, if anything, in place of its
    /// table of rows, as `--tree` draws a tree there.
    fn write_with(&self, table: Option<&WriteTable>, out: &mut dyn Write) -> Result<()> {
        for line in &self.lead {
            writeln!(out, "{line}")?;
        }
        if let Some(memory) = &self.memory {
            writeln!(out)?;
            memory.write(out)?;
        }
        if let Some(table) = table {
            writeln!(out)?;
            table(out)?;
            if !self.notes.is_empty() {
                writeln!(out)?;
            }
        }
        for note in &self.notes {
            writeln!(out, "{note}")?;
        }
        Ok(())
    }

    /// What [`Answer::write`] writes.
    #[cfg(feature = "tui")]
    pub(super) fn written(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out).expect("writing to memory");
        out
    }

    /// The table of rows, as the text writes it.
    pub(super) fn table(&self) -> Table {
        let mut table = Table::new(&self.columns);
        for row in &self.rows {
            let mut cells = row.cells.clone();
            cells.push(format!("{}{}", row.name, row.rest));
            table.row(cells);
        }
        table
    }
}

/// Where the program starts, and what each start reaches.
pub(super) struct Starts<'a> {
    binary: &'a Binary,
    /// The starts, in the order of [`starts`].
    pub starts: Vec<Start>,
    /// The roots that are functions whose end elfq doesn't know, such as
    /// `_init`, as indexes into [`Binary::items`]: elfq can't tell what
    /// they reach.
    pub unreadable: Vec<usize>,
    /// The file's blocks of memory, as the overview lists them.
    pub blocks: Vec<MemoryBlock>,
}

/// One place where the program starts, and what it reaches.
pub(super) struct Start {
    /// The function, as an index into [`Binary::items`].
    pub index: usize,
    /// Why it is a root, or `None` for `main`.
    pub root: Option<RootKind>,
    /// The entries of the vector table that list it, in the table's order,
    /// counting the stack pointer's as 0.
    pub exceptions: Vec<u64>,
    pub totals: Totals,
}

/// A row of the table of starts.
enum StartRow {
    /// A start, as an index into [`Starts::starts`].
    Start(usize),
    /// How many of the starts above, all roots of this kind, the table
    /// leaves out.
    More(RootKind, usize),
}

/// Where the program starts, as `elfq FILE reach` lists it: `main`, but
/// not a shared library's; then each root that is a function whose code
/// elfq can read, other than a PLT's header: the entry point; the handlers
/// in the vector table, in the order of their first entries; the functions
/// that the startup and exit arrays list, `.preinit_array`, `.init_array`
/// and `.fini_array` in that order, each in its array's order; then the
/// others by kind, and by address. Each comes with what it reaches, as
/// `reach START` counts it.
pub(super) fn starts<'a>(binary: &'a Binary, graph: &Graph, names: &Names) -> Starts<'a> {
    let blocks = memory_blocks(&binary.sections);
    let is_start = |index| overview::is_own_function(binary, index);
    let table = binary.vector_table.as_ref();
    let exceptions = |index: usize| -> Vec<u64> {
        let address = binary.items[index].address;
        table.map_or_else(Vec::new, |table| table.exceptions_of(address).collect())
    };
    let mut roots: Vec<(usize, RootKind)> = Vec::new();
    let mut unreadable = Vec::new();
    let mut arrays = Vec::new();
    for root in &graph.roots {
        let Node::Item(index) = root.node else {
            continue;
        };
        let item = &binary.items[index];
        if is_start(index) {
            roots.push((index, root.kind));
        } else if item.kind == ItemKind::Function && item.size == 0 {
            unreadable.push(index);
        }
        if let RootKind::ListedIn(array) = root.kind
            && !arrays.contains(&array)
        {
            arrays.push(array);
        }
    }
    // Where each array lists each function, from its first entry.
    let mut listed: HashMap<(usize, Node), usize> = HashMap::new();
    for array in arrays {
        for (position, node) in listed_in(binary, array).into_iter().enumerate() {
            listed.entry((array, node)).or_insert(position);
        }
    }
    // The roots come by node, so by address: a stable sort keeps that order
    // among roots of the same kind that no array lists.
    roots.sort_by_key(|&(index, kind)| match kind {
        RootKind::Entry => (0, 0, 0, 0),
        RootKind::Handler => (1, 0, 0, exceptions(index).first().copied().unwrap_or(0)),
        RootKind::ListedIn(array) => {
            let name = binary.sections[array].name.as_str();
            let rank = STARTUP_ARRAYS.iter().position(|&startup| startup == name);
            let position = listed.get(&(array, Node::Item(index))).copied();
            (
                2,
                rank.unwrap_or(usize::MAX),
                array,
                position.unwrap_or(0) as u64,
            )
        }
        RootKind::InSection(section) => (3, 0, section, 0),
        _ => (4, 0, 0, 0),
    });
    // A main that is a root too is listed as one.
    let main = match names.nodes("main") {
        [Node::Item(index)]
            if binary.kind != FileKind::SharedLibrary
                && is_start(*index)
                && graph.root(Node::Item(*index)).is_none() =>
        {
            Some((*index, None))
        }
        _ => None,
    };
    let starts = main
        .into_iter()
        .chain(roots.into_iter().map(|(index, kind)| (index, Some(kind))))
        .map(|(index, root)| Start {
            index,
            root,
            exceptions: exceptions(index),
            totals: Totals::find(binary, graph, index, &blocks),
        })
        .collect();
    Starts {
        binary,
        starts,
        unreadable,
        blocks,
    }
}

impl Starts<'_> {
    /// The answer's text: the starts as a table, then what elfq can't
    /// follow or read, and for a shared library, that its exported
    /// functions aren't counted. Each row is about its start.
    pub(super) fn answer(&self, path: &Path, all: bool) -> Answer {
        let binary = self.binary;
        let file = path.display();
        let several = self.blocks.len() > 1;
        let rows = self.rows(all);
        let mut answer = Answer::default();
        if !self.starts.is_empty() {
            let each = if several {
                " in each block of memory"
            } else {
                ""
            };
            answer.lead.push(format!(
                "Where {file} starts, and the bytes of code and data that each reaches{each}:"
            ));
            answer.columns = if several {
                self.blocks
                    .iter()
                    .map(|block| (text::range_start(block.range), Align::Right))
                    .collect()
            } else {
                vec![
                    ("CODE".to_string(), Align::Right),
                    ("DATA".to_string(), Align::Right),
                ]
            };
            answer.columns.push(("WHY".to_string(), Align::Left));
            answer.columns.push(("START".to_string(), Align::Left));
            let numbers = answer.columns.len() - 2;
            for row in &rows {
                let start = match *row {
                    StartRow::Start(start) => &self.starts[start],
                    StartRow::More(kind, count) => {
                        let mut cells = vec![String::new(); numbers];
                        cells.push(text::root_column(binary, kind));
                        answer
                            .rows
                            .push(AnswerRow::new(cells, format!("{count} more"), None));
                        continue;
                    }
                };
                let totals = &start.totals;
                let mut cells: Vec<String> = if several {
                    let bytes = |&(code, data): &(u64, u64)| u64::saturating_add(code, data);
                    totals
                        .blocks
                        .iter()
                        .map(|block| bytes(block).to_string())
                        .collect()
                } else {
                    vec![totals.code.to_string(), totals.data.to_string()]
                };
                cells.push(start.why(binary));
                let mut row = AnswerRow::new(
                    cells,
                    binary.items[start.index].name.clone(),
                    Some(Node::Item(start.index)),
                );
                if !totals.complete() {
                    row.rest = " (may reach more)".to_string();
                }
                answer.rows.push(row);
            }
        } else if self.unreadable.is_empty() {
            answer
                .lead
                .push(format!("elfq found no place where {file} starts."));
        }

        // The marks in the table, of the starts that it shows.
        let shown: Vec<&Totals> = rows
            .iter()
            .filter_map(|row| match *row {
                StartRow::Start(start) => Some(&self.starts[start].totals),
                StartRow::More(..) => None,
            })
            .collect();
        let unfollowed = shown.iter().any(|totals| totals.unfollowed);
        let unreadable = shown.iter().any(|totals| totals.unreadable);
        let marked = match (unfollowed, unreadable) {
            (true, false) => Some("calls or jumps that elfq can't follow"),
            (false, true) => Some("code that elfq can't read"),
            (true, true) => {
                Some("calls or jumps that elfq can't follow, or code that it can't read")
            }
            (false, false) => None,
        };
        if let Some(marked) = marked {
            answer.notes.push(format!(
                "The starts marked (may reach more) reach {marked}; reach START names them."
            ));
        }
        if !self.unreadable.is_empty() {
            answer.notes.push(self.unreadable_text(path, all));
        }
        if binary.kind == FileKind::SharedLibrary {
            answer.notes.push(
                "elfq doesn't count a shared library's exported functions as starting points yet."
                    .to_string(),
            );
        }
        answer
    }

    /// The rows of the table: every start with `all`; otherwise `main`, the
    /// entry point and the handlers, and of the other starts, those that
    /// reach the most bytes, up to [`text::shown`] rows in all, with a row
    /// such as "650 more" after the starts of each kind that it leaves out.
    /// Of starts that reach as many bytes, the first is kept.
    fn rows(&self, all: bool) -> Vec<StartRow> {
        let is_other =
            |start: &Start| !matches!(start.root, None | Some(RootKind::Entry | RootKind::Handler));
        let mut others: Vec<usize> = (0..self.starts.len())
            .filter(|&start| is_other(&self.starts[start]))
            .collect();
        let room = text::shown(all).saturating_sub(self.starts.len() - others.len());
        others.sort_by_key(|&start| Reverse(self.starts[start].totals.bytes()));
        let left_out: HashSet<usize> = others[text::shown_count(others.len(), room)..]
            .iter()
            .copied()
            .collect();
        let mut rows = Vec::new();
        let mut first = 0;
        for kind in self.starts.chunk_by(|a, b| a.root == b.root) {
            let starts = first..first + kind.len();
            first += kind.len();
            let hidden = starts
                .clone()
                .filter(|start| left_out.contains(start))
                .count();
            // A row for one start takes no more room than "1 more" would.
            let hide = |start: &usize| hidden > 1 && left_out.contains(start);
            rows.extend(starts.filter(|start| !hide(start)).map(StartRow::Start));
            if let Some(root) = kind[0].root
                && hidden > 1
            {
                rows.push(StartRow::More(root, hidden));
            }
        }
        rows
    }

    /// Names the roots whose end elfq doesn't know, which the table leaves
    /// out, as "_init and _fini are starts too, but elfq doesn't know where
    /// they end, ...", or when they are the only roots, "_start is where
    /// FILE starts, ...".
    fn unreadable_text(&self, path: &Path, all: bool) -> String {
        let binary = self.binary;
        let names: Vec<&str> = self
            .unreadable
            .iter()
            .map(|&index| binary.items[index].name.as_str())
            .collect();
        let one = names.len() == 1;
        let what = match (self.starts.is_empty(), one) {
            (true, true) => format!("is where {} starts", path.display()),
            (true, false) => format!("are where {} starts", path.display()),
            (false, true) => "is a start too".to_string(),
            (false, false) => "are starts too".to_string(),
        };
        let (it, ends, reaches) = if one {
            ("it", "ends", "reaches")
        } else {
            ("they", "end", "reach")
        };
        format!(
            "{} {what}, but elfq doesn't know where {it} {ends}, so it can't count what {it} \
             {reaches}.",
            text::names_text(&names, all)
        )
    }

    fn write_json(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let item = |index| json::Item::new(binary, Node::Item(index));
        let answer = StartsJson {
            starts: self
                .starts
                .iter()
                .map(|start| StartJson {
                    item: item(start.index),
                    root: start.root.map(|kind| json::root_field(binary, kind)),
                    exceptions: start.exception_names(binary),
                    code_bytes: start.totals.code,
                    data_bytes: start.totals.data,
                    blocks: self
                        .blocks
                        .iter()
                        .zip(&start.totals.blocks)
                        .map(|(block, &(code_bytes, data_bytes))| BlockBytesJson {
                            range: json::Range::new(block.range),
                            code_bytes,
                            data_bytes,
                        })
                        .collect(),
                    complete: start.totals.complete(),
                })
                .collect(),
            unreadable: self.unreadable.iter().map(|&index| item(index)).collect(),
        };
        json::write(&answer, out)
    }
}

impl Start {
    /// Why it is a start, for the WHY column: "main function"; the
    /// exceptions it handles, as in "NMI, HardFault and 6 more", with
    /// "(entry point)" after them for the reset handler; or why it is a
    /// root, as the HOW column of `why` says it.
    pub(super) fn why(&self, binary: &Binary) -> String {
        let names = self.exception_names(binary);
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let exceptions = text::column_names(&names);
        match self.root {
            None => "main function".to_string(),
            Some(RootKind::Entry) if names.is_empty() => "entry point".to_string(),
            Some(RootKind::Entry) => format!("{exceptions} (entry point)"),
            Some(RootKind::Handler) => exceptions,
            Some(kind) => text::root_column(binary, kind),
        }
    }

    /// The names of the exceptions it handles, in the vector table's order
    /// (see [`exception_name`]), but not the reserved entries that list it,
    /// since the processor takes none of them: "reserved" only for a
    /// handler that no other entry lists. None for a function that isn't a
    /// handler.
    pub(super) fn exception_names(&self, binary: &Binary) -> Vec<String> {
        let architecture = binary.arm_architecture;
        let names: Vec<String> = self
            .exceptions
            .iter()
            .filter(|&&number| !is_reserved(number, architecture))
            .map(|&number| exception_name(number, architecture))
            .collect();
        match self.exceptions.first() {
            Some(&number) if names.is_empty() => vec![exception_name(number, architecture)],
            _ => names,
        }
    }
}

/// What an item reaches, added up as `reach SYMBOL` counts it, but found
/// without the tree: one search through the graph that only adds up bytes,
/// which is quick enough to make for every start of a large program.
pub(super) struct Totals {
    /// The bytes of the functions, and of the data objects, among the item
    /// and what it reaches.
    pub code: u64,
    pub data: u64,
    /// The same, in each of the blocks of memory that [`Totals::find`] was
    /// given.
    pub blocks: Vec<(u64, u64)>,
    /// How many items other than itself it reaches.
    pub reached: usize,
    /// Whether the item or what it reaches has a call or jump that elfq
    /// can't follow, but for the jump in a PLT's header to the dynamic
    /// loader.
    pub unfollowed: bool,
    /// Whether it is, or reaches, a function whose end elfq doesn't know,
    /// so that it can't read its code.
    pub unreadable: bool,
}

impl Totals {
    /// Adds up what the item at `index` reaches in `graph`, the file's
    /// reference graph, in all and in each of `blocks`.
    pub(super) fn find(
        binary: &Binary,
        graph: &Graph,
        index: usize,
        blocks: &[MemoryBlock],
    ) -> Totals {
        let mut totals = Totals {
            code: 0,
            data: 0,
            blocks: vec![(0, 0); blocks.len()],
            reached: 0,
            unfollowed: false,
            unreadable: false,
        };
        let mut seen = vec![false; binary.items.len()];
        seen[index] = true;
        let mut next = vec![index];
        while let Some(index) = next.pop() {
            let item = &binary.items[index];
            let (code, data) = match item.kind {
                ItemKind::Function => (item.size, 0),
                ItemKind::Data => (0, item.size),
            };
            totals.code = u64::saturating_add(totals.code, code);
            totals.data = u64::saturating_add(totals.data, data);
            if let Some(block) = blocks
                .iter()
                .position(|block| block.range.contains(item.address))
            {
                let (block_code, block_data) = &mut totals.blocks[block];
                *block_code = u64::saturating_add(*block_code, code);
                *block_data = u64::saturating_add(*block_data, data);
            }
            if item.kind == ItemKind::Function && item.size == 0 {
                totals.unreadable = true;
            }
            if !graph.unresolved_in(index).is_empty() && !is_plt_header(graph, Node::Item(index)) {
                totals.unfollowed = true;
            }
            // Imports use nothing in the file, and take no bytes in it.
            for reference in graph.references_from(index) {
                if let Node::Item(to) = reference.to
                    && !seen[to]
                {
                    seen[to] = true;
                    totals.reached += 1;
                    next.push(to);
                }
            }
        }
        totals
    }

    /// Whether elfq can follow every call and jump in the item and what it
    /// reaches, and knows where each function among them ends, as
    /// `complete` in `reach SYMBOL --json` says.
    pub(super) fn complete(&self) -> bool {
        !self.unfollowed && !self.unreadable
    }

    /// The bytes of code and data together.
    fn bytes(&self) -> u64 {
        u64::saturating_add(self.code, self.data)
    }
}

/// Whether `node` is a PLT's header, whose one jump goes to the dynamic
/// loader.
fn is_plt_header(graph: &Graph, node: Node) -> bool {
    graph
        .root(node)
        .is_some_and(|root| root.kind == RootKind::PltHeader)
}

/// How the text lists the use that reaches `node`, from `via`: as the call
/// to a veneer when `via` is the veneer's jump to its target and the veneer
/// is reached by a call.
fn used(paths: &ShortestPaths, veneers: &Veneers, node: Node, via: Reference) -> Use {
    if veneers.target(via.from) == Some(VeneerTarget::Node(node))
        && let Some(call) = paths.via(Node::Item(via.from))
        && call.kind.is_call()
    {
        return Use {
            by: call.from,
            kind: call.kind,
            veneer: Some(via.from),
        };
    }
    Use {
        by: via.from,
        kind: via.kind,
        veneer: None,
    }
}

/// `jump` as the text lists it: when it is a veneer's jump to code outside
/// every item, and a call reaches the veneer, as that call, through the
/// veneer.
fn through_veneer(paths: &ShortestPaths, veneers: &Veneers, jump: Unresolved) -> Unfollowed {
    if let Some(target) = jump.target
        && veneers.target(jump.from) == Some(VeneerTarget::Outside(target))
        && let Some(call) = paths.via(Node::Item(jump.from))
        && call.kind.is_call()
    {
        return Unfollowed {
            unresolved: Unresolved {
                from: call.from,
                site: call.site,
                is_call: call.kind == ReferenceKind::Call,
                target: Some(target),
            },
            veneer: Some(jump.from),
        };
    }
    Unfollowed {
        unresolved: jump,
        veneer: None,
    }
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

/// "function" or "data object".
fn kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Function => "function",
        ItemKind::Data => "data object",
    }
}

/// The answer when SYMBOL, `name`, reaches nothing but perhaps itself:
/// how it uses itself, if it does ("calls" or "uses"), or else what it is,
/// then its calls and jumps that elfq can't follow, as `callees` says them,
/// with the first few sites, or with `all`, every one. `plt_header` is
/// whether SYMBOL is a PLT's header, whose jump is left out.
fn nothing_reached_lines(
    name: &str,
    self_use: Option<&str>,
    plt_header: bool,
    unresolved: &[Unresolved],
    all: bool,
) -> Vec<String> {
    let pointers = !unresolved.is_empty();
    let mut lines = match self_use {
        Some(verb) if pointers => vec![format!("{name} {verb} itself.")],
        Some(verb) => vec![format!("{name} {verb} only itself.")],
        None if pointers => vec![],
        None if plt_header => {
            vec![format!(
                "{name} is the PLT header, which jumps only to the dynamic loader."
            )]
        }
        None => vec![format!("{name} doesn't call or use anything.")],
    };
    let listed = self_use.is_some();
    lines.extend(callees::unresolved_lines(name, unresolved, listed, all));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Root;

    #[test]
    fn what_reaches_nothing_says_how_it_uses_itself() {
        let lines = |self_use, plt_header, unresolved: &[Unresolved]| {
            nothing_reached_lines("f", self_use, plt_header, unresolved, false)
        };
        let pointer = [Unresolved {
            from: 0,
            site: 0x10,
            is_call: true,
            target: None,
        }];
        assert_eq!(lines(None, false, &[]), ["f doesn't call or use anything."]);
        assert_eq!(lines(Some("calls"), false, &[]), ["f calls only itself."]);
        assert_eq!(lines(Some("uses"), false, &[]), ["f uses only itself."]);
        assert_eq!(
            lines(Some("calls"), false, &pointer),
            [
                "f calls itself.",
                "Also 1 call through a pointer, at 0x10, which elfq can't follow.",
            ]
        );
        assert_eq!(
            lines(None, false, &pointer),
            ["f makes 1 call through a pointer, at 0x10, which elfq can't follow."]
        );
        assert_eq!(
            lines(None, true, &[]),
            ["f is the PLT header, which jumps only to the dynamic loader."]
        );
    }

    /// c-x86_64.elf, whose functions the tests below make starts of.
    fn sample() -> Binary {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-x86_64.elf");
        Binary::load(&path).unwrap()
    }

    fn index(binary: &Binary, name: &str) -> usize {
        binary
            .items
            .iter()
            .position(|item| item.name == name)
            .unwrap()
    }

    fn section(binary: &Binary, name: &str) -> usize {
        binary
            .sections
            .iter()
            .position(|section| section.name == name)
            .unwrap()
    }

    /// A start at `name`, a root of `kind` or `main`, that reaches `code`
    /// bytes of code, and calls or jumps that elfq can't follow if
    /// `unfollowed`, or code that it can't read if `unreadable`.
    fn start(
        binary: &Binary,
        name: &str,
        root: Option<RootKind>,
        code: u64,
        (unfollowed, unreadable): (bool, bool),
    ) -> Start {
        Start {
            index: index(binary, name),
            root,
            exceptions: Vec::new(),
            totals: Totals {
                code,
                data: 0,
                blocks: vec![(code, 0)],
                reached: 1,
                unfollowed,
                unreadable,
            },
        }
    }

    fn text(starts: &Starts, all: bool) -> String {
        let mut out = Vec::new();
        starts
            .answer(Path::new("app.elf"), all)
            .write(&mut out)
            .unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn the_note_says_why_starts_may_reach_more() {
        let binary = sample();
        let note = |marks: &[(bool, bool)]| {
            let starts = Starts {
                binary: &binary,
                starts: marks
                    .iter()
                    .map(|&marks| start(&binary, "main", None, 10, marks))
                    .collect(),
                unreadable: Vec::new(),
                blocks: memory_blocks(&binary.sections),
            };
            text(&starts, false).lines().last().unwrap().to_string()
        };
        assert_eq!(
            note(&[(true, false)]),
            "The starts marked (may reach more) reach calls or jumps that elfq can't follow; \
             reach START names them."
        );
        assert_eq!(
            note(&[(false, true)]),
            "The starts marked (may reach more) reach code that elfq can't read; reach START \
             names them."
        );
        assert_eq!(
            note(&[(true, false), (false, true)]),
            "The starts marked (may reach more) reach calls or jumps that elfq can't follow, or \
             code that it can't read; reach START names them."
        );
        assert_eq!(note(&[(false, false)]), "  10     0  main function  main");
    }

    #[test]
    fn starts_whose_end_elfq_doesnt_know_are_named_below_the_table() {
        let binary = sample();
        let starts = |starts, unreadable: &[&str]| Starts {
            binary: &binary,
            starts,
            unreadable: unreadable.iter().map(|name| index(&binary, name)).collect(),
            blocks: memory_blocks(&binary.sections),
        };
        let main = || vec![start(&binary, "main", None, 10, (false, false))];
        assert_eq!(
            text(&starts(main(), &["_init", "_fini"]), false),
            "\
Where app.elf starts, and the bytes of code and data that each reaches:

CODE  DATA  WHY            START
  10     0  main function  main

_init and _fini are starts too, but elfq doesn't know where they end, so it can't count what they reach.
"
        );
        assert_eq!(
            text(&starts(main(), &["_init"]), false).lines().last(),
            Some(
                "_init is a start too, but elfq doesn't know where it ends, so it can't count \
                 what it reaches."
            )
        );
        // Without a table, the sentence is the answer.
        assert_eq!(
            text(&starts(Vec::new(), &["_init"]), false),
            "_init is where app.elf starts, but elfq doesn't know where it ends, so it can't \
             count what it reaches.\n"
        );
        assert_eq!(
            text(&starts(Vec::new(), &[]), false),
            "elfq found no place where app.elf starts.\n"
        );
    }

    #[test]
    fn the_table_of_starts_shows_the_starts_that_reach_the_most() {
        let binary = sample();
        let init_array = RootKind::ListedIn(section(&binary, ".init_array"));
        let fini_array = RootKind::ListedIn(section(&binary, ".fini_array"));
        // The entry point, then 31 functions that .init_array lists, each
        // reaching more than the one before, and 2 that .fini_array lists.
        let mut all = vec![start(
            &binary,
            "_start",
            Some(RootKind::Entry),
            1,
            (false, false),
        )];
        for code in 0..31 {
            all.push(start(
                &binary,
                "frame_dummy",
                Some(init_array),
                code,
                (false, false),
            ));
        }
        for code in [100, 0] {
            let name = "__do_global_dtors_aux";
            all.push(start(&binary, name, Some(fini_array), code, (false, false)));
        }
        let starts = Starts {
            binary: &binary,
            starts: all,
            unreadable: Vec::new(),
            blocks: memory_blocks(&binary.sections),
        };
        // The entry point is always shown. Of the other 33 starts, 29 more
        // fit: of .init_array's, the 28 that reach the most, and of
        // .fini_array's, the one that reaches 100 bytes, and the other too,
        // since a row of its own takes no more room than "1 more".
        let rows: Vec<String> = starts
            .rows(false)
            .iter()
            .map(|row| match *row {
                StartRow::Start(start) => starts.starts[start].totals.code.to_string(),
                StartRow::More(kind, count) => format!("{count} more {kind:?}"),
            })
            .collect();
        let mut expected: Vec<String> = ["1"].into_iter().map(String::from).collect();
        expected.extend((3..31).map(|code: u64| code.to_string()));
        expected.push(format!("3 more {init_array:?}"));
        expected.extend(["100".to_string(), "0".to_string()]);
        assert_eq!(rows, expected);
        assert!(text(&starts, false).contains("  listed in .init_array  3 more\n"));
        assert_eq!(starts.rows(true).len(), 34);
    }

    #[test]
    fn a_main_that_is_a_root_is_listed_as_one() {
        let binary = sample();
        let mut graph = Graph::build(&binary);
        let names = Names::new(&binary);
        // As if the unwinder called main.
        let main = Node::Item(index(&binary, "main"));
        graph.roots.push(Root {
            node: main,
            kind: RootKind::Personality,
        });
        graph.roots.sort_by_key(|root| root.node);
        let starts = starts(&binary, &graph, &names);
        let listed: Vec<(&str, String)> = starts
            .starts
            .iter()
            .map(|start| (binary.items[start.index].name.as_str(), start.why(&binary)))
            .collect();
        assert_eq!(
            listed,
            [
                ("_start", "entry point".to_string()),
                ("frame_dummy", "listed in .init_array".to_string()),
                ("__do_global_dtors_aux", "listed in .fini_array".to_string()),
                ("main", "used by the unwinder".to_string()),
            ]
        );
    }
}
