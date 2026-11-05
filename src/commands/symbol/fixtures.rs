//! What the unit tests of `symbol` build their files from: an executable
//! with a function for each of a list of names ([`functions`]).

use object::SectionKind;

use crate::binary::{Binary, Item, ItemKind, Section, Source};

/// An x86-64 executable whose `.text`, from 0x1000, holds a function of
/// 0x10 bytes for each of `names`, in that order, with no aliases. A name
/// that ends with `@plt` is a PLT stub's.
pub(super) fn functions<S: AsRef<str>>(names: &[S]) -> Binary {
    let items: Vec<Item> = (names.iter().enumerate())
        .map(|(index, name)| Item {
            source: if name.as_ref().ends_with("@plt") {
                Source::Plt
            } else {
                Source::SymbolTable
            },
            name: name.as_ref().to_string(),
            aliases: Vec::new(),
            kind: ItemKind::Function,
            address: 0x1000 + 0x10 * index as u64,
            size: 0x10,
            size_estimated: false,
            section: 1,
        })
        .collect();
    let section = |name: &str, address, size, kind| Section {
        name: name.to_string(),
        address,
        size,
        kind,
        allocated: address != 0,
        file_range: None,
        entry_size: 0,
        load_address: None,
    };
    let text_size = 0x10 * items.len() as u64;
    let sections = vec![
        section("", 0, 0, SectionKind::Unknown),
        section(".text", 0x1000, text_size, SectionKind::Text),
    ];
    Binary::for_tests(sections, items, Vec::new())
}
