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
//! Scanning repeats until nothing new turns up. Then the Thumb addresses
//! stored in data start functions too, such as a table of handlers or a
//! vtable, and their code is followed in turn. A word in data can also be
//! a number that only looks like such an address, as the string `"=\0\0\0"`
//! does where code starts at address 0. So an address in data starts a
//! function only if that cuts nothing in two: no jump crosses it, and the
//! code on either side still finds the constants it loads.
//!
//! Each function runs to the next start, or the end of its section, less
//! the padding before it, so its size is an estimate. It is named after its
//! address, as in `fn_0x8000040`. Code that nothing calls or points to
//! stays part of the function before it.

use std::collections::BTreeSet;

use super::{Binary, Item, ItemKind, Source, code_section, unnamed_data};
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
    let stored = stored_addresses(binary);
    loop {
        let mut found: BTreeSet<u64> = ranges(binary, &starts)
            .flat_map(|(start, end)| targets(binary, start, end))
            .filter(|target| !starts.contains(target))
            .collect();
        // Addresses in data are checked against the functions found so
        // far, so only once the code leads nowhere new.
        if found.is_empty() {
            found = stored
                .iter()
                .copied()
                .filter(|&address| {
                    !starts.contains(&address) && splits_cleanly(binary, &starts, address)
                })
                .collect();
        }
        if found.is_empty() {
            break;
        }
        starts.extend(found);
    }

    for (start, end) in ranges(binary, &starts) {
        // Without the padding that aligns the next function.
        let size = binary
            .bytes(start, end - start)
            .map_or(end - start, |code| {
                arch::code_size(binary.architecture, code, start)
            });
        if let Some(item) = items
            .iter_mut()
            .find(|item| item.kind == ItemKind::Function && item.address == start)
        {
            item.size = size;
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
            size,
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

/// The Thumb code addresses stored in data, word by word.
fn stored_addresses(binary: &Binary) -> Vec<u64> {
    let size = binary.pointer_size;
    let mut addresses = Vec::new();
    for section in &binary.sections {
        if !section.allocated || !unnamed_data::holds_data(section) {
            continue;
        }
        let mut slot = section.address.next_multiple_of(size);
        while slot + size <= section.address + section.size {
            if let Some(value) = binary.pointer_at(slot)
                && value & 1 == 1
                && code_section(&binary.sections, value & !1).is_some()
            {
                addresses.push(value & !1);
            }
            slot += size;
        }
    }
    addresses
}

/// Whether a function can start at `address` without cutting the code
/// around it in two, as far as the functions in `starts` tell: no jump
/// crosses it, except to where a function starts, and scanning the code on
/// either side finds what scanning it whole does, such as each constant it
/// loads.
fn splits_cleanly(binary: &Binary, starts: &BTreeSet<u64>, address: u64) -> bool {
    let Some(index) = code_section(&binary.sections, address) else {
        return false;
    };
    let section = &binary.sections[index];
    let section_end = section.address + section.size;
    let start = starts.range(section.address..address).next_back();
    let start = start.copied().unwrap_or(section.address);
    let end = starts.range(address..section_end).next();
    let end = end.copied().unwrap_or(section_end);
    let scan = |from: u64, to: u64| {
        let code = binary.bytes(from, to - from).unwrap_or_default();
        arch::scan(binary.architecture, code, from)
    };
    let whole = scan(start, end);
    let mut halves = scan(start, address);
    halves.extend(scan(address, end));
    let crosses = whole.iter().any(|reference| match reference.kind {
        CodeReferenceKind::Jump(target) | CodeReferenceKind::TailJump(target) => {
            (reference.site < address) != (target < address)
                && (start..end).contains(&target)
                && target != address
                && !starts.contains(&target)
        }
        _ => false,
    });
    whole == halves && !crosses
}
