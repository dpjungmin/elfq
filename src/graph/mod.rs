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
//! [`Graph::roots`] are where the program starts running (see `roots.rs`).
//! From them, [`DominatorTree`] works out which items are reachable only
//! through which (see `dominators.rs`), and [`ShortestPaths`] finds the
//! shortest chain of references to each item (see `paths.rs`).

mod dominators;
mod paths;
mod roots;

pub use dominators::DominatorTree;
pub use paths::ShortestPaths;
pub use roots::{Root, RootKind};

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
    pub to: Node,
    pub kind: ReferenceKind,
    /// Where it is: the address of the instruction, or of the stored pointer.
    pub site: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReferenceKind {
    /// A direct call.
    Call,
    /// A jump to another function, which then returns straight to the
    /// caller's caller.
    TailCall,
    /// Code taking an item's address, as when passing a function or a
    /// buffer to another function.
    Address,
    /// Code reading or writing an item's bytes, such as a global variable.
    Access,
    /// A pointer stored in data, such as an entry in a table of functions.
    Pointer,
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

    /// The calls and jumps in `item` that the graph can't follow.
    pub fn unresolved_in(&self, item: usize) -> &[Unresolved] {
        let start = self.unresolved.partition_point(|u| u.from < item);
        let end = self.unresolved.partition_point(|u| u.from <= item);
        &self.unresolved[start..end]
    }

    fn add_code_references(&mut self, binary: &Binary, function: usize) {
        let item = &binary.items[function];
        let Some(code) = binary.bytes(item.address, item.size) else {
            return;
        };
        for reference in arch::scan(binary.architecture, code, item.address) {
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
            CodeReferenceKind::Jump(target) => {
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
        let size = binary.pointer_size;
        // Pointers are aligned to their size.
        let mut site = item.address.next_multiple_of(size);
        while site + size <= item.address + item.size {
            if let Some(value) = binary.pointer_at(site)
                && let Some(to) = item_starting_at(binary, value)
                && to != from
            {
                self.add(from, Node::Item(to), ReferenceKind::Pointer, site);
            }
            site += size;
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
    pub fn name(self, binary: &Binary) -> &str {
        match self {
            Node::Item(index) => &binary.items[index].name,
            Node::Import(index) => &binary.imports[index],
        }
    }
}

/// A node's position in one list of `items` items followed by the imports,
/// for analyses that keep a value per node.
fn index(items: usize, node: Node) -> usize {
    match node {
        Node::Item(i) => i,
        Node::Import(i) => items + i,
    }
}

/// The item that starts at `address`. On Arm, a pointer to a Thumb function
/// is its address plus 1.
fn item_starting_at(binary: &Binary, address: u64) -> Option<usize> {
    let starts_at = |address| {
        let index = binary.item_at(address)?;
        (binary.items[index].address == address).then_some(index)
    };
    if address == 0 {
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
