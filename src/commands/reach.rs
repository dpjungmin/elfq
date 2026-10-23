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

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::symbol::Names;
use super::{Outcome, callees, json, ranges, text};
use crate::binary::{AddressRange, Binary, ItemKind, MemoryBlock, memory_blocks};
use crate::cli::Format;
use crate::graph::{
    Boundary, Graph, Node, Reference, ReferenceKind, RootKind, ShortestPaths, Unfollowed,
    Unresolved, VeneerTarget, Veneers,
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
    let reach = Reach::find(binary, symbol, to, all)?;
    match format {
        Format::Json => reach.write_json(out)?,
        Format::Text => reach.write_text(false, out)?,
        Format::Tree => reach.write_text(true, out)?,
    }
    Ok(reach.outcome())
}

/// What SYMBOL reaches.
struct Reach<'a> {
    binary: &'a Binary,
    graph: Graph,
    names: Names<'a>,
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

impl<'a> Reach<'a> {
    /// Finds SYMBOL's item and everything it reaches, keeping only the calls
    /// and jumps that elfq can't follow that may go to `to`, the ranges of
    /// `--to`, if there are any.
    fn find(binary: &'a Binary, symbol: &str, to: &[AddressRange], all: bool) -> Result<Reach<'a>> {
        super::check_code_is_readable(binary)?;
        let names = Names::new(binary);
        let symbol = names.find(symbol)?;
        let index = super::readable_item(binary, symbol.node)?;
        let lead_name = symbol.lead_name(binary);
        let graph = Graph::build(binary);
        let code_outside = ranges::code_outside(&graph);
        for &range in to {
            ranges::check_to_range(binary, range, &code_outside)?;
        }
        let start = Node::Item(index);
        let paths = ShortestPaths::from(binary, &graph, start);
        let veneers = Veneers::new(binary, &graph);
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
        let is_plt_header = |node| {
            graph
                .root(node)
                .is_some_and(|root| root.kind == RootKind::PltHeader)
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
        let called = called(&graph, &steps);
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
            if is_plt_header(step.node) {
                continue;
            }
            for &jump in graph.unresolved_in(index) {
                unresolved.push(jump);
                unfollowed.push(through_veneer(&paths, &veneers, jump));
            }
        }
        let plt_header = is_plt_header(start);
        if !to.is_empty() {
            let through_pointer = !Boundary::new(binary, &graph, to)
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
        let binary = self.binary;
        let name = &self.lead_name;
        let reached = &self.steps[1..];
        let outside: Vec<Unfollowed> = self
            .unfollowed
            .iter()
            .filter(|unfollowed| unfollowed.unresolved.target.is_some())
            .copied()
            .collect();
        if reached.is_empty() && outside.is_empty() {
            // Any calls and jumps elfq can't follow are in SYMBOL, through
            // pointers.
            for line in nothing_reached_lines(
                name,
                self.self_use,
                self.plt_header,
                &self.unresolved,
                self.all,
            ) {
                writeln!(out, "{line}")?;
            }
            return Ok(());
        }

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
        writeln!(out, "{lead}:")?;

        if self.blocks.len() > 1 {
            writeln!(out)?;
            self.write_memory(out)?;
        }
        writeln!(out)?;
        if tree {
            Tree::new(self).write(out)?;
        } else {
            self.write_rows(&outside, out)?;
        }

        let pointers: Vec<&Unresolved> = self
            .unfollowed
            .iter()
            .map(|unfollowed| &unfollowed.unresolved)
            .filter(|unresolved| unresolved.target.is_none())
            .collect();
        if !pointers.is_empty() || !self.unreadable.is_empty() {
            writeln!(out)?;
        }
        if !pointers.is_empty() {
            writeln!(out, "{}", self.pointer_note(&pointers))?;
        }
        if !self.unreadable.is_empty() {
            writeln!(
                out,
                "{}",
                text::unreadable_text(binary, &self.unreadable, self.all)
            )?;
        }
        Ok(())
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
    /// [`Reach::write_rows`]. Every row is shown, with the sites of the use
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
            table.row(with_at(self.symbol_row(), &[]));
        }
        for (unfollowed, sites) in ranges::group_unfollowed(&self.unfollowed) {
            let sites = self.unfollowed_sites(&unfollowed, sites);
            table.row(with_at(self.unfollowed_row(&unfollowed), &sites));
        }
        for step in self.in_row_order(reached.to_vec()) {
            table.row(with_at(self.row(step), &self.use_sites(step)));
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

    /// Writes the bytes of code and data in each block of memory.
    fn write_memory(&self, out: &mut dyn Write) -> Result<()> {
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
        table.write(out)?;
        Ok(())
    }

    /// Writes SYMBOL's row, then a row for each item it reaches, by block,
    /// functions then data objects, each largest first; then the calls and
    /// jumps to code outside every item in `outside`, and the imports.
    /// Unless the answer shows every row, only the 30 largest items are
    /// shown, and a row such as "9 more data objects in
    /// 0x20000000..0x20000450" sums up the rest of a block's.
    fn write_rows(&self, outside: &[Unfollowed], out: &mut dyn Write) -> Result<()> {
        let mut table = Table::new(&[
            ("ADDRESS", Align::Left),
            ("SIZE", Align::Right),
            ("HOW", Align::Left),
            ("ITEM", Align::Left),
        ]);
        table.row(self.symbol_row());
        let reached = &self.steps[1..];
        let shown = self.shown(reached);
        let steps = self.in_row_order(reached.iter().collect());
        let imports = steps.partition_point(|step| matches!(step.node, Node::Item(_)));
        let (items, imports) = steps.split_at(imports);
        for steps in items.chunk_by(|a, b| self.group(a) == self.group(b)) {
            self.add_group(steps, &shown, &mut table);
        }
        for (unfollowed, _) in ranges::group_unfollowed(outside) {
            table.row(self.unfollowed_row(&unfollowed));
        }
        if !imports.is_empty() {
            self.add_group(imports, &shown, &mut table);
        }
        table.write(out)?;
        Ok(())
    }

    /// The cells of SYMBOL's row.
    fn symbol_row(&self) -> Vec<String> {
        let item = &self.binary.items[self.index];
        vec![
            format!("{:#x}", item.address),
            text::size_text(item),
            String::new(),
            item.name.clone(),
        ]
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

    /// The cells of the row of a call or jump that elfq can't follow, as
    /// `callees` words it, but for the AT column: to code outside every
    /// item, at its address, or through a pointer.
    fn unfollowed_row(&self, unfollowed: &Unfollowed) -> Vec<String> {
        let unresolved = &unfollowed.unresolved;
        let (address, what) = match unresolved.target {
            Some(target) => (format!("{target:#x}"), "(outside any function)"),
            None => (String::new(), "(through a pointer)"),
        };
        vec![
            address,
            String::new(),
            ranges::unfollowed_how(unfollowed).to_string(),
            format!("{what}, used by {}", self.user_name(unresolved.from)),
        ]
    }

    /// Adds the rows of `steps`, one group of the table, such as a block's
    /// functions: those in `shown`, and a row that sums up the rest. One
    /// that would be left out on its own is shown too, since its row takes
    /// no more room than the sum's.
    fn add_group(&self, steps: &[&Step], shown: &HashSet<Node>, table: &mut Table) {
        let binary = self.binary;
        let hidden: Vec<&Step> = steps
            .iter()
            .copied()
            .filter(|step| !shown.contains(&step.node))
            .collect();
        for step in steps {
            if hidden.len() < 2 || shown.contains(&step.node) {
                table.row(self.row(step));
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
        table.row(vec![
            String::new(),
            size,
            String::new(),
            format!("{} more {what}{place}", hidden.len()),
        ]);
    }

    /// The cells of an item's or import's row. With `--to`, the row is
    /// written as `callees --to` writes it, with the whole name of the item
    /// that uses it, and no mark for a function reached through a pointer,
    /// which HOW shows.
    fn row(&self, step: &Step) -> Vec<String> {
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
        vec![
            address,
            size,
            text::how_via(binary, step.node, used.kind, used.veneer).to_string(),
            format!(
                "{}{mark}, used by {}",
                step.node.name(binary),
                self.user_name(used.by)
            ),
        ]
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
        for unresolved in pointers {
            if !callers.contains(&unresolved.from) {
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
    reach: &'r Reach<'a>,
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
    fn new(reach: &'r Reach<'a>) -> Tree<'r, 'a> {
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
}
