//! Functions in stripped firmware, which has no symbol table or unwind table
//! to list them.
//!
//! elfq follows the code from where the processor or the C runtime starts
//! running it: the entry point, the handlers in the vector table, and the
//! functions in arrays such as `.init_array`. It scans each function up to
//! the next start it knows, and these start more functions:
//!
//! - what the code calls;
//! - where it jumps in the way that carries on in another function: a
//!   Thumb `b.w`, unless it lands where the function has been or where its
//!   other jumps so far reach, since loops and long functions use `b.w`
//!   inside themselves too; and a linker's veneer.
//!
//! Scanning repeats until nothing new turns up. Then the Thumb addresses
//! that the code loads or data holds start functions too, as for a handler
//! passed to another function, a table of handlers or a vtable, and their
//! code is followed in turn. Such a word can also be a number that only
//! looks like an address, so it starts a function only where the code
//! before it ends, with a return, a jump or a call that may never return,
//! outside the padding between functions; where no jump crosses it; and
//! where the code on either side still finds the constants it loads.
//!
//! Each function runs to the next start, or the end of its section, less
//! the padding after it, so its size is an estimate. It is named after its
//! address, as in `fn_0x8000040`. What this misses:
//!
//! - Code that nothing calls or points to stays part of the function
//!   before it, and so does a function that code reaches only with a
//!   16-bit `b`, since compilers use it for tail calls to nearby functions
//!   as well as for jumps inside one.
//! - A `bl` to a place inside a function starts a function there, as in
//!   Cortex-M0 code, which jumps that way where `b` can't reach; and so
//!   does a `b.w` past everything the function has reached so far, as in
//!   long unoptimized functions.

use std::collections::BTreeSet;

use super::{Binary, Item, ItemKind, STARTUP_ARRAYS, Source, code_section, unnamed_data};
use crate::arch::{self, CodeReferenceKind, Ending};

/// Adds the functions that following the code finds, and gives the ones
/// already in `items`, such as the entry point, a size.
pub(super) fn add(binary: &Binary, items: &mut Vec<Item>) {
    let mut known: Vec<u64> = items
        .iter()
        .filter(|item| item.kind == ItemKind::Function)
        .map(|item| item.address)
        .collect();
    if let Some(table) = &binary.vector_table {
        known.extend(&table.handlers);
    }
    let arrays = binary
        .sections
        .iter()
        .filter(|section| section.allocated && STARTUP_ARRAYS.contains(&section.name.as_str()));
    known.extend(arrays.flat_map(|section| {
        thumb_addresses(binary, section.address, section.address + section.size)
    }));
    let starts = follow(binary, known.into_iter().collect());

    for (start, end) in functions(binary, &starts) {
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

/// Follows the code from `starts` until it leads nowhere new, and returns
/// the starts it knows then.
fn follow(binary: &Binary, mut starts: BTreeSet<u64>) -> BTreeSet<u64> {
    // Thumb addresses that may be numbers instead: those stored in data,
    // and those the code loads.
    let mut numbers = stored_addresses(binary);
    // A function's code is scanned again only when a new start ends it
    // sooner.
    let mut scanned = BTreeSet::new();
    loop {
        let mut found = BTreeSet::new();
        for (start, end) in functions(binary, &starts) {
            if scanned.insert((start, end)) {
                let leads = leads(binary, start, end);
                found.extend(leads.functions);
                numbers.extend(leads.thumb_addresses);
            }
        }
        found.retain(|address| !starts.contains(address));
        // They are checked against the functions found so far, so only once
        // the code leads nowhere new.
        if found.is_empty() {
            found = numbers
                .iter()
                .copied()
                .filter(|&address| {
                    !starts.contains(&address) && splits_cleanly(binary, &starts, address)
                })
                .collect();
        }
        if found.is_empty() {
            return starts;
        }
        starts.extend(found);
    }
}

/// Each function's start and end.
fn functions<'a>(
    binary: &'a Binary,
    starts: &'a BTreeSet<u64>,
) -> impl Iterator<Item = (u64, u64)> + 'a {
    starts.iter().filter_map(|&start| {
        let section = &binary.sections[code_section(&binary.sections, start)?];
        let end = section.address + section.size;
        let next = starts.range(start + 1..).next();
        Some((start, next.map_or(end, |&next| next.min(end))))
    })
}

/// What the code of one function leads to.
#[derive(Default)]
struct Leads {
    /// Where it calls or carries on in another function.
    functions: Vec<u64>,
    /// The Thumb addresses it loads, which may be numbers instead.
    thumb_addresses: Vec<u64>,
}

/// Where the code from `start` to `end` leads.
fn leads(binary: &Binary, start: u64, end: u64) -> Leads {
    let mut leads = Leads::default();
    let code = binary.bytes(start, end - start).unwrap_or_default();
    // How far the function's other jumps reach, so far.
    let mut reach = start;
    for reference in arch::scan(binary.architecture, code, start) {
        match reference.kind {
            CodeReferenceKind::Call(target) => leads.functions.push(target),
            CodeReferenceKind::Jump(target) if target < end => reach = reach.max(target),
            CodeReferenceKind::TailJump(target)
                if !(start..=reference.site.max(reach)).contains(&target) =>
            {
                leads.functions.push(target);
            }
            CodeReferenceKind::Address(address) if address & 1 == 1 => {
                leads.thumb_addresses.push(address & !1);
            }
            _ => {}
        }
    }
    let in_code = |address: &u64| code_section(&binary.sections, *address).is_some();
    leads.functions.retain(in_code);
    leads.thumb_addresses.retain(in_code);
    leads
}

/// The Thumb code addresses stored in data.
fn stored_addresses(binary: &Binary) -> BTreeSet<u64> {
    let sections = binary
        .sections
        .iter()
        .filter(|section| section.allocated && unnamed_data::holds_data(section));
    sections
        .flat_map(|section| {
            thumb_addresses(binary, section.address, section.address + section.size)
        })
        .collect()
}

/// The Thumb code addresses stored from `start` to `end`, word by word,
/// without their Thumb bit.
fn thumb_addresses(binary: &Binary, start: u64, end: u64) -> Vec<u64> {
    let size = binary.pointer_size;
    let mut addresses = Vec::new();
    let mut slot = start.next_multiple_of(size);
    while slot + size <= end {
        if let Some(value) = binary.pointer_at(slot)
            && value & 1 == 1
            && code_section(&binary.sections, value & !1).is_some()
        {
            addresses.push(value & !1);
        }
        slot += size;
    }
    addresses
}

/// Whether a function can start at `address` without cutting the code
/// before it in two, as far as `starts` tell: that code ends there, with an
/// instruction that doesn't run on into it, such as a return or a call that
/// may never return; and no jump crosses into it, except to where a
/// function starts. Scanning the code before it must also find what it
/// does with what follows, such as each constant it loads, and so must
/// scanning a function after it.
fn splits_cleanly(binary: &Binary, starts: &BTreeSet<u64>, address: u64) -> bool {
    let Some(index) = code_section(&binary.sections, address) else {
        return false;
    };
    let section = &binary.sections[index];
    let section_end = section.address + section.size;
    let before = starts.range(section.address..address).next_back();
    let start = before.map_or(section.address, |&start| start);
    let end = starts.range(address..section_end).next();
    let end = end.map_or(section_end, |&end| end);
    let bytes = |from: u64, to: u64| binary.bytes(from, to - from).unwrap_or_default();
    let scan = |from: u64, to: u64| arch::scan(binary.architecture, bytes(from, to), from);
    // Before the first function known in a section, there may be data.
    if before.is_some()
        && arch::ending(binary.architecture, bytes(start, address), start) == Ending::RunsOn
    {
        return false;
    }
    // Without the padding after the code, which can decode as a jump.
    let end = start + arch::code_size(binary.architecture, bytes(start, end), start);
    // A function starts after the padding before it.
    if address >= end || arch::leading_padding(binary.architecture, bytes(address, end)) > 0 {
        return false;
    }
    let whole = scan(start, end);
    let mut halves = scan(start, address);
    halves.extend(scan(address, end));
    let jumps_into = |reference: &arch::CodeReference, from: u64, to: u64| match reference.kind {
        CodeReferenceKind::Jump(target) | CodeReferenceKind::TailJump(target) => {
            (from..to).contains(&target) && !starts.contains(&target)
        }
        _ => false,
    };
    let crosses = whole.iter().any(|reference| {
        if reference.site < address {
            jumps_into(reference, address + 1, end)
        } else {
            jumps_into(reference, start, address)
        }
    });
    whole == halves && !crosses
}

#[cfg(test)]
mod tests {
    use object::{Architecture, SectionKind};

    use super::*;
    use crate::binary::{FileKind, Section};

    /// The functions found in firmware that runs `text` from its start at
    /// 0x1000 and has `rodata` at 0x2000, as (address, size).
    fn functions(text: &[u8], rodata: &[u8]) -> Vec<(u64, u64)> {
        let section = |name: &str, address, kind, offset, bytes: &[u8]| Section {
            name: name.to_string(),
            address,
            size: bytes.len() as u64,
            kind,
            allocated: true,
            file_range: Some((offset, bytes.len() as u64)),
        };
        let binary = Binary {
            architecture: Architecture::Arm,
            pointer_size: 4,
            kind: FileKind::Executable,
            entry: 0x1000,
            thumb: true,
            has_symbols: false,
            has_dwarf: false,
            sections: vec![
                section(".text", 0x1000, SectionKind::Text, 0, text),
                section(
                    ".rodata",
                    0x2000,
                    SectionKind::ReadOnlyData,
                    text.len() as u64,
                    rodata,
                ),
            ],
            items: Vec::new(),
            imports: Vec::new(),
            relocations: Vec::new(),
            used_by_unwinder: Vec::new(),
            merged_variables: Vec::new(),
            vector_table: None,
            warnings: Vec::new(),
            data: [text, rodata].concat(),
            big_endian: false,
        };
        let mut items = vec![Item {
            name: "entry".to_string(),
            aliases: Vec::new(),
            kind: ItemKind::Function,
            address: 0x1000,
            size: 0,
            size_estimated: false,
            section: 0,
            source: Source::EntryPoint,
        }];
        add(&binary, &mut items);
        let mut found: Vec<(u64, u64)> =
            items.iter().map(|item| (item.address, item.size)).collect();
        found.sort_unstable();
        found
    }

    #[test]
    fn a_b_w_that_stays_in_the_function_starts_nothing() {
        // push {r4, lr}; nop; b.w 0x1002, back into the function; pop.
        let looping = [0x10, 0xb5, 0x00, 0xbf, 0xff, 0xf7, 0xfd, 0xbf, 0x10, 0xbd];
        assert_eq!(functions(&looping, &[]), [(0x1000, 10)]);
        // cmp r0, #0; beq 0x100c; b.w 0x100a, which the beq reaches past, as
        // in a long function; movs r0, #1; movs r0, #2; bx lr.
        let long = [
            0x00, 0x28, 0x03, 0xd0, 0x00, 0xf0, 0x01, 0xb8, 0x01, 0x20, 0x02, 0x20, 0x70, 0x47,
        ];
        assert_eq!(functions(&long, &[]), [(0x1000, 14)]);
        // b.w 0x1006, past the function's end: a tail call. Then bx lr, and
        // the function it calls: movs r0, #1; bx lr.
        let tail_call = [0x00, 0xf0, 0x01, 0xb8, 0x70, 0x47, 0x01, 0x20, 0x70, 0x47];
        assert_eq!(functions(&tail_call, &[]), [(0x1000, 6), (0x1006, 4)]);
    }

    #[test]
    fn an_address_in_data_starts_a_function_where_the_code_before_ends() {
        // movs r0, #1; bx lr; movs r0, #2; bx lr. Only data points to 0x1004.
        let code = [0x01, 0x20, 0x70, 0x47, 0x02, 0x20, 0x70, 0x47];
        assert_eq!(
            functions(&code, &[0x05, 0x10, 0, 0]),
            [(0x1000, 4), (0x1004, 4)]
        );
        // movs r0, #1 runs on into 0x1002.
        assert_eq!(functions(&code, &[0x03, 0x10, 0, 0]), [(0x1000, 8)]);
        // movw r0, #0x1234 is one instruction of 4 bytes; bx lr.
        let movw = [0x41, 0xf2, 0x34, 0x20, 0x70, 0x47];
        assert_eq!(functions(&movw, &[0x03, 0x10, 0, 0]), [(0x1000, 6)]);
        // bx lr; nop, which pads; movs r0, #1; bx lr. A function starts
        // after the padding, not in it.
        let padded = [0x70, 0x47, 0x00, 0xbf, 0x01, 0x20, 0x70, 0x47];
        assert_eq!(functions(&padded, &[0x03, 0x10, 0, 0]), [(0x1000, 8)]);
        assert_eq!(
            functions(&padded, &[0x05, 0x10, 0, 0]),
            [(0x1000, 2), (0x1004, 4)]
        );
    }

    #[test]
    fn an_address_in_data_that_a_jump_crosses_starts_nothing() {
        // cmp r0, #0; beq 0x100a; bx lr; movs r0, #1; bx lr; movs r0, #2;
        // bx lr. The beq jumps past 0x1006.
        let code = [
            0x00, 0x28, 0x02, 0xd0, 0x70, 0x47, 0x01, 0x20, 0x70, 0x47, 0x02, 0x20, 0x70, 0x47,
        ];
        assert_eq!(functions(&code, &[0x07, 0x10, 0, 0]), [(0x1000, 14)]);
    }
}
