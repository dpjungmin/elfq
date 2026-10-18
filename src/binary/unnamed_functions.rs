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
//! outside the padding between functions; where no jump crosses it, and
//! none but a `b.w` lands on it; and where the code on either side still
//! finds the constants it loads.
//!
//! Some linker scripts put constants in `.text` after the code, as CMSIS's
//! template does. Code loads their addresses from outside the function they
//! follow, and such an address ends that function where the code before it
//! ends with a return or a jump, doesn't jump to it or past it, and still
//! finds what it did when scanned alone. Unlike a function, constants may
//! start with zeros, which look like padding. Code followed before may have
//! read the constants as code, so it is followed again.
//!
//! Each function runs to the next start, or the end of its section, less
//! the padding after it, so its size is an estimate. It is named after its
//! address, as in `fn_0x8000040`. What this misses:
//!
//! - Code that nothing calls or points to stays part of the function
//!   before it. So does a function that code reaches only with a 16-bit
//!   `b`, or that only data points to when such a `b` just before it jumps
//!   there, since compilers use `b` for tail calls to nearby functions as
//!   well as for jumps inside one. So do constants after a function that
//!   only that function loads.
//! - A `bl` to a place inside a function starts a function there, as in
//!   Cortex-M0 code, which jumps that way where `b` can't reach; and so
//!   does a `b.w` past everything the function has reached so far, as in
//!   long unoptimized functions. A number that equals the address right
//!   after a return, a jump or a call starts one too, as a CRC polynomial
//!   or a table entry can where code starts at address 0.

use std::collections::{BTreeMap, BTreeSet};

use super::{Binary, Item, ItemKind, STARTUP_ARRAYS, Section, Source, code_section, unnamed_data};
use crate::arch::{self, CodeReferenceKind, Ending};

/// What starts at an address in code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Start {
    Function,
    /// Constants that code loads, as when a linker script puts `.rodata` in
    /// `.text`. They end the function before them.
    Constants,
}

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
    let mut seeds: BTreeMap<u64, Start> = known
        .into_iter()
        .map(|address| (address, Start::Function))
        .collect();
    let stored = stored_addresses(binary);
    // Code followed before the constants after it were known may have read
    // them as code, so once there are new constants, it is followed again.
    let starts = loop {
        let (starts, loaded) = follow(binary, &seeds, &stored);
        let constants: Vec<u64> = loaded
            .into_iter()
            .filter(|&address| {
                !starts.contains_key(&address)
                    && splits_cleanly(binary, &starts, address, Start::Constants)
            })
            .collect();
        if constants.is_empty() {
            break starts;
        }
        seeds.extend(
            constants
                .into_iter()
                .map(|address| (address, Start::Constants)),
        );
    };

    // Each start comes once, so a function added below never matches
    // another: only the items there before need searching.
    let known = items.len();
    for (start, end) in functions(binary, &starts) {
        // Without the padding that aligns the next function.
        let size = binary
            .bytes(start, end - start)
            .map_or(end - start, |code| {
                arch::code_size(binary.architecture, binary.endianness, code, start)
            });
        if let Some(item) = items[..known]
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

/// Follows the code from `seeds` until it leads nowhere new, and returns the
/// starts it knows then, with the addresses in code that it loads, where
/// there may be constants.
fn follow(
    binary: &Binary,
    seeds: &BTreeMap<u64, Start>,
    stored: &BTreeSet<u64>,
) -> (BTreeMap<u64, Start>, BTreeSet<u64>) {
    let mut starts = seeds.clone();
    // Thumb addresses that may be numbers instead: those stored in data,
    // and those the code loads.
    let mut numbers = stored.clone();
    let mut constants = BTreeSet::new();
    // A function's code is scanned again only when a new start ends it
    // sooner. Only a new function, and the one just before it, can end
    // somewhere new, so only they are looked at again.
    let mut scanned = BTreeSet::new();
    let mut to_scan: Vec<u64> = functions(binary, &starts).map(|(start, _)| start).collect();
    loop {
        let mut found = BTreeSet::new();
        for start in to_scan.drain(..) {
            let Some(end) = end(binary, &starts, start) else {
                continue;
            };
            if scanned.insert((start, end)) {
                let leads = leads(binary, start, end);
                found.extend(leads.functions);
                numbers.extend(leads.thumb_addresses);
                constants.extend(leads.constants);
            }
        }
        found.retain(|address| !starts.contains_key(address));
        // They are checked against the functions found so far, so only once
        // the code leads nowhere new. So are tables among constants in code.
        if found.is_empty() {
            let tables = starts
                .iter()
                .filter(|&(_, &kind)| kind == Start::Constants)
                .filter_map(|(&start, _)| Some((start, end(binary, &starts, start)?)))
                .flat_map(|(start, end)| thumb_addresses(binary, start, end));
            found = numbers
                .iter()
                .copied()
                .chain(tables)
                .filter(|&address| {
                    !starts.contains_key(&address)
                        && splits_cleanly(binary, &starts, address, Start::Function)
                })
                .collect();
        }
        if found.is_empty() {
            return (starts, constants);
        }
        starts.extend(found.iter().map(|&address| (address, Start::Function)));
        for address in found {
            to_scan.push(address);
            if let Some((&before, Start::Function)) = starts.range(..address).next_back() {
                to_scan.push(before);
            }
        }
    }
}

/// Each function's start and end.
fn functions<'a>(
    binary: &'a Binary,
    starts: &'a BTreeMap<u64, Start>,
) -> impl Iterator<Item = (u64, u64)> + 'a {
    starts
        .iter()
        .filter(|&(_, &kind)| kind == Start::Function)
        .filter_map(|(&start, _)| Some((start, end(binary, starts, start)?)))
}

/// Where what starts at `start` ends: at the next start, or the end of its
/// section.
fn end(binary: &Binary, starts: &BTreeMap<u64, Start>, start: u64) -> Option<u64> {
    let section = &binary.sections[code_section(&binary.sections, start)?];
    let end = section.address + section.size;
    let next = starts.range(start + 1..).next();
    Some(next.map_or(end, |(&next, _)| next.min(end)))
}

/// What the code of one function leads to.
#[derive(Default)]
struct Leads {
    /// Where it calls or carries on in another function.
    functions: Vec<u64>,
    /// The Thumb addresses it loads, which may be numbers instead.
    thumb_addresses: Vec<u64>,
    /// The other addresses in code it loads, outside itself.
    constants: Vec<u64>,
}

/// Where the code from `start` to `end` leads.
fn leads(binary: &Binary, start: u64, end: u64) -> Leads {
    let mut leads = Leads::default();
    let code = binary.bytes(start, end - start).unwrap_or_default();
    // How far the function's other jumps reach, so far.
    let mut reach = start;
    for reference in arch::scan(binary.architecture, binary.endianness, code, start) {
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
            CodeReferenceKind::Address(address) if !(start..end).contains(&address) => {
                leads.constants.push(address);
            }
            _ => {}
        }
    }
    let in_code = |address: &u64| code_section(&binary.sections, *address).is_some();
    leads.functions.retain(in_code);
    leads.thumb_addresses.retain(in_code);
    leads.constants.retain(in_code);
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
    binary
        .aligned_words(start, end)
        .filter(|&(_, value)| value & 1 == 1)
        .map(|(_, value)| value & !1)
        .filter(|&address| code_section(&binary.sections, address).is_some())
        .collect()
}

/// Where the code in `section` starts: after the vector table, when a
/// linker script puts it first in `.text`.
fn code_start(binary: &Binary, section: &Section) -> u64 {
    match &binary.vector_table {
        Some(table) if section.contains(table.address) => table.address + table.size,
        _ => section.address,
    }
}

/// Whether a function, or constants, can start at `address` without
/// cutting the code before it in two, as far as `starts` tell: that code
/// ends there, with an instruction that doesn't run on into it, such as a
/// return or, before a function, a call that may never return; and no jump
/// crosses into it, except to where a function starts, or lands on it,
/// except a `b.w`, which may be a tail call. Scanning the code before it
/// must also find what it does with what follows, such as each constant it
/// loads, and so must scanning a function after it.
fn splits_cleanly(
    binary: &Binary,
    starts: &BTreeMap<u64, Start>,
    address: u64,
    kind: Start,
) -> bool {
    let Some(index) = code_section(&binary.sections, address) else {
        return false;
    };
    let section = &binary.sections[index];
    let section_end = section.address + section.size;
    let code_start = code_start(binary, section);
    if address < code_start {
        return false;
    }
    let before = starts.range(code_start..address).next_back();
    let start = match (before, kind) {
        // Inside constants, nothing is cut.
        (Some((_, Start::Constants)), _) => return false,
        (Some((&start, _)), _) => start,
        // Constants matter only where they end a function.
        (None, Start::Constants) => return false,
        (None, Start::Function) => code_start,
    };
    let end = starts.range(address..section_end).next();
    let end = end.map_or(section_end, |(&end, _)| end);
    let bytes = |from: u64, to: u64| binary.bytes(from, to - from).unwrap_or_default();
    let scan = |from: u64, to: u64| {
        arch::scan(
            binary.architecture,
            binary.endianness,
            bytes(from, to),
            from,
        )
    };
    // Before the first function known in a section, there may be data.
    if before.is_some() {
        match arch::ending(
            binary.architecture,
            binary.endianness,
            bytes(start, address),
            start,
        ) {
            Ending::RunsOn => return false,
            Ending::Call if kind == Start::Constants => return false,
            _ => {}
        }
    }
    let jumps_into = |reference: &arch::CodeReference, from: u64, to: u64| match reference.kind {
        CodeReferenceKind::Jump(target) | CodeReferenceKind::TailJump(target) => {
            (from..to).contains(&target) && !starts.contains_key(&target)
        }
        _ => false,
    };
    if kind == Start::Constants {
        let whole = scan(start, end);
        let alone = scan(start, address);
        let before = &whole[..alone.len().min(whole.len())];
        return before == alone
            && whole
                .get(alone.len())
                .is_none_or(|next| next.site >= address)
            && !alone
                .iter()
                .any(|reference| jumps_into(reference, address, end));
    }
    // Without the padding after the code, which can decode as a jump.
    let end = start
        + arch::code_size(
            binary.architecture,
            binary.endianness,
            bytes(start, end),
            start,
        );
    // A function starts after the padding before it.
    if address >= end || arch::leading_padding(binary.architecture, bytes(address, end)) > 0 {
        return false;
    }
    let whole = scan(start, end);
    let mut halves = scan(start, address);
    halves.extend(scan(address, end));
    // A `b.w` from before may be a tail call to a function there, but other
    // jumps to it stay inside one function.
    let crosses = whole.iter().any(|reference| {
        if reference.site < address {
            reference.kind == CodeReferenceKind::Jump(address)
                || jumps_into(reference, address + 1, end)
        } else {
            jumps_into(reference, start, address)
        }
    });
    whole == halves && !crosses
}

#[cfg(test)]
mod tests {
    use object::{Architecture, Endianness, SectionKind};

    use super::*;
    use crate::binary::FileKind;

    /// The functions found in firmware that runs `text` from its start at
    /// 0x1000 and has `rodata` at 0x2000, as (address, size).
    fn functions(text: &[u8], rodata: &[u8]) -> Vec<(u64, u64)> {
        functions_with(text, (".rodata", SectionKind::ReadOnlyData, rodata))
    }

    /// The same, with another section at 0x2000 than `.rodata`: its name,
    /// kind and bytes.
    fn functions_with(
        text: &[u8],
        (name, kind, data): (&str, SectionKind, &[u8]),
    ) -> Vec<(u64, u64)> {
        let section = |name: &str, address, kind, offset, bytes: &[u8]| Section {
            name: name.to_string(),
            address,
            size: bytes.len() as u64,
            kind,
            allocated: true,
            file_range: Some((offset, bytes.len() as u64)),
            entry_size: 0,
        };
        let binary = Binary {
            architecture: Architecture::Arm,
            pointer_size: 4,
            kind: FileKind::Executable,
            entry: 0x1000,
            thumb: true,
            endianness: Endianness::Little,
            has_symbols: false,
            has_dwarf: false,
            sections: vec![
                section(".text", 0x1000, SectionKind::Text, 0, text),
                section(name, 0x2000, kind, text.len() as u64, data),
            ],
            items: Vec::new(),
            imports: Vec::new(),
            relocations: Vec::new(),
            used_by_unwinder: Vec::new(),
            merged_variables: Vec::new(),
            vector_table: None,
            warnings: Vec::new(),
            data: [text, data].concat(),
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
    fn startup_arrays_are_followed() {
        // movs r0, #1; bx lr; movs r0, #2; bx lr. Only `.init_array`
        // points to 0x1004.
        let code = [0x01, 0x20, 0x70, 0x47, 0x02, 0x20, 0x70, 0x47];
        let init_array = (".init_array", SectionKind::Unknown, &[0x05, 0x10, 0, 0][..]);
        assert_eq!(
            functions_with(&code, init_array),
            [(0x1000, 4), (0x1004, 4)]
        );
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
        // bl 0x1008; bx lr; nop; then the function it calls, bx lr. Data
        // points to the nop, which pads.
        let padded = [0x00, 0xf0, 0x02, 0xf8, 0x70, 0x47, 0x00, 0xbf, 0x70, 0x47];
        assert_eq!(
            functions(&padded, &[0x07, 0x10, 0, 0]),
            [(0x1000, 6), (0x1008, 2)]
        );
        // bl 0x1008, which may never return; movs r0, #1; bx lr; b 0x1008.
        let after_a_call = [0x00, 0xf0, 0x02, 0xf8, 0x01, 0x20, 0x70, 0x47, 0xfe, 0xe7];
        assert_eq!(
            functions(&after_a_call, &[0x05, 0x10, 0, 0]),
            [(0x1000, 4), (0x1004, 4), (0x1008, 2)]
        );
        // ldr r0, [pc, #4]; bx lr; movs r0, #2; bx lr; the word it loads,
        // which the code before 0x1004 no longer finds when cut there.
        let loads = [
            0x01, 0x48, 0x70, 0x47, 0x02, 0x20, 0x70, 0x47, 0x78, 0x56, 0x34, 0x12,
        ];
        assert_eq!(functions(&loads, &[0x05, 0x10, 0, 0]), [(0x1000, 12)]);
    }

    #[test]
    fn an_address_in_data_that_a_jump_crosses_starts_nothing() {
        // cmp r0, #0; beq 0x100a; bx lr; movs r0, #1; bx lr; movs r0, #2;
        // bx lr. The beq jumps past 0x1006.
        let code = [
            0x00, 0x28, 0x02, 0xd0, 0x70, 0x47, 0x01, 0x20, 0x70, 0x47, 0x02, 0x20, 0x70, 0x47,
        ];
        assert_eq!(functions(&code, &[0x07, 0x10, 0, 0]), [(0x1000, 14)]);
        // movs r0, #1; bx lr; subs r0, #1; bne 0x1002; bx lr. The bne
        // jumps back past 0x1004.
        let back = [0x01, 0x20, 0x70, 0x47, 0x01, 0x38, 0xfc, 0xd1, 0x70, 0x47];
        assert_eq!(functions(&back, &[0x05, 0x10, 0, 0]), [(0x1000, 10)]);
    }

    #[test]
    fn an_address_in_data_that_a_jump_lands_on_starts_nothing() {
        // ldr r0, [pc, #0]; b 0x1008, over the word it loads; the word;
        // adds r0, #1; bx lr.
        let island = [
            0x00, 0x48, 0x01, 0xe0, 0x00, 0x00, 0x00, 0x20, 0x01, 0x30, 0x70, 0x47,
        ];
        assert_eq!(functions(&island, &[0x09, 0x10, 0, 0]), [(0x1000, 12)]);
        // cmp r0, #0; beq 0x1008; movs r0, #1; bx lr; movs r0, #2; bx lr.
        let branch = [
            0x00, 0x28, 0x01, 0xd0, 0x01, 0x20, 0x70, 0x47, 0x02, 0x20, 0x70, 0x47,
        ];
        assert_eq!(functions(&branch, &[0x09, 0x10, 0, 0]), [(0x1000, 12)]);
    }

    #[test]
    fn an_address_in_data_at_a_case_of_a_jump_table_starts_nothing() {
        // cmp r0, #1; bhi 0x100a; tbb [pc, r0] with cases at 0x100e and
        // 0x1012. Then for other values, movs r0, #0; bx lr; and the cases,
        // movs r0, #n; bx lr.
        let code = [
            0x01, 0x28, 0x02, 0xd8, 0xdf, 0xe8, 0x00, 0xf0, 0x03, 0x05, 0x00, 0x20, 0x70, 0x47,
            0x01, 0x20, 0x70, 0x47, 0x02, 0x20, 0x70, 0x47,
        ];
        assert_eq!(functions(&code, &[0x0f, 0x10, 0, 0]), [(0x1000, 22)]);
        assert_eq!(functions(&code, &[0x13, 0x10, 0, 0]), [(0x1000, 22)]);
    }

    #[test]
    fn an_adr_to_a_function_starts_it() {
        // As GNU as assembles `adr r0, function` to a Thumb function, at
        // the function's address + 1: push {r4, lr}; adr.w r0, 0x100d;
        // bl 0x101a; pop {r4, pc}. Then 0x100c: push {r4, lr}; movs r1, #2;
        // bl 0x1016; pop {r4, pc}. 0x1016: movs r0, #1; bx lr. 0x101a: bx
        // lr.
        let code = [
            0x10, 0xb5, 0x0f, 0xf2, 0x09, 0x00, 0x00, 0xf0, 0x08, 0xf8, 0x10, 0xbd, 0x10, 0xb5,
            0x02, 0x21, 0x00, 0xf0, 0x01, 0xf8, 0x10, 0xbd, 0x01, 0x20, 0x70, 0x47, 0x70, 0x47,
        ];
        assert_eq!(
            functions(&code, &[]),
            [(0x1000, 12), (0x100c, 10), (0x1016, 4), (0x101a, 2)]
        );
    }

    #[test]
    fn constants_among_the_code_end_the_function_before_them() {
        // push {r4, lr}; bl 0x1010; ldr r0, =0x1014, the constants; pop
        // {r4, pc}; nop; the word it loads. Then the function it calls,
        // movs r0, #1; bx lr; and the constants.
        let code = [
            0x10, 0xb5, 0x00, 0xf0, 0x05, 0xf8, 0x01, 0x48, 0x10, 0xbd, 0x00, 0xbf, 0x14, 0x10,
            0x00, 0x00, 0x01, 0x20, 0x70, 0x47, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x00,
        ];
        assert_eq!(functions(&code, &[]), [(0x1000, 16), (0x1010, 4)]);
        // The same, but the constants decode as bl 0x101a, which the code
        // followed again no longer finds.
        let code = [
            0x10, 0xb5, 0x00, 0xf0, 0x05, 0xf8, 0x01, 0x48, 0x10, 0xbd, 0x00, 0xbf, 0x14, 0x10,
            0x00, 0x00, 0x01, 0x20, 0x70, 0x47, 0x00, 0xf0, 0x01, 0xf8, 0x45, 0x46, 0x47, 0x00,
        ];
        assert_eq!(functions(&code, &[]), [(0x1000, 16), (0x1010, 4)]);
        // bl 0x1010; bx lr; 2 bytes of padding; constants that start with
        // zeros; then the function it calls: ldr r0, =0x1008; bx lr; the
        // word it loads.
        let zeros = [
            0x00, 0xf0, 0x06, 0xf8, 0x70, 0x47, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00, 0x00, 0x48, 0x70, 0x47, 0x08, 0x10, 0x00, 0x00,
        ];
        assert_eq!(functions(&zeros, &[]), [(0x1000, 6), (0x1010, 8)]);
    }

    #[test]
    fn tables_among_constants_start_functions() {
        // push {r4, lr}; bl 0x1010; ldr r0, =0x1018; pop {r4, pc}; nop; the
        // word it loads. Then 0x1010: movs r0, #1; bx lr. 0x1014: movs r0,
        // #2; bx lr, which only the table at 0x1018 points to.
        let code = [
            0x10, 0xb5, 0x00, 0xf0, 0x05, 0xf8, 0x01, 0x48, 0x10, 0xbd, 0x00, 0xbf, 0x18, 0x10,
            0x00, 0x00, 0x01, 0x20, 0x70, 0x47, 0x02, 0x20, 0x70, 0x47, 0x15, 0x10, 0x00, 0x00,
        ];
        assert_eq!(
            functions(&code, &[]),
            [(0x1000, 16), (0x1010, 4), (0x1014, 4)]
        );
    }

    #[test]
    fn what_a_function_points_to_in_itself_ends_nothing() {
        // adr r0, 0x1008; ldr r0, [r0]; bx lr; nop; the word it points to.
        let code = [
            0x01, 0xa0, 0x00, 0x68, 0x70, 0x47, 0x00, 0xbf, 0x78, 0x56, 0x34, 0x12,
        ];
        assert_eq!(functions(&code, &[]), [(0x1000, 12)]);
    }

    #[test]
    fn constants_at_a_case_of_a_jump_table_end_nothing() {
        // 0x1000: push {r4, lr}; bl 0x1028; cmp r0, #2; bls 0x100e; movs
        // r0, #0; pop {r4, pc}. 0x100e: tbb [pc, r0] with cases at 0x1016,
        // 0x101a and 0x101e: movs r0, #10; pop {r4, pc}; movs r0, #20; pop
        // {r4, pc}; bl 0x1024; pop {r4, pc}. 0x1024: movs r0, #1; bx lr.
        // 0x1028: ldr r0, =0x101a; bx lr; the word, which is the second
        // case's address.
        let code = [
            0x10, 0xb5, 0x00, 0xf0, 0x11, 0xf8, 0x02, 0x28, 0x01, 0xd9, 0x00, 0x20, 0x10, 0xbd,
            0xdf, 0xe8, 0x00, 0xf0, 0x02, 0x04, 0x06, 0x00, 0x0a, 0x20, 0x10, 0xbd, 0x14, 0x20,
            0x10, 0xbd, 0x00, 0xf0, 0x01, 0xf8, 0x10, 0xbd, 0x01, 0x20, 0x70, 0x47, 0x00, 0x48,
            0x70, 0x47, 0x1a, 0x10, 0x00, 0x00,
        ];
        assert_eq!(
            functions(&code, &[]),
            [(0x1000, 36), (0x1024, 4), (0x1028, 8)]
        );
    }
}
