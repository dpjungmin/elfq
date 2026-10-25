//! The reference graph: which items use which, and how.
//!
//! Its nodes are a binary's items and imports. Its edges are references,
//! found in two places:
//!
//! - the code of every function, which the `arch` module scans;
//! - pointers stored in data. In a position-independent file the dynamic
//!   relocations list them. In a file linked to run at one address, such as
//!   firmware, they are found by reading data items word by word: a word
//!   equal to where an item starts is taken to point at it.
//!
//! [`Graph::build`] works out which item each address belongs to. Calls and
//! jumps it can't follow, such as a call through a function pointer, are kept
//! in [`Graph::unresolved`] so that answers can say what they don't know.
//!
//! [`Boundary`] tells which references cross the boundary of some ranges
//! of memory (see `boundary.rs`).
//!
//! [`Veneers`] tells where each of a linker's veneers jumps to. A veneer is
//! code that the linker adds where a call is too far for the call
//! instruction: the call goes to the veneer, which jumps on to the target
//! (see `veneers.rs`).
//!
//! [`Graph::roots`] are where the program starts running (see `roots.rs`).
//! From them, [`DominatorTree`] works out which items are reachable only
//! through which (see `dominators.rs`), and [`ShortestPaths`] finds the
//! shortest chain of references to each item (see `paths.rs`).
//! [`ShortestPaths::from`] searches from one item instead, and finds the
//! shortest chain to each item that it uses, directly or not.

mod boundary;
mod dominators;
mod paths;
mod roots;
mod veneers;

pub use boundary::{Boundary, Crossing, Unfollowed};
pub use dominators::DominatorTree;
pub use paths::ShortestPaths;
pub use roots::{Root, RootKind, listed_in};
pub use veneers::{VeneerTarget, Veneers};

use std::collections::HashSet;

use crate::arch::{self, CodeReference, CodeReferenceKind};
use crate::binary::{Binary, FileKind, ItemKind, Relocation, RelocationTarget};

/// A node of the graph: an item in the file, or an import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Node {
    /// An index into [`Binary::items`].
    Item(usize),
    /// An index into [`Binary::imports`].
    Import(usize),
}

/// One item's use of another item or of an import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reference {
    /// The item that makes the reference, as an index into [`Binary::items`].
    pub from: usize,
    /// The item or import that `from` uses.
    pub to: Node,
    /// How `from` uses it.
    pub kind: ReferenceKind,
    /// Where it is: the address of the instruction, or of the stored pointer.
    pub site: u64,
}

/// How one item uses another. Only `Call` and `TailCall` are calls; an
/// `Address` or a `Pointer` to a function means that code may call it
/// through a pointer, which elfq can't follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReferenceKind {
    /// A direct call, a call through a pointer that the loader fills in,
    /// such as a global offset table entry, whose target is known, or on
    /// Thumb, a call through a register that the code put the target's
    /// address in, as a long call does.
    Call,
    /// A jump to another function, directly or through such a pointer,
    /// which then returns straight to the caller's caller.
    TailCall,
    /// Code taking an item's address, as when passing a function or a
    /// buffer to another function, or loading it from a global offset table
    /// entry.
    Address,
    /// Code reading or writing an item's bytes, such as a global variable,
    /// or a function pointer variable that it calls through.
    Access,
    /// A pointer stored in data, such as an entry in a table of functions.
    Pointer,
}

impl ReferenceKind {
    /// Whether it is a call or a tail call, rather than a use of an item's
    /// address or bytes.
    pub fn is_call(self) -> bool {
        matches!(self, ReferenceKind::Call | ReferenceKind::TailCall)
    }
}

/// A call or jump that the graph can't follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unresolved {
    /// The function it is in, as an index into [`Binary::items`].
    pub from: usize,
    /// The instruction's address.
    pub site: u64,
    /// Whether it is a call rather than a jump.
    pub is_call: bool,
    /// The target, when the code says where it is but no item contains it.
    /// `None` when the target is only known at run time, as in a call
    /// through a function pointer.
    pub target: Option<u64>,
}

impl Unresolved {
    /// Whether it goes through a pointer set at run time, rather than to
    /// code outside every item.
    pub fn is_indirect(&self) -> bool {
        self.target.is_none()
    }
}

/// The reference graph of a binary: its references, the calls and jumps it
/// can't follow, and its roots. Its nodes are indices into the binary's
/// items and imports, so it goes with the [`Binary`] it was built from.
#[derive(Debug, Default)]
pub struct Graph {
    /// Every reference, sorted by the item that makes it, then by site.
    pub references: Vec<Reference>,
    /// Calls and jumps the graph can't follow, sorted the same way.
    pub unresolved: Vec<Unresolved>,
    /// Where the program starts running, such as the entry point; see
    /// `roots.rs`. Sorted by node.
    pub roots: Vec<Root>,
}

impl Graph {
    /// Builds the graph. For instruction sets that the `arch` module can't
    /// read yet, it only has the pointers stored in data.
    pub fn build(binary: &Binary) -> Graph {
        let mut graph = Graph::default();
        let scan_code = arch::is_supported(binary.architecture, binary.thumb);
        for (index, item) in binary.items.iter().enumerate() {
            match item.kind {
                ItemKind::Function if scan_code => graph.add_code_references(binary, index),
                ItemKind::Data if binary.kind == FileKind::Executable => {
                    graph.add_pointers_in_data(binary, index);
                }
                _ => {}
            }
        }
        for relocation in &binary.relocations {
            graph.add_stored_pointer(binary, relocation);
        }
        graph
            .references
            .sort_by_key(|reference| (reference.from, reference.site));
        graph
            .unresolved
            .sort_by_key(|unresolved| (unresolved.from, unresolved.site));
        graph.roots = roots::find(binary);
        graph
    }

    /// The references that `item` makes, in the order they appear in it.
    pub fn references_from(&self, item: usize) -> &[Reference] {
        let start = self.references.partition_point(|r| r.from < item);
        let end = self.references.partition_point(|r| r.from <= item);
        &self.references[start..end]
    }

    /// The references to `node`, sorted by the item that makes them.
    pub fn references_to(&self, node: Node) -> impl Iterator<Item = &Reference> {
        self.references.iter().filter(move |r| r.to == node)
    }

    /// Why the program starts at `node`, if it is one of the roots.
    pub fn root(&self, node: Node) -> Option<&Root> {
        let index = self
            .roots
            .binary_search_by_key(&node, |root| root.node)
            .ok()?;
        Some(&self.roots[index])
    }

    /// The calls and jumps in `item` that the graph can't follow.
    pub fn unresolved_in(&self, item: usize) -> &[Unresolved] {
        let start = self.unresolved.partition_point(|u| u.from < item);
        let end = self.unresolved.partition_point(|u| u.from <= item);
        &self.unresolved[start..end]
    }

    fn add_code_references(&mut self, binary: &Binary, function: usize) {
        let item = &binary.items[function];
        let references = binary.code_references(item);
        let bounds = if item.contains(binary.entry) {
            section_bounds(binary, references)
        } else {
            HashSet::new()
        };
        for &reference in references {
            if let CodeReferenceKind::Address(address) = reference.kind
                && bounds.contains(&address)
            {
                continue;
            }
            self.add_code_reference(binary, function, reference);
        }
    }

    fn add_code_reference(&mut self, binary: &Binary, from: usize, reference: CodeReference) {
        let function = &binary.items[from];
        let site = reference.site;
        let unresolved = |is_call, target| Unresolved {
            from,
            site,
            is_call,
            target,
        };
        match reference.kind {
            CodeReferenceKind::Call(target) => match binary.item_at(target) {
                // A call into the function's own middle, not a recursive
                // call: nothing outside the function is involved.
                Some(to) if to == from && target != function.address => {}
                Some(to) => self.add(from, Node::Item(to), ReferenceKind::Call, site),
                None => self.unresolved.push(unresolved(true, Some(target))),
            },
            CodeReferenceKind::Jump(target) | CodeReferenceKind::TailJump(target) => {
                // Most jumps stay inside the function: loops and branches.
                if function.contains(target) {
                    return;
                }
                match binary.item_at(target) {
                    Some(to) => self.add(from, Node::Item(to), ReferenceKind::TailCall, site),
                    None => self.unresolved.push(unresolved(false, Some(target))),
                }
            }
            CodeReferenceKind::CallThrough(pointer) | CodeReferenceKind::JumpThrough(pointer) => {
                let is_call = matches!(reference.kind, CodeReferenceKind::CallThrough(_));
                if let Some(variable) = binary.item_at(pointer) {
                    // A function pointer variable: it can hold anything.
                    if variable != from {
                        self.add(from, Node::Item(variable), ReferenceKind::Access, site);
                    }
                    self.unresolved.push(unresolved(is_call, None));
                } else if let Some(to) = pointer_target(binary, pointer) {
                    // A pointer the loader fills in, such as a global offset
                    // table entry: the target is known.
                    let kind = if is_call {
                        ReferenceKind::Call
                    } else {
                        ReferenceKind::TailCall
                    };
                    self.add(from, to, kind, site);
                } else {
                    self.unresolved.push(unresolved(is_call, None));
                }
            }
            CodeReferenceKind::IndirectCall => self.unresolved.push(unresolved(true, None)),
            CodeReferenceKind::IndirectJump => self.unresolved.push(unresolved(false, None)),
            CodeReferenceKind::Address(address) => {
                for to in items_used_at(binary, address) {
                    if to != from {
                        self.add(from, Node::Item(to), ReferenceKind::Address, site);
                    }
                }
            }
            CodeReferenceKind::Memory(address) => {
                let used = items_used_at(binary, address);
                if used.is_empty() {
                    // Not in any item: maybe a global offset table entry,
                    // which code reads to get the address the loader stored
                    // there.
                    if let Some(to) = pointer_target(binary, address) {
                        self.add(from, to, ReferenceKind::Address, site);
                    }
                }
                for to in used {
                    if to != from {
                        self.add(from, Node::Item(to), ReferenceKind::Access, site);
                    }
                }
            }
        }
    }

    /// Adds a reference for a pointer stored inside an item, such as a table
    /// of functions. Pointers outside items, such as global offset table
    /// entries, are followed from the code that loads them instead.
    fn add_stored_pointer(&mut self, binary: &Binary, relocation: &Relocation) {
        let Some(from) = binary.item_at(relocation.address) else {
            return;
        };
        let Some(to) = target_node(binary, relocation.target) else {
            return;
        };
        if to != Node::Item(from) {
            self.add(from, to, ReferenceKind::Pointer, relocation.address);
        }
    }

    /// Adds a reference for each word in a data item that holds where
    /// another item starts, such as an entry in a table of functions. Only
    /// for files linked to run at one address, whose pointers are stored as
    /// they are.
    ///
    /// A number that happens to equal an item's address is taken for a
    /// pointer too. That is rare, because addresses are large numbers, and
    /// requiring the exact start of an item keeps it rarer.
    fn add_pointers_in_data(&mut self, binary: &Binary, from: usize) {
        let item = &binary.items[from];
        let words = binary.aligned_words(item.address, item.address + item.size);
        for (site, value) in words {
            if let Some(to) = item_starting_at(binary, value)
                && to != from
            {
                self.add(from, Node::Item(to), ReferenceKind::Pointer, site);
            }
        }
    }

    fn add(&mut self, from: usize, to: Node, kind: ReferenceKind, site: u64) {
        self.references.push(Reference {
            from,
            to,
            kind,
            site,
        });
    }
}

impl Node {
    /// The item's name, or the import's, as the output shows it.
    pub fn name(self, binary: &Binary) -> &str {
        match self {
            Node::Item(index) => &binary.items[index].name,
            Node::Import(index) => &binary.imports[index],
        }
    }

    /// The item's own size, or 0 for an import, whose code is in another
    /// file.
    pub fn size(self, binary: &Binary) -> u64 {
        match self {
            Node::Item(index) => binary.items[index].size,
            Node::Import(_) => 0,
        }
    }
}

/// The addresses where a section starts and ends, for each section whose
/// start and end addresses the entry point's code both loads. Startup code
/// does that to copy or clear a whole section, such as `.data` or `.bss`,
/// through symbols that the linker script defines. The section's first item
/// starts at the same address, but the code means the section, not the item.
///
/// Other code can load the same addresses to use the items there, such as
/// a function that reads the first variable in `.data` and the first one in
/// `.bss`, which starts where `.data` ends. So only the entry point gets
/// this treatment, and only for addresses it loads, not ones it reads or
/// writes.
fn section_bounds(binary: &Binary, references: &[CodeReference]) -> HashSet<u64> {
    let used: HashSet<u64> = references
        .iter()
        .filter_map(|reference| match reference.kind {
            CodeReferenceKind::Address(address) => Some(address),
            _ => None,
        })
        .collect();
    binary
        .sections
        .iter()
        .filter(|section| section.allocated && section.size > 0)
        .map(|section| (section.address, section.address + section.size))
        .filter(|(start, end)| used.contains(start) && used.contains(end))
        .flat_map(|(start, end)| [start, end])
        .collect()
}

/// A node's position in one list of `items` items followed by the imports,
/// for analyses that keep a value per node.
fn index(items: usize, node: Node) -> usize {
    match node {
        Node::Item(i) => i,
        Node::Import(i) => items + i,
    }
}

/// The item that starts at `address`, as a pointer stored in data names
/// it. On Arm, a pointer to a Thumb function is its address plus 1.
pub fn item_starting_at(binary: &Binary, address: u64) -> Option<usize> {
    let starts_at = |address| {
        let index = binary.item_at(address)?;
        (binary.items[index].address == address).then_some(index)
    };
    // Most words in data aren't pointers: those below the first item or
    // past the last item's start are passed over without a search.
    let first = binary.items.first()?.address;
    let last = binary.items.last()?.address;
    if address == 0 || address < first || address & !1 > last {
        return None;
    }
    starts_at(address).or_else(|| {
        let function = starts_at(address & !1).filter(|_| binary.thumb && address & 1 == 1)?;
        (binary.items[function].kind == ItemKind::Function).then_some(function)
    })
}

/// The items that code using `address` uses: the item there, or every
/// variable in a block of merged variables that starts there.
fn items_used_at(binary: &Binary, address: u64) -> Vec<usize> {
    let block = binary
        .merged_variables
        .iter()
        .find(|&&(start, _)| start == address);
    match block {
        Some(&(start, size)) => {
            let first = binary.items.partition_point(|item| item.address < start);
            (first..binary.items.len())
                .take_while(|&index| binary.items[index].address < start + size)
                .collect()
        }
        None => binary.item_at(address).into_iter().collect(),
    }
}

/// What the pointer at `address` points to, when the dynamic loader writes
/// it.
fn pointer_target(binary: &Binary, address: u64) -> Option<Node> {
    target_node(binary, binary.relocation_at(address)?.target)
}

fn target_node(binary: &Binary, target: RelocationTarget) -> Option<Node> {
    match target {
        RelocationTarget::Address(address) => binary.item_at(address).map(Node::Item),
        RelocationTarget::Import(import) => Some(Node::Import(import)),
    }
}
