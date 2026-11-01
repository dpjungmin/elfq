//! The unwind table, `.eh_frame`: function ranges and personality routines.
//!
//! On x86-64, compilers describe almost every function in `.eh_frame` so that
//! exceptions and debuggers can walk the stack. Programs need the table at run
//! time, so stripping keeps it, which makes it the best source of function
//! boundaries when a file has no symbols. Firmware usually has no
//! `.eh_frame` at all.
//!
//! The table also names *personality routines*, such as
//! `rust_eh_personality`, which the unwinder calls while it handles a panic
//! or an exception. Nothing in the code calls them, so
//! [`used_by_unwinder`] says where they are, for
//! [`Binary::used_by_unwinder`].

use anyhow::Result;
use gimli::{BaseAddresses, CieOrFde, EhFrame, Pointer, RunTimeEndian, UnwindSection};
use object::{Object, ObjectSection};

use super::{Binary, RelocationTarget};

/// What elfq reads from the unwind table.
#[derive(Default)]
pub(super) struct UnwindTable {
    /// `(start, length)` for every function the table describes.
    pub functions: Vec<(u64, u64)>,
    /// The personality routines: their addresses, or for an indirect
    /// pointer, the address where their address is stored.
    pub personalities: Vec<Pointer>,
}

/// Reads the unwind table, which is empty when the file has no `.eh_frame`.
pub(super) fn read(file: &object::File) -> Result<UnwindTable> {
    let mut table = UnwindTable::default();
    let Some(section) = file.section_by_name(".eh_frame") else {
        return Ok(table);
    };
    let endian = if file.is_little_endian() {
        RunTimeEndian::Little
    } else {
        RunTimeEndian::Big
    };
    let mut eh_frame = EhFrame::new(section.data()?, endian);
    eh_frame.set_address_size(if file.is_64() { 8 } else { 4 });

    // Pointers in the table can be relative to these sections.
    let mut bases = BaseAddresses::default().set_eh_frame(section.address());
    if let Some(text) = file.section_by_name(".text") {
        bases = bases.set_text(text.address());
    }
    if let Some(got) = file.section_by_name(".got") {
        bases = bases.set_got(got.address());
    }

    let mut entries = eh_frame.entries(&bases);
    while let Some(entry) = entries.next()? {
        match entry {
            CieOrFde::Cie(cie) => table.personalities.extend(cie.personality()),
            CieOrFde::Fde(partial) => {
                let fde = partial.parse(EhFrame::cie_from_offset)?;
                table.functions.push((fde.initial_address(), fde.len()));
            }
        }
    }
    table
        .personalities
        .sort_unstable_by_key(|pointer| match *pointer {
            Pointer::Direct(address) | Pointer::Indirect(address) => address,
        });
    table.personalities.dedup();
    Ok(table)
}

/// The addresses that the unwinder uses, given the table's pointers to the
/// personality routines: where each routine is, unless it is in a shared
/// library, and where an indirect pointer to it is stored.
pub(super) fn used_by_unwinder(binary: &Binary, personalities: &[Pointer]) -> Vec<u64> {
    let mut used = Vec::new();
    for &pointer in personalities {
        if let Pointer::Indirect(stored_at) = pointer {
            used.push(stored_at);
        }
        used.extend(personality_address(binary, pointer));
    }
    used
}

/// Where a personality routine is, given the unwind table's pointer to
/// it. `None` when it is in a shared library.
fn personality_address(binary: &Binary, pointer: Pointer) -> Option<u64> {
    let stored_at = match pointer {
        Pointer::Direct(address) => return Some(address),
        Pointer::Indirect(stored_at) => stored_at,
    };
    match binary.relocation_at(stored_at) {
        Some(relocation) => match relocation.target {
            RelocationTarget::Address(address) => Some(address),
            RelocationTarget::Import(_) => None,
        },
        None => binary.pointer_at(stored_at),
    }
}
