//! The Cortex-M vector table, from which the processor takes the initial
//! stack pointer at reset and the address of the handler for each
//! exception and interrupt.
//!
//! The table is a list of words: the initial stack pointer, then the
//! address of the reset handler, which is usually the entry point (plus 1,
//! for Thumb), then the other handlers. Reserved entries are 0, or on some
//! parts another word, such as the checksum that NXP's LPC boot ROM checks
//! in entry 7. Linker scripts name its section differently (`.isr_vector`,
//! `.vector_table`) or put it first in `.text`, so `find` finds it by its
//! shape, and its entries tell where it ends (see `VectorTable::size`).

use object::SectionKind;

use super::{ArmArchitecture, Binary, FileKind, Item, ItemKind, Section, Source, made_names};

/// Where the vector table is and the handlers it lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorTable {
    /// The address of its first word, the initial stack pointer.
    pub address: u64,
    /// Its size in bytes. From the reset entry on, the entries are read in
    /// turn, counting the stack pointer's as entry 0:
    ///
    /// 1. An entry that holds the address of Thumb code, an odd address in a
    ///    code section once bit 0 is cleared, is a handler, and the table
    ///    runs past it.
    /// 2. So does an entry that holds 0.
    /// 3. A reserved entry (7, 8, 9, 10 or 13) that holds another word is
    ///    skipped: the table runs past it only if a later entry does, so
    ///    such entries at its end, which are code after a short table, are
    ///    left out.
    /// 4. Any other entry ends the table before it, and so do the first
    ///    entry that the file holds no bytes for and the end of the reset
    ///    entry's section.
    pub size: u64,
    /// Where each handler's code starts, without the Thumb bit, in the
    /// table's order: first the reset handler, which is usually the entry
    /// point. A handler for several exceptions is listed once for each.
    pub handlers: Vec<u64>,
    /// The exception of each of `handlers`, by its entry's number, counting
    /// the stack pointer's as 0: 1 for reset, 3 for HardFault, 15 for
    /// SysTick, and 16 on for the chip's interrupts.
    pub exceptions: Vec<u64>,
}

impl VectorTable {
    /// Whether the code at `address` handles an exception that comes in
    /// normal operation: SVCall, PendSV, SysTick or one of the chip's
    /// interrupts. The others come at reset, when something has gone wrong
    /// (the faults, and NMI, which most chips raise for a hardware error),
    /// while debugging (DebugMonitor), or never (reserved entries).
    pub fn runs_in_normal_operation(&self, address: u64) -> bool {
        self.exceptions_of(address)
            .any(|exception| ROUTINE.contains(&exception) || exception >= FIRST_INTERRUPT)
    }

    /// Where the reset handler's code starts: the code the processor runs
    /// at reset. `None` when the reset entry doesn't hold the address of Thumb
    /// code, which happens only when the word taken for it holds the
    /// address of an entry point that isn't code.
    pub fn reset_handler(&self) -> Option<u64> {
        let position = self
            .exceptions
            .iter()
            .position(|&exception| exception == 1)?;
        Some(self.handlers[position])
    }

    /// Whether the table holds `address`.
    pub fn contains(&self, address: u64) -> bool {
        (self.address..self.address + self.size).contains(&address)
    }

    /// The exceptions that the code at `address` handles, by their entries'
    /// numbers, in the table's order: none when no entry lists it.
    pub fn exceptions_of(&self, address: u64) -> impl Iterator<Item = u64> + '_ {
        self.handlers
            .iter()
            .zip(&self.exceptions)
            .filter(move |&(&handler, _)| handler == address)
            .map(|(_, &exception)| exception)
    }
}

/// The name of the exception of entry `number`, counting the stack
/// pointer's as 0, as Arm's manuals name it for `architecture`, which the
/// file's build attributes give: "Reset", "HardFault", "SysTick",
/// "reserved" for the entries that the architecture reserves (see
/// [`is_reserved`]), or "IRQ 3" for the chip's fourth interrupt. Entry 0 is
/// "initial SP".
pub fn exception_name(number: u64, architecture: Option<ArmArchitecture>) -> String {
    if is_reserved(number, architecture) {
        return "reserved".to_string();
    }
    let name = match number {
        0 => "initial SP",
        1 => "Reset",
        2 => "NMI",
        3 => "HardFault",
        4 => "MemManage",
        5 => "BusFault",
        6 => "UsageFault",
        7 => "SecureFault",
        11 => "SVCall",
        12 => "DebugMonitor",
        14 => "PendSV",
        15 => "SysTick",
        number => return format!("IRQ {}", number - FIRST_INTERRUPT),
    };
    name.to_string()
}

/// Whether `architecture` reserves entry `number` of the vector table,
/// counting the stack pointer's as 0: entries 7 to 10 and 13 on Armv7-M,
/// and when the file doesn't say; also 4 to 6 and 12 on Armv6-M and
/// Armv8-M Baseline, which have no MemManage, BusFault, UsageFault or
/// DebugMonitor; and not 7 on Armv8-M Mainline, where it is SecureFault.
pub fn is_reserved(number: u64, architecture: Option<ArmArchitecture>) -> bool {
    match architecture.map(|architecture| architecture.version) {
        // Armv6-M, Armv6S-M and Armv8-M Baseline.
        Some(11 | 12 | 16) => RESERVED.contains(&number) || [4, 5, 6, 12].contains(&number),
        // Armv8-M Mainline and Armv8.1-M Mainline.
        Some(17 | 21) => number != 7 && RESERVED.contains(&number),
        _ => RESERVED.contains(&number),
    }
}

/// The entries that Armv7-M reserves, counted from the stack pointer's.
const RESERVED: [u64; 5] = [7, 8, 9, 10, 13];

/// The entries of SVCall, PendSV and SysTick, the exceptions other than the
/// chip's interrupts that come in normal operation.
const ROUTINE: [u64; 3] = [11, 14, 15];

/// The entry of the chip's first interrupt.
const FIRST_INTERRUPT: u64 = 16;

/// The kinds of section that can hold the vector table: code or data, not
/// a Linux program's version tables or an unwinder's `.ARM.exidx`. LLD
/// marks a section of read-only data that holds any strings as one of
/// strings, so the table can begin one.
const CODE_OR_DATA: [SectionKind; 4] = [
    SectionKind::Text,
    SectionKind::ReadOnlyData,
    SectionKind::ReadOnlyString,
    SectionKind::Data,
];

/// Finds the vector table in a Thumb file, if it has one, by its shape:
///
/// 1. Sections of code or data are tried in the order the file lists
///    them.
/// 2. In each, the words at multiples of the pointer size are read from the
///    second one on, since the stack pointer comes first. In a code section,
///    only that second word is read, so that no constant in a function is
///    taken for the reset entry.
/// 3. The first word that holds the entry point's Thumb address, the entry
///    point plus 1, and follows one that could be the stack pointer (see
///    [`could_be_stack_pointer`]), is the reset entry, and the table
///    starts one word before it.
/// 4. Unless that word is its section's second, a section that begins like
///    a table comes first (see [`at_section_start`]), as in firmware whose
///    entry point is `main`, or none: no word holds the entry point there,
///    or one that does is a pointer to `main` in data.
/// 5. The entries from there tell where it ends (see `VectorTable::size`).
pub(super) fn find(binary: &Binary) -> Option<VectorTable> {
    let size = binary.pointer_size;
    let held = holding_entry(binary);
    let (section, reset) = match held {
        Some((section, reset)) if reset - size == section.address => (section, reset),
        _ => at_section_start(binary).or(held)?,
    };

    let address = reset - size;
    let section_end = section.address + section.size;
    let mut handlers = Vec::new();
    let mut exceptions = Vec::new();
    let mut end = reset;
    let mut next = reset;
    for (slot, value) in binary.words(reset, section_end) {
        // The table ends at the first entry the file holds no bytes for.
        if slot != next {
            break;
        }
        next = slot + size;
        let index = (slot - address) / size;
        if let Some(handler) = binary.thumb_code_address(value) {
            handlers.push(handler);
            exceptions.push(index);
        } else if value != 0 {
            if RESERVED.contains(&index) {
                continue;
            }
            break;
        }
        end = slot + size;
    }
    Some(VectorTable {
        address,
        size: end - address,
        handlers,
        exceptions,
    })
}

/// The section that holds the vector table and its reset entry, where a
/// word holds the entry point's Thumb address (steps 1 to 3 of [`find`]).
fn holding_entry(binary: &Binary) -> Option<(&Section, u64)> {
    let size = binary.pointer_size;
    let follows_stack = |slot: u64| {
        let mut before = binary.words(slot - size, slot);
        before
            .next()
            .is_some_and(|(_, stack)| could_be_stack_pointer(binary, stack))
    };
    binary.sections.iter().find_map(|section| {
        if !section.allocated || !CODE_OR_DATA.contains(&section.kind) {
            return None;
        }
        // The reset entry follows the stack pointer, so it can't be first.
        let first = section.address.next_multiple_of(size) + size;
        let mut end = section.address + section.size;
        // In code, only there, so that no constant in a function is taken
        // for it.
        if section.is_code() {
            end = end.min(first + size);
        }
        let (slot, _) = binary
            .words(first, end)
            .find(|&(slot, value)| value == binary.entry | 1 && follows_stack(slot))?;
        Some((section, slot))
    })
}

/// The section that holds the vector table and its reset entry, for
/// firmware whose entry point isn't its reset handler: a linker script can
/// name `main` as the entry point, or none, and pico-sdk's names code that
/// runs before the reset handler. The table is at the start of the first
/// section of code or data, by address, that the file holds bytes for and
/// that begins like a table:
///
/// 1. the initial stack pointer (see [`could_be_stack_pointer`]);
/// 2. the Thumb addresses of the reset, NMI and HardFault handlers (see
///    [`Binary::thumb_code_address`]), which every table fills in. A
///    table of pairs of an argument and a handler, such as Zephyr's
///    `_sw_isr_table`, holds the second interrupt's argument where NMI's
///    would be.
///
/// Sections that startup code copies into place, such as `.data`, come
/// last: the processor reads the table at reset, before any copy, and
/// where RAM comes before flash, as on an i.MX RT, `.data` can begin with
/// a pointer and three callbacks.
///
/// Only in an executable linked to run at one address, as firmware usually
/// is. Position-independent files are mostly Linux programs and libraries,
/// so in firmware linked with `-pie`, the table is found only where a word
/// holds the entry point.
fn at_section_start(binary: &Binary) -> Option<(&Section, u64)> {
    if binary.kind != FileKind::Executable {
        return None;
    }
    let size = binary.pointer_size;
    let mut sections: Vec<&Section> = (binary.sections.iter())
        .filter(|section| section.allocated && section.file_range.is_some())
        .filter(|section| CODE_OR_DATA.contains(&section.kind))
        .collect();
    sections.sort_by_key(|section| (section.load_address.is_some(), section.address));
    sections.into_iter().find_map(|section| {
        let start = section.address;
        if !start.is_multiple_of(size) || section.size < 4 * size {
            return None;
        }
        let words: Vec<u64> = (binary.words(start, start + 4 * size))
            .map(|(_, value)| value)
            .collect();
        let [stack, reset, nmi, hard_fault] = words[..] else {
            return None;
        };
        let table = could_be_stack_pointer(binary, stack)
            && [reset, nmi, hard_fault]
                .iter()
                .all(|&value| binary.thumb_code_address(value).is_some());
        table.then_some((section, start + size))
    })
}

/// Whether `value` could be the initial stack pointer: a multiple of 4
/// other than 0, whose stack's first word isn't in code or read-only data
/// that the file holds, as the stack is in RAM. So a list of pointers, such
/// as a driver's configuration and then its functions, doesn't pass for
/// the vector table when its first pointer is 0 or points to a constant.
fn could_be_stack_pointer(binary: &Binary, value: u64) -> bool {
    let read_only = [
        SectionKind::Text,
        SectionKind::ReadOnlyData,
        SectionKind::ReadOnlyString,
    ];
    let in_read_only = |address| {
        (binary.section_at(address))
            .is_some_and(|index| read_only.contains(&binary.sections[index].kind))
            && binary.pointer_at(address).is_some()
    };
    value != 0 && value.is_multiple_of(4) && !in_read_only(value - 4)
}

/// An item for the vector table when no item covers any of it, as in
/// stripped firmware. It is named after its section, such as
/// `.isr_vector`, or when it is only part of it, after the section and its
/// address, as in `.text@0x0`.
pub(super) fn item(binary: &Binary, items: &[Item]) -> Option<Item> {
    let table = binary.vector_table.as_ref()?;
    let end = table.address + table.size;
    // An item of unknown size that starts in the table covers it.
    let covered = items
        .iter()
        .any(|item| item.address < end && table.address < item.address + item.size.max(1));
    if covered {
        return None;
    }
    let index = binary.section_at(table.address)?;
    let section = &binary.sections[index];
    let name = if (section.address, section.size) == (table.address, table.size) {
        section.name.clone()
    } else {
        made_names::data_name_at(section, table.address)
    };
    Some(Item {
        name,
        aliases: Vec::new(),
        kind: ItemKind::Data,
        address: table.address,
        size: table.size,
        size_estimated: false,
        section: index,
        source: Source::Section,
    })
}

#[cfg(test)]
mod tests;
