//! The Cortex-M vector table, from which the processor takes the initial
//! stack pointer at reset and the address of the handler for each
//! exception and interrupt.
//!
//! The table is a list of words: the initial stack pointer, then the
//! address of the reset handler, which is the entry point (plus 1, for
//! Thumb), then the other handlers. Reserved entries are 0, or on some parts
//! another word, such as the checksum that NXP's LPC boot ROM checks in
//! entry 7. Linker scripts name its section differently (`.isr_vector`,
//! `.vector_table`) or put it first in `.text`, so `find` finds it by its
//! shape, and its entries tell where it ends (see `VectorTable::size`).

use object::SectionKind;

use super::{ArmArchitecture, Binary, Item, ItemKind, Source, code_section};

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
    /// table's order: first the reset handler, which is the entry point. A
    /// handler for several exceptions is listed once for each.
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

/// Finds the vector table in a Thumb file, if it has one, by its shape:
///
/// 1. Allocated sections are tried in the order the file lists them.
/// 2. In each, the words at multiples of the pointer size are read from the
///    second one on, since the stack pointer comes first. In a code section,
///    only that second word is read, so that no constant in a function is
///    taken for the reset entry.
/// 3. The first word that holds the entry point's Thumb address, the entry
///    point plus 1, is the reset entry, and the table starts one word
///    before it.
/// 4. The entries from there tell where it ends (see `VectorTable::size`).
pub(super) fn find(binary: &Binary) -> Option<VectorTable> {
    let size = binary.pointer_size;
    let (section, reset) = binary.sections.iter().find_map(|section| {
        if !section.allocated {
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
            .find(|&(_, value)| value == binary.entry | 1)?;
        Some((section, slot))
    })?;

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
        if value & 1 == 1 && code_section(&binary.sections, value & !1).is_some() {
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
    use crate::binary::{FileKind, Section};

    #[test]
    fn a_short_table_in_code_ends_at_its_last_handler() {
        // The stack pointer, then the reset handler and five fault handlers,
        // all at 0x1030. Five words of code follow, where entries 7 to 10
        // would be reserved, and then the reset handler's code.
        let mut words = vec![0x2000_1000_u32];
        words.extend([0x1031; 6]);
        words.extend([0xbf00_bf00; 5]);
        words.push(0xe7fe_e7fe);
        let text: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        let binary = Binary {
            architecture: Architecture::Arm,
            pointer_size: 4,
            kind: FileKind::Executable,
            entry: 0x1030,
            thumb: true,
            endianness: Endianness::Little,
            has_symbols: false,
            has_dwarf: false,
            sections: vec![Section {
                name: ".text".to_string(),
                address: 0x1000,
                size: text.len() as u64,
                kind: SectionKind::Text,
                allocated: true,
                file_range: Some((0, text.len() as u64)),
                entry_size: 0,
                load_address: None,
            }],
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
            data: text,
        };
        let table = VectorTable {
            address: 0x1000,
            size: 28,
            handlers: vec![0x1030; 6],
            exceptions: vec![1, 2, 3, 4, 5, 6],
        };
        assert_eq!(find(&binary), Some(table));
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
