//! Loading an ELF file and finding the items in it.
//!
//! An *item* is a function or a data object: a named range of addresses that
//! the rest of elfq reasons about. [`Binary::load`] finds items in three
//! places, each one only adding what the earlier ones missed:
//!
//! 1. the symbol tables, `.symtab` and `.dynsym` (see `symbols.rs`);
//! 2. the unwind table, `.eh_frame`, which still describes most functions in
//!    stripped x86-64 files (see `eh_frame.rs`);
//! 3. the entry point.

mod eh_frame;
mod symbols;

use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use object::{Architecture, Object, ObjectKind, ObjectSection, SectionFlags, SectionKind};

/// An ELF file that has been read, with the items found in it.
#[derive(Debug)]
pub struct Binary {
    /// The instruction set, such as x86-64 or 32-bit Arm.
    pub architecture: Architecture,
    /// An executable, or a shared library or position-independent executable.
    pub kind: ObjectKind,
    /// The address where execution starts.
    pub entry: u64,
    /// The file's sections, indexed by section number as `readelf -S` shows
    /// them. Entry 0 is the ELF null section.
    pub sections: Vec<Section>,
    /// Functions and data objects, sorted by address.
    pub items: Vec<Item>,
    /// Problems that did not stop loading but may make answers incomplete.
    pub warnings: Vec<String>,
}

/// A section of the file, such as `.text` or `.rodata`.
#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub address: u64,
    pub size: u64,
    pub kind: SectionKind,
    /// Whether the section is loaded into memory when the program runs.
    /// Only allocated sections hold items.
    pub allocated: bool,
}

/// A function or data object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The item's symbol name, or `fn_0x...` when it has no symbol.
    pub name: String,
    /// Other symbols at the same address, such as weak aliases.
    pub aliases: Vec<String>,
    pub kind: ItemKind,
    pub address: u64,
    /// Size in bytes, or 0 when it is unknown.
    pub size: u64,
    /// Whether `size` is a guess: the distance to the next item, used when
    /// the file does not record the size.
    pub size_estimated: bool,
    /// The section holding the item, as an index into [`Binary::sections`].
    pub section: usize,
    /// Where elfq found the item.
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemKind {
    Function,
    Data,
}

/// Where an item was found, from most to least reliable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// The symbol table, `.symtab`.
    SymbolTable,
    /// The dynamic symbol table, `.dynsym`, which survives stripping.
    DynamicSymbolTable,
    /// The unwind table, `.eh_frame`. The item has no name of its own.
    EhFrame,
    /// The ELF header's entry point. The item has no name of its own.
    EntryPoint,
}

impl Binary {
    /// Reads and parses the ELF file at `path`.
    pub fn load(path: &Path) -> Result<Binary> {
        let data =
            std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        Binary::parse(&data).with_context(|| format!("cannot load {}", path.display()))
    }

    /// Parses an ELF file that is already in memory.
    pub fn parse(data: &[u8]) -> Result<Binary> {
        if !data.starts_with(b"\x7fELF") {
            bail!("not an ELF file");
        }
        let file = object::File::parse(data).context("malformed ELF file")?;
        match file.kind() {
            ObjectKind::Executable | ObjectKind::Dynamic => {}
            ObjectKind::Relocatable => bail!(
                "this is an object file; elfq needs a linked executable, shared library \
                 or firmware image"
            ),
            ObjectKind::Core => bail!("core dumps are not supported"),
            _ => bail!("unknown ELF file type"),
        }

        let architecture = file.architecture();
        let sections = read_sections(&file);
        let mut warnings = Vec::new();

        let mut items = symbols::items(&file, &sections);
        match eh_frame::function_ranges(&file) {
            Ok(ranges) => add_unwind_items(&mut items, &ranges, &sections),
            Err(err) => warnings.push(format!("cannot read .eh_frame: {err}")),
        }
        let entry = code_address(architecture, file.entry());
        add_entry_item(&mut items, entry, &sections);

        items.sort_by(|a, b| (a.address, b.size, &a.name).cmp(&(b.address, a.size, &b.name)));
        estimate_missing_sizes(&mut items, &sections);

        Ok(Binary {
            architecture,
            kind: file.kind(),
            entry,
            sections,
            items,
            warnings,
        })
    }
}

impl Section {
    /// Whether `address` falls inside the section.
    pub fn contains(&self, address: u64) -> bool {
        address >= self.address && address - self.address < self.size
    }
}

impl fmt::Display for ItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ItemKind::Function => "function",
            ItemKind::Data => "data",
        })
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Source::SymbolTable => "symtab",
            Source::DynamicSymbolTable => "dynsym",
            Source::EhFrame => "eh_frame",
            Source::EntryPoint => "entry",
        })
    }
}

/// Clears the Thumb bit that 32-bit Arm sets in the addresses of Thumb
/// functions: the code itself starts at the even address.
fn code_address(architecture: Architecture, address: u64) -> u64 {
    if architecture == Architecture::Arm {
        address & !1
    } else {
        address
    }
}

fn read_sections(file: &object::File) -> Vec<Section> {
    let mut sections = Vec::new();
    for section in file.sections() {
        let index = section.index().0;
        if sections.len() <= index {
            sections.resize_with(index + 1, null_section);
        }
        let allocated = match section.flags() {
            SectionFlags::Elf { sh_flags, .. } => sh_flags.contains(object::elf::SHF_ALLOC),
            _ => false,
        };
        sections[index] = Section {
            name: String::from_utf8_lossy(section.name_bytes().unwrap_or_default()).into_owned(),
            address: section.address(),
            size: section.size(),
            kind: section.kind(),
            allocated,
        };
    }
    sections
}

fn null_section() -> Section {
    Section {
        name: String::new(),
        address: 0,
        size: 0,
        kind: SectionKind::Unknown,
        allocated: false,
    }
}

/// The executable section containing `address`, if any.
fn code_section(sections: &[Section], address: u64) -> Option<usize> {
    sections
        .iter()
        .position(|s| s.allocated && s.kind == SectionKind::Text && s.contains(address))
}

/// Adds a function for each unwind-table range that the symbols missed, and
/// fills in sizes that the symbol table left as 0.
fn add_unwind_items(items: &mut Vec<Item>, ranges: &[(u64, u64)], sections: &[Section]) {
    // Functions known from symbols, as (address, size, index into items).
    let mut known: Vec<(u64, u64, usize)> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.kind == ItemKind::Function)
        .map(|(index, item)| (item.address, item.size, index))
        .collect();
    known.sort_unstable();

    for &(start, length) in ranges {
        if start == 0 || length == 0 {
            continue;
        }
        let before = known.partition_point(|&(address, _, _)| address <= start);
        if let Some(&(address, size, index)) = before.checked_sub(1).map(|i| &known[i]) {
            if address == start {
                if size == 0 {
                    items[index].size = length;
                }
                continue;
            }
            if start < address + size {
                // Part of a function the symbols already describe.
                continue;
            }
        }
        let Some(section) = code_section(sections, start) else {
            continue;
        };
        items.push(Item {
            name: format!("fn_{start:#x}"),
            aliases: Vec::new(),
            kind: ItemKind::Function,
            address: start,
            size: length,
            size_estimated: false,
            section,
            source: Source::EhFrame,
        });
    }
}

/// Adds the entry point as a function when nothing else covers it, as in
/// stripped firmware.
fn add_entry_item(items: &mut Vec<Item>, entry: u64, sections: &[Section]) {
    let covered = items.iter().any(|item| {
        item.kind == ItemKind::Function
            && (item.address == entry || (item.address < entry && entry < item.address + item.size))
    });
    if covered {
        return;
    }
    if let Some(section) = code_section(sections, entry) {
        items.push(Item {
            name: "entry".to_string(),
            aliases: Vec::new(),
            kind: ItemKind::Function,
            address: entry,
            size: 0,
            size_estimated: false,
            section,
            source: Source::EntryPoint,
        });
    }
}

/// Gives each function of unknown size the distance to the next item in its
/// section. Functions with nothing after them keep size 0 (unknown), since
/// the end of the section would usually be a wild overestimate.
fn estimate_missing_sizes(items: &mut [Item], sections: &[Section]) {
    for i in 0..items.len() {
        let item = &items[i];
        if item.kind != ItemKind::Function || item.size != 0 {
            continue;
        }
        let section = &sections[item.section];
        let next = items[i + 1..]
            .iter()
            .map(|next| next.address)
            .find(|&address| address > item.address && section.contains(address));
        if let Some(next) = next {
            items[i].size = next - items[i].address;
            items[i].size_estimated = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_section() -> Section {
        Section {
            name: ".text".to_string(),
            address: 0x1000,
            size: 0x1000,
            kind: SectionKind::Text,
            allocated: true,
        }
    }

    fn function(name: &str, address: u64, size: u64) -> Item {
        Item {
            name: name.to_string(),
            aliases: Vec::new(),
            kind: ItemKind::Function,
            address,
            size,
            size_estimated: false,
            section: 0,
            source: Source::SymbolTable,
        }
    }

    #[test]
    fn unwind_table_fills_gaps_but_never_overrides_symbols() {
        let sections = [text_section()];
        let mut items = vec![
            function("sized", 0x1000, 0x20),
            function("unsized", 0x1100, 0),
        ];
        let ranges = [
            (0x1000, 0x30), // same start as a symbol: the symbol's size wins
            (0x1010, 0x08), // inside a symbol, such as a split-off cold part
            (0x1100, 0x40), // the symbol has no size: take the table's
            (0x1200, 0x10), // no symbol at all: a new, unnamed function
            (0x9000, 0x10), // outside any code section: ignored
        ];
        add_unwind_items(&mut items, &ranges, &sections);
        let found: Vec<(&str, u64, u64, Source)> = items
            .iter()
            .map(|i| (i.name.as_str(), i.address, i.size, i.source))
            .collect();
        assert_eq!(
            found,
            [
                ("sized", 0x1000, 0x20, Source::SymbolTable),
                ("unsized", 0x1100, 0x40, Source::SymbolTable),
                ("fn_0x1200", 0x1200, 0x10, Source::EhFrame),
            ]
        );
    }

    #[test]
    fn missing_sizes_are_estimated_from_the_next_item() {
        let sections = [text_section()];
        let mut items = vec![
            function("a", 0x1000, 0),
            function("b", 0x1030, 0x10),
            function("last", 0x1100, 0),
        ];
        estimate_missing_sizes(&mut items, &sections);
        assert_eq!((items[0].size, items[0].size_estimated), (0x30, true));
        // Nothing follows `last`, so its size stays unknown.
        assert_eq!((items[2].size, items[2].size_estimated), (0, false));
    }
}
