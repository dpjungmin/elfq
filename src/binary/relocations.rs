//! Dynamic relocations: the addresses that the dynamic loader writes into
//! memory when the program starts.
//!
//! A position-independent program learns where it was loaded only when it
//! runs, so its file lists every place that holds an address and the loader
//! fixes each one up. For elfq that list is an exact map of the pointers
//! stored in data. Pointers into shared libraries name what the program
//! imports.

use std::collections::HashMap;

use object::{
    Architecture, Object, ObjectSymbol, ObjectSymbolTable, RelocationFlags, SymbolKind, elf,
};

use super::{Relocation, RelocationTarget, code_address};

/// Returns the imports, and the relocations sorted by address.
pub(super) fn read(
    file: &object::File,
    architecture: Architecture,
) -> (Vec<String>, Vec<Relocation>) {
    let mut imports = Vec::new();
    let mut relocations = Vec::new();
    let (Some(all), Some(symbols)) = (file.dynamic_relocations(), file.dynamic_symbol_table())
    else {
        return (imports, relocations);
    };
    // Import names, as indices into `imports`.
    let mut import_index: HashMap<String, usize> = HashMap::new();

    for (address, relocation) in all {
        let RelocationFlags::Elf { r_type } = relocation.flags() else {
            continue;
        };
        if !stores_address(architecture, r_type) {
            continue;
        }
        let target = match relocation.target() {
            // The pointer's value is the addend: an address in this file.
            object::RelocationTarget::Absolute => {
                RelocationTarget::Address(relocation.addend() as u64)
            }
            object::RelocationTarget::Symbol(index) => {
                let Ok(symbol) = symbols.symbol_by_index(index) else {
                    continue;
                };
                if symbol.is_undefined() {
                    let Ok(name) = symbol.name() else {
                        continue;
                    };
                    let index = *import_index.entry(name.to_string()).or_insert_with(|| {
                        imports.push(name.to_string());
                        imports.len() - 1
                    });
                    RelocationTarget::Import(index)
                } else {
                    let mut target = symbol.address().wrapping_add_signed(relocation.addend());
                    if symbol.kind() == SymbolKind::Text {
                        target = code_address(architecture, target);
                    }
                    RelocationTarget::Address(target)
                }
            }
            _ => continue,
        };
        relocations.push(Relocation { address, target });
    }
    relocations.sort_by_key(|relocation| relocation.address);
    (imports, relocations)
}

/// Whether a relocation type stores an address, as opposed to something like
/// a thread-local offset or a copy of a variable. Only x86-64 for now.
fn stores_address(architecture: Architecture, r_type: elf::RelocationType) -> bool {
    match architecture {
        Architecture::X86_64 => matches!(
            r_type,
            elf::R_X86_64_64
                | elf::R_X86_64_GLOB_DAT
                | elf::R_X86_64_JUMP_SLOT
                | elf::R_X86_64_RELATIVE
                | elf::R_X86_64_IRELATIVE
        ),
        _ => false,
    }
}
