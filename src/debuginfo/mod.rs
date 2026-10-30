//! DWARF debug info, when the file has it.
//!
//! elfq reads one thing from it: where the compiler inlined one
//! function into another (see [`inlined_copies`]). Everything else in elfq
//! works without DWARF.

use std::borrow::Cow;
use std::collections::HashMap;

use anyhow::{Context, Result};
use gimli::{AttributeValue, DebuggingInformationEntry, EndianSlice, RunTimeEndian, Unit};
use object::{Object, ObjectSection};

use crate::binary::{self, Binary};

type Reader<'a> = EndianSlice<'a, RunTimeEndian>;

/// A copy of a function that the compiler inlined into another function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlinedCopy {
    /// The name of the function that was inlined, demangled as item names
    /// are.
    pub name: String,
    /// The function whose code holds the copy, as an index into
    /// [`Binary::items`].
    pub into: usize,
    /// Where the code of the copy starts running: the entry that DWARF
    /// gives, or the start of the first of its ranges.
    pub entry: u64,
    /// The bytes of code the copy takes, including the copies inlined into
    /// it.
    pub size: u64,
    /// The bytes of code the copy takes, not counting the copies inlined
    /// into it.
    pub own: u64,
    /// The copy that this one was inlined into, when it was inlined into an
    /// inlined copy, as an index into the same list.
    pub parent: Option<usize>,
}

/// Every inlined copy in the file, in the order DWARF lists them, which puts
/// each copy before the copies inlined into it. Empty when the file has no
/// DWARF.
pub fn inlined_copies(binary: &Binary) -> Result<Vec<InlinedCopy>> {
    let file = object::File::parse(binary.file_data())?;
    let endian = if file.is_little_endian() {
        RunTimeEndian::Little
    } else {
        RunTimeEndian::Big
    };
    let sections = gimli::DwarfSections::load(|id| -> Result<Cow<[u8]>> {
        match file.section_by_name(id.name()) {
            Some(section) => section
                .uncompressed_data()
                .with_context(|| format!("cannot read {}", id.name())),
            None => Ok(Cow::Borrowed(&[])),
        }
    })?;
    let dwarf = sections.borrow(|section| EndianSlice::new(section, endian));
    let mut units = Vec::new();
    let mut headers = dwarf.units();
    while let Some(header) = headers.next()? {
        units.push(dwarf.unit(header)?);
    }

    let mut walk = Walk {
        binary,
        dwarf: &dwarf,
        units: &units,
        copies: Vec::new(),
        names: HashMap::new(),
    };
    for unit in &units {
        walk.unit(unit)?;
    }
    let mut copies = walk.copies;
    for index in 0..copies.len() {
        if let Some(parent) = copies[index].parent {
            copies[parent].own = copies[parent].own.saturating_sub(copies[index].size);
        }
    }
    Ok(copies)
}

/// A walk through every entry of every unit, collecting inlined copies.
struct Walk<'a, 'd> {
    binary: &'a Binary,
    dwarf: &'a gimli::Dwarf<Reader<'d>>,
    units: &'a [Unit<Reader<'d>>],
    copies: Vec<InlinedCopy>,
    /// The names found for the entries that copies take their names from,
    /// by where those entries are in `.debug_info`: thousands of copies of
    /// one function share an entry.
    names: HashMap<usize, Option<String>>,
}

impl<'a, 'd> Walk<'a, 'd> {
    /// Visits the unit's root entry and every entry below it, in the order
    /// DWARF lists them. A loop rather than recursion, since entries can
    /// nest deeper than the stack would hold.
    fn unit(&mut self, unit: &'a Unit<Reader<'d>>) -> Result<()> {
        // The copies that enclose the current entry, innermost last, each
        // with its depth.
        let mut enclosing: Vec<(isize, usize)> = Vec::new();
        let mut entries = unit.entries();
        // The root's depth, once it is read. The walk stops where the
        // root's children end, before reading what a damaged unit holds
        // after them.
        let mut root = None;
        while root.is_none_or(|root| entries.next_depth() > root) {
            if !entries.next_entry()? {
                break;
            }
            // A null entry ends a list of children. As in gimli, the first
            // entry that isn't null is the root.
            let Some(entry) = entries.current() else {
                continue;
            };
            let depth = entry.depth();
            root.get_or_insert(depth);
            while enclosing.last().is_some_and(|&(at, _)| at >= depth) {
                enclosing.pop();
            }
            let parent = enclosing.last().map(|&(_, copy)| copy);
            if entry.tag() == gimli::DW_TAG_inlined_subroutine
                && let Some(copy) = self.copy(unit, entry, parent)?
            {
                self.copies.push(copy);
                enclosing.push((depth, self.copies.len() - 1));
            }
        }
        Ok(())
    }

    /// The copy that an inlined-subroutine entry describes, or `None` if
    /// its code isn't in the file: the linker drops unused functions but
    /// leaves their DWARF.
    fn copy(
        &mut self,
        unit: &'a Unit<Reader<'d>>,
        entry: &DebuggingInformationEntry<Reader<'d>>,
        parent: Option<usize>,
    ) -> Result<Option<InlinedCopy>> {
        let mut ranges = self.dwarf.die_ranges(unit, entry)?;
        let mut start = None;
        let mut size: u64 = 0;
        while let Some(range) = ranges.next()? {
            if range.end > range.begin {
                start.get_or_insert(range.begin);
                // Ranges that overlap, which only damaged debug info has,
                // can add up to more than a u64 holds.
                size = size.saturating_add(range.end - range.begin);
            }
        }
        // A copy that the optimizer merged into the code around it has no
        // bytes, only an address.
        if start.is_none() {
            let address = entry
                .attr_value(gimli::DW_AT_low_pc)
                .or_else(|| entry.attr_value(gimli::DW_AT_entry_pc));
            if let Some(address) = address {
                start = self.dwarf.attr_address(unit, address)?;
            }
        }
        let Some(start) = start else {
            return Ok(None);
        };
        let Some(into) = self.binary.item_at(start) else {
            return Ok(None);
        };
        // DWARF 5 may give the entry as an offset from the start, which
        // `attr_address` doesn't read: the start is taken then.
        let entry_pc = match entry.attr_value(gimli::DW_AT_entry_pc) {
            Some(address) => self.dwarf.attr_address(unit, address)?,
            None => None,
        };
        let name = match self.origin(unit, entry) {
            Some(origin) => match self.names.get(&origin) {
                Some(name) => name.clone(),
                None => {
                    let name = self.name(unit, entry)?;
                    self.names.insert(origin, name.clone());
                    name
                }
            },
            None => self.name(unit, entry)?,
        };
        let name = name.unwrap_or_else(|| "(unnamed)".to_string());
        Ok(Some(InlinedCopy {
            name,
            into,
            entry: entry_pc.unwrap_or(start),
            size,
            own: size,
            parent,
        }))
    }

    /// The name of the function an entry describes, following the links
    /// from an inlined copy to the function's own entry.
    fn name(
        &self,
        unit: &'a Unit<Reader<'d>>,
        entry: &DebuggingInformationEntry<Reader<'d>>,
    ) -> Result<Option<String>> {
        let mut unit = unit;
        let mut entry = entry.clone();
        // The links rarely go more than two deep; the limit stops cycles in
        // malformed files.
        for _ in 0..8 {
            let linkage_name = entry
                .attr_value(gimli::DW_AT_linkage_name)
                .or_else(|| entry.attr_value(gimli::DW_AT_MIPS_linkage_name));
            let linkage_name = match linkage_name {
                Some(value) => Some(self.string(unit, value)?),
                None => None,
            };
            // Rust and C++ linkage names demangle to their whole path;
            // DW_AT_name has only the last part.
            if let Some(demangled) = linkage_name.as_deref().and_then(binary::demangle) {
                return Ok(Some(demangled));
            }
            if let Some(value) = entry.attr_value(gimli::DW_AT_name) {
                return Ok(Some(self.string(unit, value)?));
            }
            if linkage_name.is_some() {
                return Ok(linkage_name);
            }
            let next = entry
                .attr_value(gimli::DW_AT_abstract_origin)
                .or_else(|| entry.attr_value(gimli::DW_AT_specification));
            let Some(next) = next else {
                return Ok(None);
            };
            let Some((next_unit, next_entry)) = self.entry_at(unit, next)? else {
                return Ok(None);
            };
            unit = next_unit;
            entry = next_entry;
        }
        Ok(None)
    }

    /// Where in `.debug_info` the entry is that a copy takes its name from,
    /// when the copy has no name of its own: [`Walk::name`] then gives the
    /// same name for every copy that links to that entry.
    fn origin(
        &self,
        unit: &Unit<Reader<'d>>,
        entry: &DebuggingInformationEntry<Reader<'d>>,
    ) -> Option<usize> {
        let names = [
            gimli::DW_AT_linkage_name,
            gimli::DW_AT_MIPS_linkage_name,
            gimli::DW_AT_name,
        ];
        if names.iter().any(|&name| entry.attr_value(name).is_some()) {
            return None;
        }
        match entry.attr_value(gimli::DW_AT_abstract_origin)? {
            AttributeValue::UnitRef(offset) => {
                // gimli adds the unit's offset without a check, so an
                // offset outside the unit, in damaged debug info, could
                // come out as another entry's.
                if !offset.is_in_bounds(&unit.header) {
                    return None;
                }
                offset
                    .to_debug_info_offset(&unit.header)
                    .map(|offset| offset.0)
            }
            AttributeValue::DebugInfoRef(offset) => Some(offset.0),
            _ => None,
        }
    }

    /// The entry that a reference attribute points to, and its unit.
    fn entry_at(
        &self,
        unit: &'a Unit<Reader<'d>>,
        value: AttributeValue<Reader<'d>>,
    ) -> Result<Option<(&'a Unit<Reader<'d>>, DebuggingInformationEntry<Reader<'d>>)>> {
        match value {
            AttributeValue::UnitRef(offset) => Ok(Some((unit, unit.entry(offset)?))),
            AttributeValue::DebugInfoRef(offset) => {
                for other in self.units {
                    if let Some(offset) = offset.to_unit_offset(&other.header) {
                        return Ok(Some((other, other.entry(offset)?)));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn string(&self, unit: &Unit<Reader<'d>>, value: AttributeValue<Reader<'d>>) -> Result<String> {
        let text = self.dwarf.attr_string(unit, value)?;
        Ok(text.to_string_lossy().into_owned())
    }
}
