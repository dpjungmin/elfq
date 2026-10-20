//! Roots: the places where the program starts running. Every path through
//! the graph begins at one, and the program never uses an item that no path
//! reaches, or uses it in a way elfq can't see.
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

use crate::binary::{Binary, FileKind, STARTUP_ARRAYS, Source};

use super::{Node, item_starting_at, pointer_target};

/// A place where the program starts running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Root {
    /// The item where the program starts, or an import that a startup or
    /// exit array's relocation names.
    pub node: Node,
    /// Why it is a root: the first reason `find` comes to, when there are
    /// several.
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
            // From the array's first byte, aligned or not, as the program
            // reads it.
            let slots = binary.words(section.address, section.address + section.size);
            add(
                slots
                    .filter_map(|(slot, value)| stored_pointer(binary, slot, value))
                    .collect(),
                RootKind::ListedIn(index),
            );
        }
    }
    // PLT headers are named after their section; the stubs after what they
    // jump to.
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
    let (table, handlers) = vector_table(binary);
    add(table, RootKind::VectorTable);
    add(handlers, RootKind::Handler);
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
/// says, or in a file linked to run at one address, the `value` the file
/// holds there.
fn stored_pointer(binary: &Binary, slot: u64, value: u64) -> Option<Node> {
    if let Some(node) = pointer_target(binary, slot) {
        return Some(node);
    }
    if binary.kind != FileKind::Executable {
        return None;
    }
    item_starting_at(binary, value).map(Node::Item)
}

/// The items holding the Cortex-M vector table, and the handlers it lists.
fn vector_table(binary: &Binary) -> (Vec<Node>, Vec<Node>) {
    let Some(table) = &binary.vector_table else {
        return (Vec::new(), Vec::new());
    };
    // The table can be several items, one for each part of it.
    let slots = (table.address..table.address + table.size).step_by(binary.pointer_size as usize);
    let items = slots.filter_map(|slot| binary.item_at(slot).map(Node::Item));
    // A handler can start at address 0, as in a Cortex-M7's ITCM, where
    // `item_starting_at` would see a null pointer.
    let handlers = table.handlers.iter().filter_map(|&handler| {
        let index = binary.item_at(handler)?;
        (binary.items[index].address == handler).then_some(Node::Item(index))
    });
    (items.collect(), handlers.collect())
}
