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

use super::{ArmArchitecture, Binary, FileKind, Item, ItemKind, Section, Source, code_section};

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
/// a Linux program's version tables or an unwinder's `.ARM.exidx`.
const CODE_OR_DATA: [SectionKind; 3] = [
    SectionKind::Text,
    SectionKind::ReadOnlyData,
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
        if thumb_code(binary, value) {
            handlers.push(value & !1);
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
        if section.kind == SectionKind::Text {
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
/// 2. the reset handler's Thumb address (see [`thumb_code`]);
/// 3. NMI's and HardFault's, each a Thumb address or 0.
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
            && thumb_code(binary, reset)
            && [nmi, hard_fault]
                .iter()
                .all(|&value| value == 0 || thumb_code(binary, value));
        table.then_some((section, start + size))
    })
}

/// Whether `value` could be the initial stack pointer: a multiple of 4
/// other than 0, whose stack's first word isn't in code or read-only data
/// that the file holds, as the stack is in RAM. So a table of pairs of an
/// argument and a handler, such as Zephyr's `_sw_isr_table`, doesn't pass
/// for the vector table when its first argument is 0 or a pointer to a
/// constant.
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

/// Whether `value` is the address of Thumb code: odd, and in a code section
/// once bit 0 is cleared.
fn thumb_code(binary: &Binary, value: u64) -> bool {
    value & 1 == 1 && code_section(&binary.sections, value & !1).is_some()
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
        format!("{}@{:#x}", section.name, table.address)
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
mod tests {
    use object::{Architecture, Endianness};

    use super::*;

    /// A Thumb executable whose entry point is `entry`, with `sections`:
    /// each a name, an address, a kind and the words it holds, which the
    /// file stores one section after another.
    fn firmware(entry: u64, sections: &[(&str, u64, SectionKind, Vec<u32>)]) -> Binary {
        let mut data = Vec::new();
        let sections = (sections.iter())
            .map(|(name, address, kind, words)| {
                let offset = data.len() as u64;
                data.extend(words.iter().flat_map(|word| word.to_le_bytes()));
                let size = data.len() as u64 - offset;
                Section {
                    name: name.to_string(),
                    address: *address,
                    size,
                    kind: *kind,
                    allocated: true,
                    file_range: Some((offset, size)),
                    entry_size: 0,
                    load_address: None,
                }
            })
            .collect();
        Binary {
            architecture: Architecture::Arm,
            pointer_size: 4,
            kind: FileKind::Executable,
            entry,
            thumb: true,
            endianness: Endianness::Little,
            has_symbols: false,
            has_dwarf: false,
            sections,
            items: Vec::new(),
            imports: Vec::new(),
            relocations: Vec::new(),
            used_by_unwinder: Vec::new(),
            merged_variables: Vec::new(),
            vector_table: None,
            arm_architecture: None,
            data_in_code: Vec::new(),
            warnings: Vec::new(),
            code_references: std::collections::HashMap::new(),
            data,
        }
    }

    #[test]
    fn a_short_table_in_code_ends_at_its_last_handler() {
        // The stack pointer, then the reset handler and five fault handlers,
        // all at 0x1030. Five words of code follow, where entries 7 to 10
        // would be reserved, and then the reset handler's code.
        let mut words = vec![0x2000_1000_u32];
        words.extend([0x1031; 6]);
        words.extend([0xbf00_bf00; 5]);
        words.push(0xe7fe_e7fe);
        let binary = firmware(0x1030, &[(".text", 0x1000, SectionKind::Text, words)]);
        let table = VectorTable {
            address: 0x1000,
            size: 28,
            handlers: vec![0x1030; 6],
            exceptions: vec![1, 2, 3, 4, 5, 6],
        };
        assert_eq!(find(&binary), Some(table));
    }

    /// Firmware whose entry point, 0x1050, isn't its reset handler, 0x1040,
    /// as when a linker script names `main` as the entry point. `.isr` holds
    /// the vector table, with NMI's entry 0; `.rodata`, which the file lists
    /// first and whose address comes first, begins like a table but for a
    /// stack pointer that isn't a multiple of 4, as an array of Thumb
    /// addresses would.
    fn entry_point_is_main(kind: FileKind) -> Binary {
        let mut binary = firmware(
            0x1050,
            &[
                (
                    ".rodata",
                    0x800,
                    SectionKind::ReadOnlyData,
                    vec![0x1041, 0x1041, 0x1041, 0x1041],
                ),
                (
                    ".isr",
                    0x900,
                    SectionKind::ReadOnlyData,
                    vec![0x2000_1000, 0x1041, 0, 0x1045],
                ),
                (".text", 0x1000, SectionKind::Text, vec![0xbf00_bf00; 32]),
            ],
        );
        binary.kind = kind;
        binary
    }

    #[test]
    fn a_table_that_does_not_hold_the_entry_point_is_at_a_section_start() {
        let table = VectorTable {
            address: 0x900,
            size: 16,
            handlers: vec![0x1040, 0x1044],
            exceptions: vec![1, 3],
        };
        let binary = entry_point_is_main(FileKind::Executable);
        assert_eq!(find(&binary), Some(table));
        assert_eq!(find(&binary).unwrap().reset_handler(), Some(0x1040));
    }

    #[test]
    fn only_executables_linked_at_one_address_have_a_table_found_by_how_it_begins() {
        for kind in [
            FileKind::PositionIndependentExecutable,
            FileKind::SharedLibrary,
        ] {
            assert_eq!(find(&entry_point_is_main(kind)), None);
        }
    }

    #[test]
    fn a_table_can_begin_a_section_of_code() {
        // As in pico-sdk, `.text` begins with the table, and the entry
        // point is code after it.
        let mut text = vec![0x2000_1000, 0x1015, 0x1019, 0x1019];
        text.extend([0xbf00_bf00; 8]);
        let binary = firmware(0x1010, &[(".text", 0x1000, SectionKind::Text, text)]);
        let table = find(&binary).unwrap();
        assert_eq!(
            (table.address, table.reset_handler()),
            (0x1000, Some(0x1014))
        );
    }

    #[test]
    fn an_unwind_table_does_not_hold_the_reset_entry() {
        // Linked by LLD with no entry point, firmware's entry point is 0,
        // and `.ARM.exidx` can begin with an entry that says a function
        // can't be unwound: an offset, then 1, the Thumb address of 0.
        let mut text = vec![0x2000_1000, 0x41, 0x45, 0x45];
        text.extend([0xbf00_bf00; 16]);
        let binary = firmware(
            0,
            &[
                (".text", 0, SectionKind::Text, text),
                (
                    ".ARM.exidx",
                    0x100,
                    SectionKind::Unknown,
                    vec![0x7fff_ff5c, 1],
                ),
            ],
        );
        let table = find(&binary).unwrap();
        assert_eq!((table.address, table.reset_handler()), (0, Some(0x40)));
    }

    #[test]
    fn a_table_of_handlers_and_their_arguments_does_not_pass_for_one() {
        // Built to start after a boot loader, Zephyr firmware begins with
        // 0s and then the vector table. `_sw_isr_table`, after it, holds
        // each interrupt's argument and handler: for an interrupt that
        // isn't used, 0 and a handler that reports it.
        let mut rom_start = vec![0; 4];
        rom_start.extend([0x2000_1000, 0x1041, 0x1045, 0x1045]);
        let sections = |kind| {
            [
                ("rom_start", 0x800, kind, rom_start.clone()),
                (
                    "sw_isr_table",
                    0x900,
                    SectionKind::ReadOnlyData,
                    vec![0, 0x1049, 0, 0x1049],
                ),
                (".text", 0x1000, SectionKind::Text, vec![0xbf00_bf00; 32]),
            ]
        };
        // In data, the word that holds the entry point is the reset entry.
        let binary = firmware(0x1040, &sections(SectionKind::ReadOnlyData));
        assert_eq!(find(&binary).map(|table| table.address), Some(0x810));
        // In code, that word isn't read, and elfq finds no table rather
        // than a wrong one.
        let binary = firmware(0x1040, &sections(SectionKind::Text));
        assert_eq!(find(&binary), None);
    }

    #[test]
    fn a_section_starts_a_table_only_if_its_first_four_words_could() {
        // `.isr` ends where `.text` starts, and `.text` begins with 0s, so
        // that the words after a short `.isr` would do for NMI's and
        // HardFault's.
        let starts = |words: Vec<u32>| {
            let isr = 0x1000 - 4 * words.len() as u64;
            let sections = [
                (".isr", isr, SectionKind::ReadOnlyData, words),
                (".text", 0x1000, SectionKind::Text, vec![0; 32]),
            ];
            find(&firmware(0x1050, &sections)).is_some()
        };
        assert!(starts(vec![0x2000_1000, 0x1041, 0x1041, 0x1041]));
        // The stack pointer isn't a multiple of 4, is 0, or points into
        // code, as a pointer to a constant in flash would, not into RAM.
        assert!(!starts(vec![0x2000_1002, 0x1041, 0x1041, 0x1041]));
        assert!(!starts(vec![0, 0x1041, 0, 0x1041]));
        assert!(!starts(vec![0x1004, 0x1041, 0x1041, 0x1041]));
        // The reset entry is 0, or an even address.
        assert!(!starts(vec![0x2000_1000, 0, 0x1041, 0x1041]));
        assert!(!starts(vec![0x2000_1000, 0x1040, 0x1041, 0x1041]));
        // NMI's or HardFault's entry is odd, but outside code: in `.isr`.
        assert!(!starts(vec![0x2000_1000, 0x1041, 0x0ff1, 0x1041]));
        assert!(!starts(vec![0x2000_1000, 0x1041, 0x1041, 0x0ff1]));
        // Too short to hold four words.
        assert!(!starts(vec![0x2000_1000, 0x1041, 0x1041]));
    }

    #[test]
    fn the_first_section_by_address_that_begins_like_a_table_holds_it() {
        // `.ram_vectors` comes first in the file, but `.isr` by address.
        let ram_vectors = vec![0x2000_1000, 0x1045, 0x1045, 0x1045];
        let isr = vec![0x2000_1000, 0x1041, 0, 0x1045];
        let binary = firmware(
            0x1050,
            &[
                (".ram_vectors", 0xa00, SectionKind::Data, ram_vectors),
                (".isr", 0x900, SectionKind::ReadOnlyData, isr),
                (".text", 0x1000, SectionKind::Text, vec![0xbf00_bf00; 32]),
            ],
        );
        assert_eq!(find(&binary).map(|table| table.address), Some(0x900));
    }

    #[test]
    fn only_sections_of_code_or_data_begin_a_table() {
        // A Linux program's `.gnu.version`, a list of 16-bit numbers, can
        // begin like a table.
        let versions = vec![0x2000_1000, 0x1041, 0x1041, 0x1041];
        let binary = firmware(
            0x1050,
            &[
                (".gnu.version", 0x800, SectionKind::Unknown, versions),
                (".text", 0x1000, SectionKind::Text, vec![0xbf00_bf00; 32]),
            ],
        );
        assert_eq!(find(&binary), None);
    }

    #[test]
    fn sections_that_startup_code_copies_begin_a_table_last() {
        // `.data` comes first by address, but startup code copies it into
        // place, after the processor has read the table at reset. It begins
        // like one: a pointer, then a callback.
        let data = vec![0x4018_4000, 0x1041, 0, 0];
        let isr = vec![0x2000_1000, 0x1041, 0, 0x1045];
        let code = vec![0xbf00_bf00; 32];
        let mut binary = firmware(
            0x1050,
            &[
                (".data", 0x700, SectionKind::Data, data.clone()),
                (".isr", 0x900, SectionKind::ReadOnlyData, isr),
                (".text", 0x1000, SectionKind::Text, code.clone()),
            ],
        );
        binary.sections[0].load_address = Some(0x1100);
        assert_eq!(find(&binary).map(|table| table.address), Some(0x900));
        // But when no other section begins like a table, as when a linker
        // script puts the table in RAM, the copy holds it.
        let mut binary = firmware(
            0x1050,
            &[
                (".data", 0x700, SectionKind::Data, data),
                (".text", 0x1000, SectionKind::Text, code),
            ],
        );
        binary.sections[0].load_address = Some(0x1100);
        assert_eq!(find(&binary).map(|table| table.address), Some(0x700));
    }

    #[test]
    fn a_word_that_holds_the_entry_point_where_a_table_starts_holds_its_reset_entry() {
        // `.rodata` begins like a table too, but `.isr`'s reset entry holds
        // the entry point.
        let rodata = vec![0x2000_1000, 0x1045, 0x1045, 0x1045];
        let isr = vec![0x2000_1000, 0x1041, 0, 0x1045];
        let binary = firmware(
            0x1040,
            &[
                (".rodata", 0x800, SectionKind::ReadOnlyData, rodata),
                (".isr", 0x900, SectionKind::ReadOnlyData, isr),
                (".text", 0x1000, SectionKind::Text, vec![0xbf00_bf00; 32]),
            ],
        );
        assert_eq!(find(&binary).map(|table| table.address), Some(0x900));
    }

    #[test]
    fn a_word_in_the_middle_of_a_section_holds_the_reset_entry_last() {
        // A pointer to `main`, the entry point, after a pointer to data.
        let rodata = vec![0x1041, 0x1041, 0x800, 0x1051];
        let isr = vec![0x2000_1000, 0x1041, 0, 0x1045];
        let code = vec![0xbf00_bf00; 32];
        let binary = firmware(
            0x1050,
            &[
                (".rodata", 0x800, SectionKind::ReadOnlyData, rodata.clone()),
                (".isr", 0x900, SectionKind::ReadOnlyData, isr),
                (".text", 0x1000, SectionKind::Text, code.clone()),
            ],
        );
        assert_eq!(find(&binary).map(|table| table.address), Some(0x900));
        // When no section begins like a table, the pointer is the reset
        // entry.
        let binary = firmware(
            0x1050,
            &[
                (".rodata", 0x800, SectionKind::ReadOnlyData, rodata),
                (".text", 0x1000, SectionKind::Text, code),
            ],
        );
        assert_eq!(find(&binary).map(|table| table.address), Some(0x808));
    }

    #[test]
    fn the_reset_entry_follows_a_word_that_could_be_the_stack_pointer() {
        // The entry point is NMI's handler, as when a linker script names
        // none and `.text` begins with it. NMI's and HardFault's entries
        // follow odd words, the reset entry's and NMI's, and entry 11
        // follows a reserved entry that holds 0, so none is the reset
        // entry, and the table is where it begins `.isr`.
        let mut isr = vec![0x2000_1000, 0x1045, 0x1041, 0x1041];
        isr.extend([0; 7]);
        isr.push(0x1041);
        let code = vec![0xbf00_bf00; 32];
        let binary = firmware(
            0x1040,
            &[
                (".isr", 0x900, SectionKind::ReadOnlyData, isr.clone()),
                (".text", 0x1000, SectionKind::Text, code.clone()),
            ],
        );
        let table = find(&binary).unwrap();
        assert_eq!(
            (table.address, table.reset_handler()),
            (0x900, Some(0x1044))
        );
        // Where the table doesn't begin its section, elfq finds none,
        // rather than one that starts a word late.
        isr.insert(0, 1);
        let binary = firmware(
            0x1040,
            &[
                (".rodata", 0x8fc, SectionKind::ReadOnlyData, isr),
                (".text", 0x1000, SectionKind::Text, code),
            ],
        );
        assert_eq!(find(&binary), None);
    }

    #[test]
    fn the_reset_handler_is_the_code_its_entry_holds() {
        let table = |exceptions: Vec<u64>| VectorTable {
            address: 0,
            size: 16,
            handlers: vec![0x100, 0x200],
            exceptions,
        };
        assert_eq!(table(vec![1, 3]).reset_handler(), Some(0x100));
        // The reset entry holds no address of code, as when it is a word
        // that holds the address of an entry point outside code.
        assert_eq!(table(vec![2, 3]).reset_handler(), None);
    }

    #[test]
    fn a_handler_runs_in_normal_operation_when_an_entry_for_it_is_routine() {
        let table = VectorTable {
            address: 0,
            size: 128,
            handlers: vec![0x100, 0x200, 0x200, 0x300, 0x300, 0x400, 0x500, 0x500],
            exceptions: vec![1, 2, 3, 4, 12, 11, 6, 20],
        };
        // SVCall, and an interrupt with a fault.
        assert!(table.runs_in_normal_operation(0x400));
        assert!(table.runs_in_normal_operation(0x500));
        // Reset; NMI and HardFault; MemManage and DebugMonitor.
        assert!(!table.runs_in_normal_operation(0x100));
        assert!(!table.runs_in_normal_operation(0x200));
        assert!(!table.runs_in_normal_operation(0x300));
        // No entry lists it.
        assert!(!table.runs_in_normal_operation(0x600));
    }

    #[test]
    fn exceptions_are_named_by_their_entries() {
        let names = |version: Option<u64>| {
            let architecture = version.map(|version| ArmArchitecture {
                version,
                profile: Some(b'M'),
            });
            (0..18)
                .map(|number| exception_name(number, architecture))
                .collect::<Vec<String>>()
        };
        // Armv7E-M, as when the file doesn't say.
        let armv7 = [
            "initial SP",
            "Reset",
            "NMI",
            "HardFault",
            "MemManage",
            "BusFault",
            "UsageFault",
            "reserved",
            "reserved",
            "reserved",
            "reserved",
            "SVCall",
            "DebugMonitor",
            "reserved",
            "PendSV",
            "SysTick",
            "IRQ 0",
            "IRQ 1",
        ];
        assert_eq!(names(Some(13)), armv7);
        assert_eq!(names(None), armv7);
        // Armv6S-M, as on a Cortex-M0, has no faults but HardFault, and no
        // DebugMonitor.
        let mut armv6 = armv7;
        for number in [4, 5, 6, 12] {
            armv6[number] = "reserved";
        }
        assert_eq!(names(Some(12)), armv6);
        // Armv8-M Mainline, as on a Cortex-M33, adds SecureFault.
        let mut armv8 = armv7;
        armv8[7] = "SecureFault";
        assert_eq!(names(Some(17)), armv8);
    }
}
