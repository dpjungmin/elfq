//! What the startup walk (see `arch::startup`) reads of a file: its code as
//! it runs, what memory holds at reset, and where its functions are.

use object::SectionKind;

use super::{Binary, ItemKind, word};
use crate::arch::startup::Program;

impl Program for Binary {
    fn code(&self, address: u64) -> Option<&[u8]> {
        let section = &self.sections[self.section_at(address)?];
        if section.kind != SectionKind::Text {
            return None;
        }
        let rest = section.address + section.size - address;
        self.section_bytes(section, address, rest)
    }

    fn word_at_reset(&self, address: u64) -> Option<u32> {
        // A section that the file stores where the program uses it, as in
        // flash, or a load image, where the file stores a section's bytes
        // for startup code to copy. Other sections, such as `.data` and
        // `.bss`, hold nothing at reset.
        let stored = self
            .section_at(address)
            .map(|index| (&self.sections[index], address))
            .filter(|(section, _)| section.load_address.is_none());
        let loaded = || {
            self.sections.iter().find_map(|section| {
                let start = section.load_address?;
                let offset = address.checked_sub(start)?;
                (offset < section.size).then(|| (section, section.address + offset))
            })
        };
        let (section, at) = stored.or_else(loaded)?;
        let bytes = self.section_bytes(section, at, 4)?;
        (bytes.len() == 4).then(|| word(bytes, self.endianness) as u32)
    }

    fn function_at(&self, address: u64) -> Option<(u64, u64)> {
        let item = &self.items[self.item_at(address)?];
        (item.kind == ItemKind::Function && item.size > 0)
            .then(|| (item.address, item.address + item.size))
    }
}
