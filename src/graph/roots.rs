//! Roots: the places where the program starts running. Every path through
//! the graph begins at one, and an item that no path reaches is dead code.
//!
//! - the entry point;
//! - the functions in the startup and exit arrays, such as `.init_array`,
//!   which the C runtime calls before and after `main`, and the code in the
//!   `.init` and `.fini` sections, which the dynamic loader runs;
//! - the header of the procedure linkage table, `.plt`, which the dynamic
//!   loader uses to look up imported functions;
//! - personality routines, which the unwinder calls while it handles a
//!   panic or an exception;
//! - notes, such as `__abi_tag`, which the system reads;
//! - on Cortex-M, the vector table, from which the processor takes the
//!   addresses of the reset and interrupt handlers.
//!
//! Exported symbols, which other files can use, are not roots yet, so a
//! shared library's graph is mostly unreachable.

use object::SectionKind;

use crate::binary::{Binary, FileKind, Source};

use super::{Node, item_starting_at, pointer_target};

/// A place where the program starts running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Root {
    pub node: Node,
    pub kind: RootKind,
}

/// Why something is a root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    /// The entry point.
    Entry,
    /// In `.init`, `.fini`, a startup or exit array such as `.init_array`, or
    /// a note. Holds the section's index in [`Binary::sections`].
    InSection(usize),
    /// A function that a startup or exit array points to. Holds the array's
    /// index in [`Binary::sections`].
    ListedIn(usize),
    /// The header of the procedure linkage table.
    PltHeader,
    /// A personality routine.
    Personality,
    /// The Cortex-M vector table.
    VectorTable,
    /// A handler that the vector table lists.
    Handler,
}

/// Arrays of pointers to the functions that the C runtime calls at startup
/// and exit.
const STARTUP_ARRAYS: [&str; 5] = [
    ".preinit_array",
    ".init_array",
    ".fini_array",
    ".ctors",
    ".dtors",
];
/// Code that the dynamic loader runs at startup and exit.
const STARTUP_CODE: [&str; 2] = [".init", ".fini"];

/// Returns the roots, sorted by node. A node that is a root for several
/// reasons appears once, with the reason found first, so the entry point is
/// always [`RootKind::Entry`].
pub(super) fn find(binary: &Binary) -> Vec<Root> {
    let mut roots = Vec::new();
    let mut add = |nodes: Vec<Node>, kind| {
        roots.extend(nodes.into_iter().map(|node| Root { node, kind }));
    };
    let entry = binary.item_at(binary.entry).map(Node::Item);
    add(entry.into_iter().collect(), RootKind::Entry);
    for (index, section) in binary.sections.iter().enumerate() {
        if !section.allocated {
            continue;
        }
        let name = section.name.as_str();
        if STARTUP_CODE.contains(&name)
            || STARTUP_ARRAYS.contains(&name)
            || section.kind == SectionKind::Note
        {
            add(items_in_section(binary, index), RootKind::InSection(index));
        }
        if STARTUP_ARRAYS.contains(&name) {
            let end = section.address + section.size;
            let mut slot = section.address;
            while slot + binary.pointer_size <= end {
                add(
                    stored_pointer(binary, slot).into_iter().collect(),
                    RootKind::ListedIn(index),
                );
                slot += binary.pointer_size;
            }
        }
    }
    // PLT headers are named after their section; the stubs after imports.
    let plt_headers = (0..binary.items.len()).filter(|&index| {
        let item = &binary.items[index];
        item.source == Source::Plt && item.name == binary.sections[item.section].name
    });
    add(plt_headers.map(Node::Item).collect(), RootKind::PltHeader);
    let personalities = binary.used_by_unwinder.iter();
    add(
        personalities
            .filter_map(|&address| binary.item_at(address).map(Node::Item))
            .collect(),
        RootKind::Personality,
    );
    if binary.thumb {
        let (table, handlers) = vector_table(binary);
        add(table, RootKind::VectorTable);
        add(handlers, RootKind::Handler);
    }
    // A stable sort keeps each node's first reason first.
    roots.sort_by_key(|root| root.node);
    roots.dedup_by_key(|root| root.node);
    roots
}

/// The items in a section.
fn items_in_section(binary: &Binary, section: usize) -> Vec<Node> {
    (0..binary.items.len())
        .filter(|&index| binary.items[index].section == section)
        .map(Node::Item)
        .collect()
}

/// What the pointer stored at `slot` points to: the dynamic relocation
/// says, or in a file linked to run at one address, the pointer itself.
fn stored_pointer(binary: &Binary, slot: u64) -> Option<Node> {
    if let Some(node) = pointer_target(binary, slot) {
        return Some(node);
    }
    if binary.kind != FileKind::Executable {
        return None;
    }
    item_starting_at(binary, binary.pointer_at(slot)?).map(Node::Item)
}

/// The items holding the Cortex-M vector table, and the handlers it lists.
/// The table starts
/// with the initial stack pointer, followed by the address of the reset
/// handler, which is the entry point (plus 1, for Thumb), then the other
/// handlers. Reserved entries are 0.
///
/// The table is found by its shape rather than by a section name, since
/// linker scripts name it differently: the first word in data that holds
/// the entry point's Thumb address is taken for the reset entry.
fn vector_table(binary: &Binary) -> (Vec<Node>, Vec<Node>) {
    let size = binary.pointer_size;
    let Some(reset) = binary.sections.iter().find_map(|section| {
        if !section.allocated || section.kind == SectionKind::Text {
            return None;
        }
        let mut slot = section.address.next_multiple_of(size) + size;
        while slot + size <= section.address + section.size {
            if binary.pointer_at(slot) == Some(binary.entry | 1) {
                return Some(slot);
            }
            slot += size;
        }
        None
    }) else {
        return (Vec::new(), Vec::new());
    };

    // The items holding the table, which can be several, and the handlers
    // its entries point to, until an entry is neither 0 nor a handler.
    let mut table: Vec<Node> = binary
        .item_at(reset - size)
        .map(Node::Item)
        .into_iter()
        .collect();
    let mut handlers = Vec::new();
    let mut slot = reset;
    while let Some(value) = binary.pointer_at(slot) {
        let handler = item_starting_at(binary, value).filter(|_| value & 1 == 1);
        if value != 0 && handler.is_none() {
            break;
        }
        table.extend(binary.item_at(slot).map(Node::Item));
        handlers.extend(handler.map(Node::Item));
        slot += size;
    }
    (table, handlers)
}
