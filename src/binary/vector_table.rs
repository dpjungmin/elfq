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

use super::{Binary, Item, ItemKind, Source, code_section};

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
}

/// The entries the architecture reserves, counted from the stack pointer's.
const RESERVED: [u64; 5] = [7, 8, 9, 10, 13];

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
            warnings: Vec::new(),
            code_references: std::collections::HashMap::new(),
            data: text,
        };
        let table = VectorTable {
            address: 0x1000,
            size: 28,
            handlers: vec![0x1030; 6],
        };
        assert_eq!(find(&binary), Some(table));
    }
}
