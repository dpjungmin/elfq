//! Names for PLT stubs.
//!
//! Code calls a function in a shared library through a stub in the procedure
//! linkage table (`.plt`, `.plt.sec` or `.plt.got`), which jumps through a
//! pointer that the dynamic loader fills in. No symbol names the stubs, so
//! elfq names each one after the import its pointer leads to, as `objdump`
//! does: `write@plt`.
//!
//! Code also calls some of the file's own functions through stubs, which
//! elfq names after the function:
//!
//! - a shared library calls the functions it exports through stubs, so
//!   that another file can replace them: `log_msg@plt` jumps to the
//!   library's own `log_msg`, unless the program defines one too;
//! - any file calls an ifunc, a function that picks its code when the file
//!   is loaded, through a stub whose pointer the loader sets to the code
//!   the ifunc's resolver picks. The ifunc's symbol is at the resolver, so
//!   the stub is named after the function there: `strlen@plt` in glibc.
//!   When no symbol is at the resolver, as in a stripped library, elfq
//!   makes no item for the stub.

use std::collections::HashMap;

use object::Architecture;

use super::{Binary, Item, ItemKind, RelocationTarget, Section, Source};

/// Returns a function for each stub, and one for the rest of each table
/// before its first stub: the header that lazy binding uses, named after its
/// section (`.plt`). `symbols` are the items from the symbol tables, sorted
/// by kind, then by address, which name the stubs for the file's own
/// functions.
pub(super) fn stubs(binary: &Binary, symbols: &[Item]) -> Vec<Item> {
    if binary.architecture != Architecture::X86_64 {
        return Vec::new();
    }
    let mut items = Vec::new();
    for (index, section) in binary.sections.iter().enumerate() {
        if !section.allocated || !section.name.starts_with(".plt") {
            continue;
        }
        let Some(code) = binary.bytes(section.address, section.size) else {
            continue;
        };
        let entry_size = entry_size(section);
        let mut first_stub = None;
        for (number, entry) in code.chunks(entry_size as usize).enumerate() {
            let address = section.address + number as u64 * entry_size;
            let (name, aliases) = match target(binary, entry, address) {
                Some(RelocationTarget::Import(import)) => (&binary.imports[import], &[][..]),
                Some(RelocationTarget::Address(target)) => match function_at(symbols, target) {
                    Some(function) => (&function.name, &function.aliases[..]),
                    None => continue,
                },
                None => continue,
            };
            first_stub.get_or_insert(address);
            let mut stub = function(format!("{name}@plt"), address, entry.len() as u64, index);
            // So that `realloc@plt` finds glibc's stub for `__libc_realloc`,
            // whose alias it is, as objdump names it.
            stub.aliases = aliases.iter().map(|alias| format!("{alias}@plt")).collect();
            items.push(stub);
        }
        let header_end = first_stub.unwrap_or(section.address + section.size);
        if header_end > section.address {
            items.push(function(
                section.name.clone(),
                section.address,
                header_end - section.address,
                index,
            ));
        }
    }
    items
}

/// The stubs for the file's own functions, each with that function, as
/// indices into [`Binary::items`].
pub(super) fn own_function_stubs(binary: &Binary) -> HashMap<usize, usize> {
    let mut stubs = HashMap::new();
    for (index, stub) in binary.items.iter().enumerate() {
        // Headers are named after their section.
        let Some(name) = stub.name.strip_suffix("@plt") else {
            continue;
        };
        if stub.source != Source::Plt {
            continue;
        }
        let Some(entry) = binary.bytes(stub.address, stub.size) else {
            continue;
        };
        let Some(RelocationTarget::Address(target)) = target(binary, entry, stub.address) else {
            continue;
        };
        // The function that `stubs` named the stub after.
        let first = binary.items.partition_point(|item| item.address < target);
        let function = binary.items[first..]
            .iter()
            .take_while(|item| item.address == target)
            .position(|item| item.kind == ItemKind::Function && item.name == name);
        if let Some(function) = function {
            stubs.insert(index, first + function);
        }
    }
    stubs
}

/// Where the PLT entry at `address` jumps: what the dynamic loader stores in
/// the pointer it jumps through, as a relocation says. `None` when no
/// relocation fills that pointer, as for the header, whose pointer leads to
/// the loader's own code.
fn target(binary: &Binary, entry: &[u8], address: u64) -> Option<RelocationTarget> {
    let slot = jump_slot(entry, address)?;
    Some(binary.relocation_at(slot)?.target)
}

/// The function among `symbols`, sorted by kind and then by address, that
/// starts at `address`.
fn function_at(symbols: &[Item], address: u64) -> Option<&Item> {
    let index = symbols
        .binary_search_by_key(&(ItemKind::Function, address), |item| {
            (item.kind, item.address)
        })
        .ok()?;
    Some(&symbols[index])
}

/// How long each entry in a PLT section is: 16 bytes, except in a
/// `.plt.got` that GNU ld makes without Intel CET, whose entries are 8 bytes
/// long, as its header says.
fn entry_size(section: &Section) -> u64 {
    if section.entry_size == 8 { 8 } else { 16 }
}

/// The address of the pointer that an x86-64 PLT entry jumps through, from
/// its `jmp [rip + offset]` instruction (bytes `ff 25`, then a 32-bit offset
/// from the end of the instruction).
fn jump_slot(entry: &[u8], address: u64) -> Option<u64> {
    let at = entry.windows(2).position(|bytes| bytes == [0xff, 0x25])?;
    let offset = i32::from_le_bytes(entry.get(at + 2..at + 6)?.try_into().ok()?);
    let next_instruction = address + at as u64 + 6;
    Some(next_instruction.wrapping_add_signed(offset.into()))
}

fn function(name: String, address: u64, size: u64, section: usize) -> Item {
    Item {
        name,
        aliases: Vec::new(),
        kind: ItemKind::Function,
        address,
        size,
        size_estimated: false,
        section,
        source: Source::Plt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_pointer_a_stub_jumps_through() {
        // endbr64; jmp [rip + 0x2f3e]; nop: `write@plt` in c-x86_64.elf.
        let entry = [
            0xf3, 0x0f, 0x1e, 0xfa, 0xff, 0x25, 0x3e, 0x2f, 0x00, 0x00, 0x66, 0x0f, 0x1f, 0x44,
            0x00, 0x00,
        ];
        assert_eq!(jump_slot(&entry, 0x1070), Some(0x3fb8));
        // endbr64; push 0; jmp .plt: a lazy-binding entry with no pointer.
        let entry = [
            0xf3, 0x0f, 0x1e, 0xfa, 0x68, 0x00, 0x00, 0x00, 0x00, 0xe9, 0xe2, 0xff, 0xff, 0xff,
            0x66, 0x90,
        ];
        assert_eq!(jump_slot(&entry, 0x1030), None);
    }
}
