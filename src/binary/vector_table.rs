//! The Cortex-M vector table, from which the processor takes the initial
//! stack pointer at reset and the address of the handler for each
//! exception and interrupt.
//!
//! The table is a list of words: the initial stack pointer, then the
//! address of the reset handler, which is the entry point (plus 1, for
//! Thumb), then the other handlers. Reserved entries are 0, or on some parts
//! another word, such as the checksum that NXP's LPC boot ROM checks in
//! entry 7. Linker scripts name its section differently (`.isr_vector`,
//! `.vector_table`), so it is found by its shape: the first word in data
//! that holds the entry point's Thumb address is taken for the reset entry.

use object::SectionKind;

use super::{Binary, code_section};

/// Where the vector table is and the handlers it lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorTable {
    /// The address of its first word, the initial stack pointer.
    pub address: u64,
    /// Its size in bytes: up to the first entry outside the reserved ones
    /// that is neither 0 nor the address of Thumb code, or the end of its
    /// section.
    pub size: u64,
    /// Where each handler's code starts, without the Thumb bit, in the
    /// table's order: first the reset handler, which is the entry point. A
    /// handler for several exceptions is listed once for each.
    pub handlers: Vec<u64>,
}

/// The entries the architecture reserves, counted from the stack pointer's.
const RESERVED: [u64; 5] = [7, 8, 9, 10, 13];

/// Finds the vector table in a Thumb file, if it has one.
pub(super) fn find(binary: &Binary) -> Option<VectorTable> {
    let size = binary.pointer_size;
    let (section, reset) = binary.sections.iter().find_map(|section| {
        if !section.allocated || section.kind == SectionKind::Text {
            return None;
        }
        // The reset entry follows the stack pointer, so it can't be first.
        let mut slot = section.address.next_multiple_of(size) + size;
        while slot + size <= section.address + section.size {
            if binary.pointer_at(slot) == Some(binary.entry | 1) {
                return Some((section, slot));
            }
            slot += size;
        }
        None
    })?;

    let address = reset - size;
    let end = section.address + section.size;
    let mut handlers = Vec::new();
    let mut slot = reset;
    while slot + size <= end {
        let Some(value) = binary.pointer_at(slot) else {
            break;
        };
        if value & 1 == 1 && code_section(&binary.sections, value & !1).is_some() {
            handlers.push(value & !1);
        } else if value != 0 && !RESERVED.contains(&((slot - address) / size)) {
            break;
        }
        slot += size;
    }
    Some(VectorTable {
        address,
        size: slot - address,
        handlers,
    })
}
