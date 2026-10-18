//! Items for data that has no symbol: vtables and string literals, which
//! compilers leave unnamed, and in stripped files every variable.
//!
//! Nothing in the file says where such data is, so it is found by following
//! references. An address that code uses, or that a stored pointer holds,
//! starts a data object when no item contains it. The object runs to the
//! next such address, the next item or the end of its section, whichever
//! comes first, so its size is an estimate. It is named after its section
//! and address, as in `.rodata@0x2004`: elfq doesn't guess which function
//! it belongs to.
//!
//! Firmware has no list of stored pointers, and a word in its data could be
//! a pointer or just a number, so there only code finds unnamed data.

use object::SectionKind;

use super::{Binary, Item, ItemKind, RelocationTarget, Section, Source};
use crate::arch::CodeReferenceKind;

/// Returns an item for each unnamed data object, sorted by address.
pub(super) fn items(binary: &Binary) -> Vec<Item> {
    let mut starts: Vec<u64> = referenced_addresses(binary)
        .into_iter()
        // Zero is a null pointer, or just a number.
        .filter(|&address| address != 0 && binary.item_at(address).is_none())
        .filter(|&address| {
            binary.section_at(address).is_some_and(|index| {
                let section = &binary.sections[index];
                // In Thumb code, what no function covers is constants, as
                // when a linker script puts `.rodata` in `.text`.
                holds_data(section) || (binary.thumb && section.kind == SectionKind::Text)
            })
        })
        .collect();
    starts.sort_unstable();
    starts.dedup();

    let mut items = Vec::new();
    for (i, &start) in starts.iter().enumerate() {
        let Some(index) = binary.section_at(start) else {
            continue;
        };
        let section = &binary.sections[index];
        let next_start = starts.get(i + 1).copied().unwrap_or(u64::MAX);
        let next_item = binary
            .items
            .get(binary.items.partition_point(|item| item.address <= start))
            .map_or(u64::MAX, |item| item.address);
        let end = (section.address + section.size)
            .min(next_start)
            .min(next_item);
        items.push(Item {
            name: format!("{}@{start:#x}", section.name),
            aliases: Vec::new(),
            kind: ItemKind::Data,
            address: start,
            size: end - start,
            size_estimated: true,
            section: index,
            source: Source::Reference,
        });
    }
    items
}

/// Every address that code uses or a stored pointer holds, in no order and
/// with repeats.
fn referenced_addresses(binary: &Binary) -> Vec<u64> {
    let mut addresses = Vec::new();
    for function in binary
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Function)
    {
        for reference in binary.code_references(function) {
            match reference.kind {
                CodeReferenceKind::Address(address)
                | CodeReferenceKind::Memory(address)
                | CodeReferenceKind::CallThrough(address)
                | CodeReferenceKind::JumpThrough(address) => addresses.push(address),
                _ => {}
            }
        }
    }
    for relocation in &binary.relocations {
        if let RelocationTarget::Address(address) = relocation.target {
            addresses.push(address);
        }
    }
    addresses
}

/// Whether `section` holds data, whose unnamed objects get items. Not code,
/// though in Thumb files the constants in code get items too (see
/// `items`), and not the global offset table: elfq follows the pointers
/// stored there to what they point at instead.
pub(super) fn holds_data(section: &Section) -> bool {
    matches!(
        section.kind,
        SectionKind::Data
            | SectionKind::ReadOnlyData
            | SectionKind::ReadOnlyString
            | SectionKind::UninitializedData
    ) && !matches!(section.name.as_str(), ".got" | ".got.plt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_data_sections_hold_unnamed_data() {
        let section = |name: &str, kind| Section {
            name: name.to_string(),
            address: 0x1000,
            size: 0x100,
            kind,
            allocated: true,
            file_range: None,
            entry_size: 0,
        };
        assert!(holds_data(&section(".rodata", SectionKind::ReadOnlyData)));
        assert!(holds_data(&section(".data.rel.ro", SectionKind::Data)));
        assert!(holds_data(&section(".bss", SectionKind::UninitializedData)));
        assert!(!holds_data(&section(".text", SectionKind::Text)));
        assert!(!holds_data(&section(".got", SectionKind::Data)));
        assert!(!holds_data(&section(".tdata", SectionKind::Tls)));
    }
}
