//! Functions in stripped firmware, which has no symbol table or unwind table
//! to list them.
//!
//! elfq follows the code from where the processor starts running it: the
//! entry point and the handlers in the vector table. It scans each function
//! up to the next start it knows, and these start more functions:
//!
//! - what the code calls;
//! - where it jumps in the way that carries on in another function: a
//!   Thumb `b.w`, unless it goes back into the function, which is a loop,
//!   and a linker's veneer;
//! - the addresses of Thumb functions that it loads, which have bit 0 set,
//!   as when it passes a handler to another function.
//!
//! Scanning repeats until nothing new turns up. Each function then runs to
//! the next start, or the end of its section, so its size is an estimate.
//! It is named after its address, as in `fn_0x8000040`. Code that nothing
//! calls or points to stays part of the function before it.

use std::collections::BTreeSet;

use super::{Binary, Item, ItemKind, Source, code_section};
use crate::arch::{self, CodeReferenceKind};

/// Adds the functions that following the code finds, and gives the ones
/// already in `items`, such as the entry point, a size.
pub(super) fn add(binary: &Binary, items: &mut Vec<Item>) {
    let mut starts: BTreeSet<u64> = items
        .iter()
        .filter(|item| item.kind == ItemKind::Function)
        .map(|item| item.address)
        .collect();
    if let Some(table) = &binary.vector_table {
        starts.extend(&table.handlers);
    }
    loop {
        let found: BTreeSet<u64> = ranges(binary, &starts)
            .flat_map(|(start, end)| targets(binary, start, end))
            .filter(|target| !starts.contains(target))
            .collect();
        if found.is_empty() {
            break;
        }
        starts.extend(found);
    }

    for (start, end) in ranges(binary, &starts) {
        if let Some(item) = items
            .iter_mut()
            .find(|item| item.kind == ItemKind::Function && item.address == start)
        {
            item.size = end - start;
            item.size_estimated = true;
            continue;
        }
        let Some(section) = code_section(&binary.sections, start) else {
            continue;
        };
        items.push(Item {
            name: format!("fn_{start:#x}"),
            aliases: Vec::new(),
            kind: ItemKind::Function,
            address: start,
            size: end - start,
            size_estimated: true,
            section,
            source: Source::Reached,
        });
    }
}

/// Each function's start and end: the next start, or the end of its
/// section.
fn ranges<'a>(
    binary: &'a Binary,
    starts: &'a BTreeSet<u64>,
) -> impl Iterator<Item = (u64, u64)> + 'a {
    starts.iter().filter_map(|&start| {
        let section = &binary.sections[code_section(&binary.sections, start)?];
        let next = starts.range(start + 1..).next().copied();
        let end = section.address + section.size;
        Some((start, next.map_or(end, |next| next.min(end))))
    })
}

/// Where the code from `start` to `end` leads to other functions.
fn targets(binary: &Binary, start: u64, end: u64) -> Vec<u64> {
    let Some(code) = binary.bytes(start, end - start) else {
        return Vec::new();
    };
    arch::scan(binary.architecture, code, start)
        .into_iter()
        .filter_map(|reference| match reference.kind {
            CodeReferenceKind::Call(target) => Some(target),
            CodeReferenceKind::TailJump(target) if !(start..=reference.site).contains(&target) => {
                Some(target)
            }
            CodeReferenceKind::Address(address) if address & 1 == 1 => Some(address & !1),
            _ => None,
        })
        .filter(|&target| code_section(&binary.sections, target).is_some())
        .collect()
}
