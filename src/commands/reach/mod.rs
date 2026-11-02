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
//! - A *step* is SYMBOL, or an item or import that it reaches, at its
//!   place in the tree ([`Step`]). Its id is its place in the search's
//!   order ([`StepId`]), and SYMBOL's is 0. A step *has a row* when the
//!   text shows it on a line of its own; a row such as "12 more" sums up
//!   the others.
//! - A step's *chain* is the shortest chain of references from SYMBOL to
//!   it. Its *via* is the last reference on the chain, from its *parent*.
//! - The *use* that a row lists ([`Use`]) is that reference, but for the
//!   target of a veneer, which is listed as called through the veneer.
//! - A call or jump that elfq *can't follow* ([`CantFollow`]) goes through
//!   a pointer, or to code outside every item. It is *found* as the code
//!   makes it, and *listed* as the rows list it: a veneer's jump as the
//!   call to the veneer.
//! - The *counted* steps are those whose bytes the answer adds up: every
//!   one, or with `--to`, those in its ranges ([`Reach::counted`]).
//! - The steps *in the answer* are those that the tree and the JSON have:
//!   every one, or with `--to`, the chains to what is listed
//!   ([`Reach::in_answer`]).
//! - The *largest* items are those that get rows without `--all`: the 30
//!   that take the most bytes ([`Text::largest`]).
//!
//! Here, `mod.rs` holds what the search finds, with what every form of the
//! answer asks of it, and [`Text`], the view that words it for people;
//! `find.rs` the search, [`Reach::find`]; `rows.rs` the text without
//! `--to`, by block of memory; `to.rs` the text with `--to`; `tree.rs` the
//! tree of `--tree`; `json.rs` the JSON, from the facts alone; and
//! `starts.rs` the answer without SYMBOL.
//!
//! [`Reached`]: crate::graph::Reached

mod find;
mod json;
mod rows;
mod starts;
mod to;
mod tree;

pub(super) use starts::starts;

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};

use super::checks::{check_code_is_readable, readable_item};
#[cfg(feature = "tui")]
use super::flat_answer::FlatTab;
use super::json::section_names;
use super::kept::Kept;
use super::symbol::Names;
use super::text::{self, CommandLine};
use super::{Outcome, range_check};
use crate::binary::{AddressRange, Binary, CodeAndData, MemoryBlock};
use crate::cli::Format;
use crate::graph::{Graph, Node, Reference, ReferenceKind, Unfollowed, Unresolved};
use crate::render::Align;

/// Answers `elfq FILE reach SYMBOL`. The text lists SYMBOL and the 30
/// largest items it reaches, or with `all`, every one, by block, or with
/// `--tree`, as a tree. `to`, the ranges of `--to`, limits the answer to
/// what SYMBOL reaches in them, with every row, and then the answer is
/// [`Outcome::Crossed`] when it lists something.
pub(super) fn run(
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
    check_to(binary, &graph, to)?;
    let reach = Kept::new(Reach::find(binary, &graph, index, to));
    match format {
        Format::Json => reach.write_json(out)?,
        Format::Text | Format::Tree => {
            let lead_name = symbol.lead_name(binary);
            reach.text(&names, &lead_name, all, format).write(out)?;
        }
    }
    Ok(reach.outcome())
}

/// Refuses a range of `--to`, `to`, that holds nothing that code in
/// `graph` can use, as `callees --to` refuses it.
fn check_to(binary: &Binary, graph: &Graph, to: &[AddressRange]) -> Result<()> {
    let code_outside = graph.code_outside_items();
    for &range in to {
        range_check::check_to_range(binary, range, &code_outside)?;
    }
    Ok(())
}

/// Answers `elfq FILE reach` without SYMBOL, for the file at `path`: where
/// the program starts, and the bytes of code and data that each start
/// reaches, in each block of memory when there are several. `all` lists
/// every start, and names every function that elfq can't read. `to`, the
/// ranges of `--to`, `range`, that of `--range`, and `--tree` need SYMBOL,
/// and are refused.
pub(super) fn run_starts(
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

/// What `reach SYMBOL` writes as text about the item at `index`, as tab 4
/// of browse shows it, whose first line names it `lead_name` (see
/// [`Symbol::lead_name`]), with `graph`, the file's reference graph, and
/// `names` for the short names of the rows: with every row and name, as
/// with `--all`; its notes as the command writes them without `--all`; and
/// whether the command needs `--all` to print it all.
///
/// [`Symbol::lead_name`]: super::symbol::Symbol::lead_name
#[cfg(feature = "tui")]
pub(super) fn tab<'a>(
    binary: &'a Binary,
    graph: &Graph,
    names: &Names<'a>,
    index: usize,
    lead_name: &str,
) -> FlatTab {
    let reach = Reach::find(binary, graph, index, &[]);
    let answer = |all| reach.text(names, lead_name, all, Format::Text).answer();
    FlatTab::new(answer(true), answer(false))
}

/// What `reach SYMBOL` prints as text about the item at `index`, whose
/// first line names it `lead_name`, with `graph` and `names`, which the
/// caller built: for browse's test that tab 4 shows what it prints.
#[cfg(all(test, feature = "tui"))]
pub(super) fn text_output<'a>(
    binary: &'a Binary,
    graph: &Graph,
    names: &Names<'a>,
    index: usize,
    lead_name: &str,
    all: bool,
) -> String {
    let reach = Reach::find(binary, graph, index, &[]);
    let mut out = Vec::new();
    let text = reach.text(names, lead_name, all, Format::Text);
    text.write(&mut out).expect("writing to memory");
    String::from_utf8(out).expect("elfq writes UTF-8")
}

/// The columns of the rows by block and of the tree, and with `at`, of the
/// rows of `--to`, which have AT before ITEM.
fn columns(at: bool) -> Vec<(String, Align)> {
    let mut columns = vec![
        ("ADDRESS", Align::Left),
        ("SIZE", Align::Right),
        ("HOW", Align::Left),
        ("ITEM", Align::Left),
    ];
    if at {
        columns.insert(3, ("AT", Align::Left));
    }
    (columns.into_iter())
        .map(|(header, align)| (header.to_string(), align))
        .collect()
}

/// What SYMBOL reaches: the facts that the search finds, which the JSON
/// has, and which [`Text`] words.
struct Reach<'g, 'a> {
    binary: &'a Binary,
    graph: &'g Graph,
    /// SYMBOL's item, as an index into [`Binary::items`].
    symbol: usize,
    /// How SYMBOL uses itself, as a recursive function calls itself:
    /// "calls" or "uses", or `None` when it doesn't. No row shows it.
    self_use: Option<&'static str>,
    /// Whether SYMBOL is a PLT's header, whose one jump goes to the dynamic
    /// loader.
    plt_header: bool,
    /// SYMBOL and what it reaches, as the steps of the tree: SYMBOL first,
    /// then depth first, each item's children in the order it first uses
    /// them.
    steps: Vec<Step>,
    /// The calls and jumps in what SYMBOL reaches that elfq can't follow,
    /// but for the jump in a PLT's header, which goes to the dynamic
    /// loader, and with `--to`, for those that can't go to its ranges. In
    /// the order of the steps they are in, then by site.
    cant_follow: Vec<CantFollow>,
    /// The functions SYMBOL reaches whose end elfq doesn't know, in the
    /// order of the steps.
    unreadable: Vec<usize>,
    /// The file's blocks of memory, as the overview lists them.
    blocks: Vec<MemoryBlock>,
    /// The ranges of `--to`: when there are any, the answer is what SYMBOL
    /// reaches in them.
    to: Vec<AddressRange>,
}

/// How the text answer words what [`Reach`] found: the names it gives the
/// items, and how much of it it shows. The JSON has every fact, and no
/// words.
struct Text<'r, 'a> {
    reach: &'r Reach<'r, 'a>,
    /// The names that find each item, for the short names of the rows.
    names: &'r Names<'a>,
    /// SYMBOL's name as the answer's first line gives it (see
    /// [`Symbol::lead_name`]).
    ///
    /// [`Symbol::lead_name`]: super::symbol::Symbol::lead_name
    lead_name: &'r str,
    /// Whether the table shows every row, and the sentences every name.
    all: bool,
    /// Whether the answer draws a tree, with `--tree`, in place of its
    /// rows.
    tree: bool,
}

/// A step's place in [`Reach::steps`]: 0 is SYMBOL's.
type StepId = usize;

/// SYMBOL, or an item or import that it reaches, at its place in the tree.
struct Step {
    node: Node,
    /// The last link of its chain from SYMBOL: `None` exactly for SYMBOL,
    /// the first step.
    link: Option<Link>,
    /// The block of memory that holds it, as an index into
    /// [`Reach::blocks`].
    block: Option<usize>,
    /// Whether it is a function that nothing SYMBOL reaches calls (see
    /// `ItemJson::through_pointer` in `json.rs`).
    through_pointer: bool,
}

/// The last link of the chain from SYMBOL to a step other than SYMBOL's:
/// the item before it on the chain, and how that item uses it.
struct Link {
    /// The step of the item that uses it on its chain from SYMBOL.
    parent: StepId,
    /// The reference on the chain that reaches it, from its parent.
    via: Reference,
    /// Where its parent uses it as `via` does.
    sites: Vec<u64>,
    /// How the text lists the use that reaches it.
    used: Use,
}

impl Step {
    /// The step of the item that uses it on its chain from SYMBOL, or
    /// `None` for SYMBOL.
    fn parent(&self) -> Option<StepId> {
        self.link.as_ref().map(|link| link.parent)
    }

    /// The last link of its chain from SYMBOL, for a step that the caller
    /// knows isn't SYMBOL's, such as one that has a row of its own.
    fn link(&self) -> &Link {
        self.link.as_ref().expect("only SYMBOL's step has no link")
    }
}

/// What some steps take in one block of memory (see
/// [`Reach::blocks_reached`]).
struct BlockReached<'a> {
    /// The bytes of code and of data that they take there.
    bytes: CodeAndData,
    /// The names of the sections that hold them, in address order.
    sections: Vec<&'a str>,
}

/// A call or jump in what SYMBOL reaches that elfq can't follow.
#[derive(Clone, Copy)]
struct CantFollow {
    /// As the code makes it, which the tree and the JSON list.
    found: Unresolved,
    /// As the rows list it: a veneer's jump to code outside every item as
    /// the call to the veneer, as `callees` lists it for ranges.
    listed: Unfollowed,
}

/// How the text lists the use of an item that reaches it: the use on its
/// chain, or for the target of a veneer, the call to the veneer.
struct Use {
    /// The item that uses it, as an index into [`Binary::items`].
    user: usize,
    kind: ReferenceKind,
    /// The veneer or PLT stub that the call goes through.
    veneer: Option<usize>,
}

impl<'g, 'a> Reach<'g, 'a> {
    /// The text answer's view of it, which names SYMBOL `lead_name` in its
    /// first line and the other items as `names` finds them, with `all`,
    /// shows every row and name, and as `format` says, draws a tree or
    /// lists rows.
    fn text<'r>(
        &'r self,
        names: &'r Names<'a>,
        lead_name: &'r str,
        all: bool,
        format: Format,
    ) -> Text<'r, 'a> {
        Text {
            reach: self,
            names,
            lead_name,
            all,
            tree: format == Format::Tree,
        }
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
            || !self.cant_follow.is_empty()
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

    /// What `steps` take in each block, in the order of [`Reach::blocks`].
    fn blocks_reached(&self, steps: &[&Step]) -> Vec<BlockReached<'a>> {
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
                BlockReached {
                    bytes,
                    sections: section_names(self.binary, &sections),
                }
            })
            .collect()
    }

    /// Which steps the answer has: every one, or with `--to`, SYMBOL, the
    /// steps in its ranges, those with calls and jumps that elfq can't
    /// follow that may go there, and every step on their chains from
    /// SYMBOL.
    fn in_answer(&self) -> Vec<bool> {
        let with_cant_follow: HashSet<Node> = self
            .cant_follow
            .iter()
            .map(|cant| Node::Item(cant.found.from))
            .collect();
        let mut in_answer = vec![self.to.is_empty(); self.steps.len()];
        in_answer[0] = true;
        // A step comes after its parent, so its children come first.
        for (id, step) in self.steps.iter().enumerate().rev() {
            if in_answer[id] || self.in_to(step) || with_cant_follow.contains(&step.node) {
                in_answer[id] = true;
                if let Some(parent) = step.parent() {
                    in_answer[parent] = true;
                }
            }
        }
        in_answer
    }

    /// The calls and jumps to code outside every item, which the rows list.
    fn outside(&self) -> Vec<Unfollowed> {
        self.listed()
            .into_iter()
            .filter(|listed| listed.unresolved.target.is_some())
            .collect()
    }

    /// The calls and jumps that elfq can't follow, as the rows list them.
    fn listed(&self) -> Vec<Unfollowed> {
        self.cant_follow.iter().map(|cant| cant.listed).collect()
    }

    /// Whether SYMBOL reaches nothing that a row would list, so that the
    /// answer is a few sentences.
    fn reaches_nothing(&self) -> bool {
        self.steps.len() == 1 && self.outside().is_empty()
    }
}

impl Text<'_, '_> {
    /// The calls and jumps that elfq can't follow, as the answer counts
    /// them: as the code makes them for a tree, which has the veneers that
    /// the rows by block step over, and as the rows list them otherwise.
    fn counted_cant_follow(&self) -> Vec<&Unresolved> {
        let cant_follow = self.reach.cant_follow.iter();
        match self.tree {
            true => cant_follow.map(|cant| &cant.found).collect(),
            false => cant_follow.map(|cant| &cant.listed.unresolved).collect(),
        }
    }

    /// Whether the rows are worded as `callees --to` words them, as they
    /// are with `--to`: each names the item that uses it by its whole name,
    /// and a function reached through a pointer gets no mark, since HOW
    /// shows it.
    fn like_callees_to(&self) -> bool {
        !self.reach.to.is_empty()
    }

    /// Writes the answer: the items by block, or with `--to`, those in its
    /// ranges; or with `--tree`, as a tree drawn with branches.
    fn write(&self, out: &mut dyn Write) -> Result<()> {
        let answer = match self.reach.to[..] {
            [] => self.answer(),
            _ => self.to_answer(),
        };
        answer.write(out)
    }
}

/// What the first line calls the items in `nodes` that SYMBOL reaches,
/// after "the", as in "main and the 12 functions it reaches take": the
/// kind of one item, or as [`text::count_text`] counts them.
fn reached_words(binary: &Binary, nodes: &[Node]) -> String {
    match nodes {
        [Node::Item(index)] => text::kind_name(binary.items[*index].kind).to_string(),
        _ => text::count_text(binary, nodes),
    }
}
