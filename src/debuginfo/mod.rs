//! DWARF debug info, when the file has it.
//!
//! elfq reads one thing from it: where the compiler inlined one
//! function into another (see [`inlined_copies`]). Everything else in elfq
//! works without DWARF.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use gimli::{
    AttributeValue, DebugInfoOffset, DebuggingInformationEntry, EndianSlice, RunTimeEndian, Unit,
    UnitOffset,
};
use object::{Object, ObjectSection};

use crate::binary::{self, Binary};

type Reader<'a> = EndianSlice<'a, RunTimeEndian>;

/// A copy of a function that the compiler inlined into another function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlinedCopy {
    /// The name of the function that was inlined, demangled as item names
    /// are. Its copies share it, since a function can have thousands.
    /// Empty when [`inlined_copies`] wasn't asked to name the copy (see
    /// [`CopiesToName`]).
    pub name: Arc<str>,
    /// The function whose code holds the copy, as an index into
    /// [`Binary::items`].
    pub into: usize,
    /// Where the code of the copy starts running: the entry that DWARF
    /// gives, or the start of the first of its ranges.
    pub(crate) entry: u64,
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

/// Which inlined copies [`inlined_copies`] names. Working out the names
/// takes about half of its time, demangling them above all, and `inlined
/// SYMBOL` reads only the names of the copies in SYMBOL's function.
///
/// The names of the other copies are still read, though not worked out, so
/// that in damaged debug info, a name that can't be read fails
/// [`inlined_copies`] whichever copies it names.
#[derive(Debug, Clone, Copy)]
pub enum CopiesToName {
    /// Every copy.
    All,
    /// Only the copies in the function at this index into
    /// [`Binary::items`]. The others' names are empty.
    OnlyInto(usize),
}

impl CopiesToName {
    /// Whether they include the copies in the function at `into`, an index
    /// into [`Binary::items`].
    fn include_copies_in(self, into: usize) -> bool {
        match self {
            CopiesToName::All => true,
            CopiesToName::OnlyInto(function) => function == into,
        }
    }
}

/// Every inlined copy in the file, in the order DWARF lists them, which puts
/// each copy before the copies inlined into it, named as `copies_to_name`
/// says. Empty when the file has no DWARF.
pub fn inlined_copies(binary: &Binary, copies_to_name: CopiesToName) -> Result<Vec<InlinedCopy>> {
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
    // Each unit's header is read where the one before ends, so the units
    // are in the order `.debug_info` holds them, as `unit_that_can_hold`
    // needs.
    let mut units = Vec::new();
    let mut headers = dwarf.units();
    while let Some(header) = headers.next()? {
        units.push(dwarf.unit(header)?);
    }

    let mut walk = Walk {
        binary,
        dwarf: &dwarf,
        units: &units,
        copies_to_name,
        copies: Vec::new(),
        names: HashMap::new(),
        checked: HashSet::new(),
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
    /// Every unit, in the order `.debug_info` holds them.
    units: &'a [Unit<Reader<'d>>],
    copies_to_name: CopiesToName,
    copies: Vec<InlinedCopy>,
    /// The names found for the entries that copies take their names from,
    /// by where those entries are in `.debug_info`: thousands of copies of
    /// one function share an entry, and its name.
    names: HashMap<usize, Option<Arc<str>>>,
    /// The entries, by where they are in `.debug_info` as `names` keys
    /// them, whose names [`Walk::check_name`] has checked for copies that
    /// aren't named, so that each is checked once. A check that fails ends
    /// the walk.
    checked: HashSet<usize>,
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
        let name = match self.copies_to_name.include_copies_in(into) {
            true => self.copy_name(unit, entry)?,
            false => {
                self.check_name(unit, entry)?;
                // The empty name. `Arc`'s default needn't allocate one for
                // each copy, as `Arc::from("")` does.
                Arc::default()
            }
        };
        Ok(Some(InlinedCopy {
            name,
            into,
            entry: entry_pc.unwrap_or(start),
            size,
            own: size,
            parent,
        }))
    }

    /// The name of the function that an inlined-subroutine entry is a copy
    /// of, worked out once for all the copies that take it from one entry.
    fn copy_name(
        &mut self,
        unit: &'a Unit<Reader<'d>>,
        entry: &DebuggingInformationEntry<Reader<'d>>,
    ) -> Result<Arc<str>> {
        let name = match self.origin(unit, entry) {
            Some(origin) => match self.names.get(&origin) {
                Some(name) => name.clone(),
                None => {
                    let name = self.name(unit, entry)?.map(Arc::from);
                    self.names.insert(origin, name.clone());
                    name
                }
            },
            None => self.name(unit, entry)?.map(Arc::from),
        };
        Ok(name.unwrap_or_else(|| Arc::from("(unnamed)")))
    }

    /// Fails where [`Walk::copy_name`] would, for a copy that isn't named,
    /// but works out the name only when one of its strings can't be read.
    /// Like the name, the check is made once for all the copies that take
    /// their name from one entry.
    fn check_name(
        &mut self,
        unit: &'a Unit<Reader<'d>>,
        entry: &DebuggingInformationEntry<Reader<'d>>,
    ) -> Result<()> {
        if let Some(origin) = self.origin(unit, entry)
            && !self.checked.insert(origin)
        {
            return Ok(());
        }
        let Some(named) = self.named_entry(unit, entry)? else {
            return Ok(());
        };
        let readable = name_attributes(&named.entry)
            .into_iter()
            .flatten()
            .all(|value| self.dwarf.attr_string(named.unit, value).is_ok());
        if !readable {
            // `name` reads DW_AT_name only when the linkage name doesn't
            // demangle, so only `name` can say whether a string that can't
            // be read fails it.
            self.name(unit, entry)?;
        }
        Ok(())
    }

    /// The name of the function an entry describes, following the links
    /// from an inlined copy to the function's own entry. It reads no
    /// strings but those of [`name_attributes`], which
    /// [`Walk::check_name`] checks.
    fn name(
        &self,
        unit: &'a Unit<Reader<'d>>,
        entry: &DebuggingInformationEntry<Reader<'d>>,
    ) -> Result<Option<String>> {
        let Some(EntryInUnit { unit, entry }) = self.named_entry(unit, entry)? else {
            return Ok(None);
        };
        let [linkage_name, name] = name_attributes(&entry);
        let linkage_name = match linkage_name {
            Some(value) => Some(self.string(unit, value)?),
            None => None,
        };
        // Rust and C++ linkage names demangle to their whole path;
        // DW_AT_name has only the last part.
        if let Some(demangled) = linkage_name.as_deref().and_then(binary::names::demangle) {
            return Ok(Some(demangled));
        }
        if let Some(value) = name {
            return Ok(Some(self.string(unit, value)?));
        }
        Ok(linkage_name)
    }

    /// The entry that has the name of the function an entry describes: the
    /// entry itself, or the one its links to the function's own entry lead
    /// to. `None` when the links end before a name.
    fn named_entry(
        &self,
        unit: &'a Unit<Reader<'d>>,
        entry: &DebuggingInformationEntry<Reader<'d>>,
    ) -> Result<Option<EntryInUnit<'a, 'd>>> {
        let mut at = EntryInUnit {
            unit,
            entry: entry.clone(),
        };
        // The links rarely go more than two deep; the limit stops cycles in
        // malformed files.
        for _ in 0..8 {
            if has_name(&at.entry) {
                return Ok(Some(at));
            }
            let link = at
                .entry
                .attr_value(gimli::DW_AT_abstract_origin)
                .or_else(|| at.entry.attr_value(gimli::DW_AT_specification));
            let Some(link) = link else {
                return Ok(None);
            };
            let Some(next) = self.entry_at(at.unit, link)? else {
                return Ok(None);
            };
            at = next;
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
        if has_name(entry) {
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

    /// The entry that a reference attribute in `unit` points to.
    fn entry_at(
        &self,
        unit: &'a Unit<Reader<'d>>,
        value: AttributeValue<Reader<'d>>,
    ) -> Result<Option<EntryInUnit<'a, 'd>>> {
        match value {
            AttributeValue::UnitRef(offset) => Ok(Some(EntryInUnit::read(unit, offset)?)),
            AttributeValue::DebugInfoRef(offset) => {
                let Some(other) = unit_that_can_hold(self.units, offset) else {
                    return Ok(None);
                };
                match offset.to_unit_offset(&other.header) {
                    Some(offset) => Ok(Some(EntryInUnit::read(other, offset)?)),
                    // In the unit's header, or past its end.
                    None => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }

    fn string(&self, unit: &Unit<Reader<'d>>, value: AttributeValue<Reader<'d>>) -> Result<String> {
        let text = self.dwarf.attr_string(unit, value)?;
        Ok(text.to_string_lossy().into_owned())
    }
}

/// An entry of `.debug_info`, with the unit that holds it, which reading
/// the entry's attributes needs.
struct EntryInUnit<'a, 'd> {
    unit: &'a Unit<Reader<'d>>,
    entry: DebuggingInformationEntry<Reader<'d>>,
}

impl<'a, 'd> EntryInUnit<'a, 'd> {
    /// The entry at `offset` in `unit`.
    fn read(unit: &'a Unit<Reader<'d>>, offset: UnitOffset) -> Result<Self> {
        let entry = unit.entry(offset)?;
        Ok(EntryInUnit { unit, entry })
    }
}

/// Whether an entry gives a function's name itself, rather than through a
/// link to another entry.
fn has_name(entry: &DebuggingInformationEntry<Reader<'_>>) -> bool {
    name_attributes(entry).iter().any(Option::is_some)
}

/// The attributes of an entry that give its function's name: its linkage
/// name, the mangled name, and its DW_AT_name. [`Walk::name`] reads the
/// name from their strings alone, and [`Walk::check_name`] checks that
/// those can be read.
fn name_attributes<'d>(
    entry: &DebuggingInformationEntry<Reader<'d>>,
) -> [Option<AttributeValue<Reader<'d>>>; 2] {
    let linkage_name = entry
        .attr_value(gimli::DW_AT_linkage_name)
        .or_else(|| entry.attr_value(gimli::DW_AT_MIPS_linkage_name));
    [linkage_name, entry.attr_value(gimli::DW_AT_name)]
}

/// The only unit among `units` whose entries can hold `offset`, a place in
/// `.debug_info`: the last one that starts at or before it. `None` when
/// every unit starts after it.
///
/// `units` are in the order `.debug_info` holds them, and each starts where
/// the one before ends, so their entries don't overlap. A binary search
/// finds that unit: asking each unit in turn took about a tenth of the
/// time of `inlined` and `diff` on large builds, which have hundreds or
/// thousands of units.
fn unit_that_can_hold<'u, 'd>(
    units: &'u [Unit<Reader<'d>>],
    offset: DebugInfoOffset,
) -> Option<&'u Unit<Reader<'d>>> {
    let first_after = units.partition_point(|unit| unit.header.offset().0 <= offset.0);
    Some(&units[first_after.checked_sub(1)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit of DWARF 4 whose root entry, of abbreviation 1, is followed
    /// by `padding` null entries. Its header takes 11 bytes: its length in
    /// 4, the version in 2, the abbreviations' offset, 0, in 4, and the
    /// size of an address, 8, in 1.
    fn unit(padding: usize) -> Vec<u8> {
        let mut rest = vec![4, 0, 0, 0, 0, 0, 8, 1];
        rest.resize(rest.len() + padding, 0);
        [(rest.len() as u32).to_le_bytes().to_vec(), rest].concat()
    }

    /// Where `offset` is, as (where its unit starts, where it is in the
    /// unit), as `Walk::entry_at` finds it, asking only the unit that
    /// `unit_that_can_hold` finds, after checking that asking each unit in
    /// turn finds the same.
    fn holding(units: &[Unit<Reader<'_>>], offset: usize) -> Option<(usize, usize)> {
        let offset = DebugInfoOffset(offset);
        let place = |unit: &Unit<Reader<'_>>| {
            let at = offset.to_unit_offset(&unit.header)?;
            Some((unit.header.offset().0, at.0))
        };
        let found = unit_that_can_hold(units, offset).and_then(place);
        assert_eq!(found, units.iter().find_map(place), "at {}", offset.0);
        found
    }

    #[test]
    fn the_unit_holding_an_offset_is_the_last_that_starts_before_it() {
        // Units of 12, 14 and 13 bytes, at 0, 12 and 26, ending at 39.
        // Abbreviation 1 is a compile unit without children or attributes.
        let info = [unit(0), unit(2), unit(1)].concat();
        let abbreviations = [1, 0x11, 0, 0, 0, 0];
        let dwarf = gimli::Dwarf {
            debug_info: EndianSlice::new(&info, RunTimeEndian::Little).into(),
            debug_abbrev: EndianSlice::new(&abbreviations, RunTimeEndian::Little).into(),
            ..Default::default()
        };
        let mut units = Vec::new();
        let mut headers = dwarf.units();
        while let Some(header) = headers.next().unwrap() {
            units.push(dwarf.unit(header).unwrap());
        }

        // The first and last bytes of the second unit's entries, after its
        // header.
        assert_eq!(holding(&units, 23), Some((12, 11)));
        assert_eq!(holding(&units, 25), Some((12, 13)));
        // Its header, from its start; its end, where the third unit starts;
        // the end of the last unit; and past it.
        for offset in [12, 22, 26, 39, 40, 1000] {
            assert_eq!(holding(&units, offset), None, "at {offset}");
        }
        // Before the first unit, when the units start after 0.
        assert_eq!(holding(&units[1..], 0), None);
        assert_eq!(holding(&units[1..], 11), None);
        assert_eq!(holding(&[], 0), None);
        // Every offset from 0 to past the end, with and without the first
        // unit.
        for offset in 0..45 {
            holding(&units, offset);
            holding(&units[1..], offset);
        }
    }
}
