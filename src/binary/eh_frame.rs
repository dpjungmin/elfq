//! Function ranges from the unwind table, `.eh_frame`.
//!
//! On x86-64, compilers describe almost every function in `.eh_frame` so that
//! exceptions and debuggers can walk the stack. Programs need the table at run
//! time, so stripping keeps it, which makes it the best source of function
//! boundaries when a file has no symbols. Firmware usually has no
//! `.eh_frame` at all.

use anyhow::Result;
use gimli::{BaseAddresses, CieOrFde, EhFrame, RunTimeEndian, UnwindSection};
use object::{Object, ObjectSection};

/// Returns `(start, length)` for every function the unwind table describes,
/// or nothing when the file has no `.eh_frame`.
pub(super) fn function_ranges(file: &object::File) -> Result<Vec<(u64, u64)>> {
    let Some(section) = file.section_by_name(".eh_frame") else {
        return Ok(Vec::new());
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

    let mut ranges = Vec::new();
    let mut entries = eh_frame.entries(&bases);
    while let Some(entry) = entries.next()? {
        if let CieOrFde::Fde(partial) = entry {
            let fde = partial.parse(EhFrame::cie_from_offset)?;
            ranges.push((fde.initial_address(), fde.len()));
        }
    }
    Ok(ranges)
}
