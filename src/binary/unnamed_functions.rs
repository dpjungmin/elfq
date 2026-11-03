//! Functions in stripped firmware, which has no symbol table or unwind table
//! to list them.
//!
//! elfq follows the code from where the processor or the C runtime starts
//! running it: the entry point, the handlers in the vector table, and the
//! functions in arrays such as `.init_array`. It scans each function up to
//! the next start it knows, and these start more functions, when they are
//! in code:
//!
//! 1. what the code calls;
//! 2. where a Thumb `b.w`, a linker's veneer, or a jump through a register
//!    that the code put an address in goes, as code does to carry on in
//!    another function. Loops and long functions use `b.w` inside
//!    themselves too, so such a jump starts nothing where it lands:
//!    - between the function's start and the jump itself;
//!    - or no further into the function than its other jumps reach so far.
//!
//! Scanning repeats until nothing new turns up. Then the Thumb addresses
//! that the code loads or data holds start functions too, as for a callback
//! passed to another function, a table of callbacks or a vtable, and so do
//! those in tables among the constants in code; their code is followed in
//! turn. Such a word can also be a number that only looks like an address,
//! so it starts a function only where (`KnownStarts::function_can_start`
//! checks these in order, marked with their numbers):
//!
//! 1. it is in code, after the vector table;
//! 2. it isn't inside constants: what starts last before it in its
//!    section, if anything does, is a function;
//! 3. the code of that function ends there with a return, a jump, data
//!    such as a literal pool, or a call that may never return. Before the
//!    first start in a section there may be data, so this isn't checked
//!    there;
//! 4. it is outside the padding between functions: the code there doesn't
//!    start with padding, and isn't only padding up to the next start;
//! 5. the code before it and the code after it, scanned apart, find
//!    exactly what they find scanned together: no instruction spans it,
//!    and each side still finds the constants it loads;
//! 6. no jump crosses it, short of the padding before the next start, and
//!    none but a `b.w`, which may be a tail call, lands on it. A jump from
//!    after it may land on the start of the function before it.
//!
//! Some linker scripts put constants in `.text` after the code, as CMSIS's
//! template does. Code loads their addresses from outside the function they
//! follow, and such an address ends that function where
//! (`KnownStarts::constants_can_start` checks these in order, marked with
//! their numbers):
//!
//! 1. it is in code, after the vector table;
//! 2. the last thing that starts before it in its section is a function,
//!    not constants: constants matter only where they end a function;
//! 3. the code of that function ends there with a return, a jump or data,
//!    not a call;
//! 4. scanned alone, that code finds exactly what it finds scanned on to
//!    the next start, such as each constant it loads;
//! 5. that code doesn't jump to it or past it, short of the next start,
//!    not even with a `b.w`.
//!
//! Unlike a function, constants may start with zeros, which look like
//! padding. Code followed before may have read the constants as code, so
//! it is followed again.
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

use super::{AddressRange, Binary, Item, ItemKind, Section, Source, made_names};
use crate::arch::{CodeReference, CodeReferenceKind, Ending, InstructionSet};

/// What starts at an address in code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Start {
    Function,
    /// Constants that code loads, as when a linker script puts `.rodata` in
    /// `.text`. They end the function before them.
    Constants,
}

/// The addresses in a file's code where elfq knows that a function or
/// constants start, with the file, which says where each one ends.
#[derive(Clone)]
struct KnownStarts<'a> {
    binary: &'a Binary,
    /// What starts at each address.
    at: BTreeMap<u64, Start>,
}

/// How this module reads code: as Thumb, since only Thumb firmware has its
/// functions found by following its code (see `Binary::from_vec`).
fn thumb(binary: &Binary) -> InstructionSet {
    InstructionSet::Thumb {
        endianness: binary.endianness,
    }
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
        known.extend(table.handlers.iter().map(|handler| handler.address));
    }
    let arrays = (binary.sections.iter()).filter(|section| section.is_startup_array());
    known.extend(arrays.flat_map(|section| {
        thumb_addresses(binary, section.address, section.address + section.size)
    }));
    let mut seeds = KnownStarts {
        binary,
        at: known
            .into_iter()
            .map(|address| (address, Start::Function))
            .collect(),
    };
    let stored = stored_addresses(binary);
    // Code followed before the constants after it were known may have read
    // them as code, so once there are new constants, it is followed again.
    let starts = loop {
        let Followed {
            starts,
            maybe_constants,
        } = follow(&seeds, &stored);
        let constants: Vec<u64> = maybe_constants
            .into_iter()
            .filter(|&address| {
                !starts.at.contains_key(&address) && starts.constants_can_start(address)
            })
            .collect();
        if constants.is_empty() {
            break starts;
        }
        seeds.at.extend(
            constants
                .into_iter()
                .map(|address| (address, Start::Constants)),
        );
    };

    // Each start comes once, so a function added below never matches
    // another: only the items there before need searching.
    let known = items.len();
    for AddressRange { start, end } in starts.ranges(Start::Function) {
        // Without the padding that aligns the next function.
        let size = binary
            .bytes(start, end - start)
            .map_or(end - start, |code| thumb(binary).code_size(code, start));
        if let Some(item) = items[..known]
            .iter_mut()
            .find(|item| item.kind == ItemKind::Function && item.address == start)
        {
            item.size = size;
            item.size_estimated = true;
            continue;
        }
        let Some(section) = binary.code_section_at(start) else {
            continue;
        };
        items.push(Item {
            name: made_names::function_name_at(start),
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

/// What following the code finds (see [`follow`]).
struct Followed<'a> {
    /// The starts that elfq knows once the code leads nowhere new.
    starts: KnownStarts<'a>,
    /// The addresses in code that the code loads from outside the function
    /// that loads them, where constants may start.
    maybe_constants: BTreeSet<u64>,
}

/// Follows the code from `seeds` until it leads nowhere new.
fn follow<'a>(seeds: &KnownStarts<'a>, stored: &BTreeSet<u64>) -> Followed<'a> {
    let binary = seeds.binary;
    let mut starts = seeds.clone();
    // Thumb addresses that may be numbers instead: those stored in data,
    // and those the code loads.
    let mut maybe_functions = stored.clone();
    let mut maybe_constants = BTreeSet::new();
    // A function's code is scanned again only when a new start ends it
    // sooner. Only a new function, and the one just before it, can end
    // somewhere new, so only they are looked at again.
    let mut scanned = BTreeSet::new();
    let mut to_scan: Vec<u64> = (starts.ranges(Start::Function))
        .map(|function| function.start)
        .collect();
    loop {
        // Where the code calls or carries on in another function.
        let mut led_to = BTreeSet::new();
        for start in to_scan.drain(..) {
            let Some(end) = starts.end(start) else {
                continue;
            };
            if scanned.insert((start, end)) {
                let leads = leads(binary, start, end);
                led_to.extend(leads.functions);
                maybe_functions.extend(leads.thumb_addresses);
                maybe_constants.extend(leads.constants);
            }
        }
        led_to.retain(|address| !starts.at.contains_key(address));
        // The Thumb addresses that may be numbers are checked against the
        // functions found so far, so only once the code leads nowhere new.
        // So are those in tables among constants in code.
        let new_functions: BTreeSet<u64> = if led_to.is_empty() {
            let tables = (starts.ranges(Start::Constants))
                .flat_map(|constants| thumb_addresses(binary, constants.start, constants.end));
            maybe_functions
                .iter()
                .copied()
                .chain(tables)
                .filter(|&address| {
                    !starts.at.contains_key(&address) && starts.function_can_start(address)
                })
                .collect()
        } else {
            led_to
        };
        if new_functions.is_empty() {
            return Followed {
                starts,
                maybe_constants,
            };
        }
        let functions = new_functions
            .iter()
            .map(|&address| (address, Start::Function));
        starts.at.extend(functions);
        for address in new_functions {
            to_scan.push(address);
            if let Some((&before, Start::Function)) = starts.at.range(..address).next_back() {
                to_scan.push(before);
            }
        }
    }
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
    for reference in thumb(binary).scan(code, start) {
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
    let in_code = |address: &u64| binary.code_section_at(*address).is_some();
    leads.functions.retain(in_code);
    leads.thumb_addresses.retain(in_code);
    leads.constants.retain(in_code);
    leads
}

/// The Thumb code addresses stored in data.
fn stored_addresses(binary: &Binary) -> BTreeSet<u64> {
    let sections = (binary.sections.iter()).filter(|section| section.holds_data());
    sections
        .flat_map(|section| {
            thumb_addresses(binary, section.address, section.address + section.size)
        })
        .collect()
}

/// The Thumb code addresses stored from `start` to `end`, word by word,
/// without their Thumb bit (see [`Binary::thumb_code_address`]).
fn thumb_addresses(binary: &Binary, start: u64, end: u64) -> Vec<u64> {
    binary
        .aligned_words(start, end)
        .filter_map(|(_, value)| binary.thumb_code_address(value))
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

/// Where something that starts at an address would cut the code of its
/// section in two, as far as the known starts tell.
struct Cut {
    /// What starts last before the address in the section's code, which
    /// is where the code before the address starts.
    before: Before,
    /// Where the code after the address ends: at the next start, or the
    /// end of the section.
    end: u64,
}

/// What starts last before an address in its section's code.
enum Before {
    /// A function, which starts at `start`.
    Function { start: u64 },
    /// Constants: nothing that starts inside them cuts any code.
    Constants,
    /// Nothing: the section's code starts at `code_start`, and may begin
    /// with data.
    Nothing { code_start: u64 },
}

impl KnownStarts<'_> {
    /// From each start of `kind`, a function or constants, to where what
    /// starts there ends.
    fn ranges(&self, kind: Start) -> impl Iterator<Item = AddressRange> + '_ {
        (self.at.iter())
            .filter(move |&(_, &what)| what == kind)
            .filter_map(|(&start, _)| {
                let end = self.end(start)?;
                Some(AddressRange { start, end })
            })
    }

    /// Where what starts at `start` ends: at the next start, or the end of
    /// its section.
    fn end(&self, start: u64) -> Option<u64> {
        let section = &self.binary.sections[self.binary.code_section_at(start)?];
        let end = section.address + section.size;
        let next = self.at.range(start + 1..).next();
        Some(next.map_or(end, |(&next, _)| next.min(end)))
    }

    /// Where something that starts at `address` would cut the code, if
    /// `address` is in code, after the vector table: rule 1 of both lists
    /// in the module doc.
    fn cut(&self, address: u64) -> Option<Cut> {
        let section = &self.binary.sections[self.binary.code_section_at(address)?];
        let code_start = code_start(self.binary, section);
        if address < code_start {
            return None;
        }
        let before = match self.at.range(code_start..address).next_back() {
            Some((&start, Start::Function)) => Before::Function { start },
            Some((_, Start::Constants)) => Before::Constants,
            None => Before::Nothing { code_start },
        };
        let section_end = section.address + section.size;
        let end = self.at.range(address..section_end).next();
        let end = end.map_or(section_end, |(&end, _)| end);
        Some(Cut { before, end })
    }

    /// Whether a function can start at `address`, where nothing starts
    /// yet, without cutting the code before it in two: the rules for a
    /// function in the module doc, whose numbers the comments below
    /// follow.
    fn function_can_start(&self, address: u64) -> bool {
        // 1.
        let Some(cut) = self.cut(address) else {
            return false;
        };
        let start = match cut.before {
            // 2.
            Before::Constants => return false,
            // 3.
            Before::Function { start } => match self.ending(start, address) {
                Ending::RunsOn => return false,
                Ending::Call | Ending::Stops => start,
            },
            // 3 isn't checked: the section's code may begin with data.
            Before::Nothing { code_start } => code_start,
        };
        // Without the padding after the code, which can decode as a jump.
        let end = start + thumb(self.binary).code_size(self.bytes(start, cut.end), start);
        // 4.
        if address >= end || thumb(self.binary).leading_padding(self.bytes(address, end)) > 0 {
            return false;
        }
        // 5.
        let whole = self.scan(start, end);
        let mut halves = self.scan(start, address);
        halves.extend(self.scan(address, end));
        if whole != halves {
            return false;
        }
        // 6.
        !whole.iter().any(|reference| {
            if reference.site < address {
                // A `b.w` from before may be a tail call to a function
                // there, but other jumps to it stay inside one function.
                reference.kind == CodeReferenceKind::Jump(address)
                    || self.jumps_into(reference, address + 1, end)
            } else {
                self.jumps_into(reference, start, address)
            }
        })
    }

    /// Whether constants can start at `address`, where nothing starts yet,
    /// and end the function before them: the rules for constants in the
    /// module doc, whose numbers the comments below follow.
    fn constants_can_start(&self, address: u64) -> bool {
        // 1.
        let Some(cut) = self.cut(address) else {
            return false;
        };
        // 2.
        let start = match cut.before {
            Before::Function { start } => start,
            Before::Constants | Before::Nothing { .. } => return false,
        };
        // 3.
        match self.ending(start, address) {
            Ending::Stops => {}
            Ending::RunsOn | Ending::Call => return false,
        }
        // 4. Scanned on, the code finds the same first, and nothing else
        // before `address`.
        let whole = self.scan(start, cut.end);
        let alone = self.scan(start, address);
        let (first, rest) = whole.split_at(alone.len().min(whole.len()));
        if first != alone || rest.first().is_some_and(|next| next.site < address) {
            return false;
        }
        // 5.
        !alone
            .iter()
            .any(|reference| self.jumps_into(reference, address, cut.end))
    }

    /// How the code from `start` up to `address` ends.
    fn ending(&self, start: u64, address: u64) -> Ending {
        thumb(self.binary).ending(self.bytes(start, address), start)
    }

    /// The references in the code from `from` up to `to`, each instruction
    /// read on its own: when code that ends with a call to a function that
    /// never returns runs on into the next function, as far as elfq can
    /// tell, an address it left in a register would make the next
    /// function's references differ when scanned with it.
    fn scan(&self, from: u64, to: u64) -> Vec<CodeReference> {
        thumb(self.binary).scan_instructions(self.bytes(from, to), from)
    }

    /// The bytes from `from` up to `to`, or none where the file holds none.
    fn bytes(&self, from: u64, to: u64) -> &[u8] {
        self.binary.bytes(from, to - from).unwrap_or_default()
    }

    /// Whether `reference` is a jump that lands from `from` up to `to`,
    /// other than on a known start.
    fn jumps_into(&self, reference: &CodeReference, from: u64, to: u64) -> bool {
        let target = match reference.kind {
            CodeReferenceKind::Jump(target) | CodeReferenceKind::TailJump(target) => target,
            CodeReferenceKind::Call(_)
            | CodeReferenceKind::CallThrough(_)
            | CodeReferenceKind::JumpThrough(_)
            | CodeReferenceKind::IndirectCall
            | CodeReferenceKind::IndirectJump
            | CodeReferenceKind::Address(_)
            | CodeReferenceKind::Memory(_) => return false,
        };
        (from..to).contains(&target) && !self.at.contains_key(&target)
    }
}

#[cfg(test)]
mod tests;
