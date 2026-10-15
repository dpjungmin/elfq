//! Loading an ELF file and finding the items in it.
//!
//! An *item* is a function or a data object: a named range of addresses that
//! the rest of elfq reasons about. [`Binary::load`] finds items in these
//! places, each one only adding what the earlier ones missed:
//!
//! 1. the symbol tables, `.symtab` and `.dynsym` (see `symbols.rs`);
//! 2. the stubs that call shared libraries, such as `write@plt` (see
//!    `plt.rs`), and arrays of startup and shutdown functions such as
//!    `.init_array`, which have no symbols;
//! 3. the unwind table, `.eh_frame`, which still describes most functions in
//!    stripped x86-64 files (see `eh_frame.rs`);
//! 4. the entry point, and in firmware the vector table, when no symbol
//!    covers it;
//! 5. in firmware that nothing lists functions for, the code itself,
//!    followed from the entry point and the vector table (see
//!    `unnamed_functions.rs`);
//! 6. references to data that has no symbol, such as vtables and string
//!    literals (see `unnamed_data.rs`).
//!
//! Loading also reads the dynamic relocations (see `relocations.rs`), which
//! say where the program stores pointers and which functions and variables
//! it imports from shared libraries, and in Cortex-M firmware the vector
//! table (see `vector_table.rs`).

mod eh_frame;
mod names;
mod plt;
mod range;
mod relocations;
mod symbols;
mod unnamed_data;
mod unnamed_functions;
mod vector_table;

pub use names::{demangle, short_names};
pub use range::{AddressRange, MemoryBlock, memory_blocks};
pub use vector_table::VectorTable;

use std::collections::HashSet;
use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use object::{
    Architecture, Object, ObjectKind, ObjectSection, ObjectSymbol, SectionFlags, SectionKind,
    SymbolKind,
};

/// An ELF file that has been read, with the items found in it.
#[derive(Debug)]
pub struct Binary {
    /// The instruction set, such as x86-64 or 32-bit Arm.
    pub architecture: Architecture,
    /// The size of a pointer in bytes: 4 or 8.
    pub pointer_size: u64,
    pub kind: FileKind,
    /// The address where execution starts.
    pub entry: u64,
    /// For 32-bit Arm, whether the code is Thumb, the instruction set that
    /// Cortex-M runs, rather than the older Arm (A32) instruction set. Thumb
    /// functions have bit 0 set in their symbols and in the entry point.
    pub thumb: bool,
    /// Whether the file has a symbol table (`.symtab`). Stripping removes it.
    pub has_symbols: bool,
    /// Whether the file has DWARF debug info (`.debug_info`).
    pub has_dwarf: bool,
    /// The file's sections, indexed by section number as `readelf -S` shows
    /// them. Entry 0 is the ELF null section.
    pub sections: Vec<Section>,
    /// Functions and data objects, sorted by address.
    pub items: Vec<Item>,
    /// Names of the functions and variables the program uses from shared
    /// libraries, such as `write`.
    pub imports: Vec<String>,
    /// Pointers that the dynamic loader writes into memory at startup,
    /// sorted by address.
    pub relocations: Vec<Relocation>,
    /// Addresses that the unwinder uses while it handles a panic or an
    /// exception, as the unwind table names them (see `eh_frame.rs`): the
    /// personality routines it calls, and where pointers to them are stored
    /// when the table points there instead.
    pub used_by_unwinder: Vec<u64>,
    /// Blocks of variables that the compiler placed side by side, such as
    /// LLVM's `.L_MergedGlobals`, as `(address, size)`. Code takes the
    /// block's address and reaches each variable at an offset from it, so
    /// taking that address can mean using any of them.
    pub merged_variables: Vec<(u64, u64)>,
    /// The Cortex-M vector table, in Thumb files that have one.
    pub vector_table: Option<VectorTable>,
    /// Problems that did not stop loading but may make answers incomplete.
    pub warnings: Vec<String>,
    /// The whole file.
    data: Vec<u8>,
    big_endian: bool,
}

/// What kind of program the file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// An executable that runs at a fixed address, such as firmware.
    Executable,
    /// An executable that can run at any address, as most Linux programs do.
    PositionIndependentExecutable,
    /// A shared library.
    SharedLibrary,
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
    /// Where the section's bytes are in the file, as (offset, size). `None`
    /// for sections such as `.bss` that take no space in the file.
    pub file_range: Option<(u64, u64)>,
}

/// A function or data object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The item's symbol name, demangled if it is a Rust symbol. Without a
    /// symbol, a name made from where it is, such as `fn_0x1690` or
    /// `.rodata@0x2004`.
    pub name: String,
    /// Other names for the item: the symbol as the file has it when `name`
    /// is demangled, and other symbols at the same address, such as weak
    /// aliases.
    pub aliases: Vec<String>,
    pub kind: ItemKind,
    pub address: u64,
    /// Size in bytes, or 0 when it is unknown.
    pub size: u64,
    /// Whether `size` is a guess, made when the file doesn't record the
    /// size: the distance to the next item, or for unnamed data, to the next
    /// address that something refers to.
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
    /// The procedure linkage table. The item is a stub named after the
    /// import it jumps to, or the table's header named after its section.
    Plt,
    /// A section with a known purpose and no symbol, such as `.init_array`
    /// or the vector table's. The item is named after the section.
    Section,
    /// Code that elfq followed from the entry point reaches the address, in
    /// firmware that nothing lists functions for. The item is a function
    /// named after its address, such as `fn_0x8000040`.
    Reached,
    /// Code or a stored pointer refers to the address, and nothing else
    /// names what is there, as with vtables and string literals. The item is
    /// data named after its section and address, such as `.rodata@0x2004`.
    Reference,
}

impl Source {
    /// Whether the source lists the file's functions, as the symbol tables
    /// and the unwind table do.
    pub fn lists_functions(self) -> bool {
        matches!(
            self,
            Source::SymbolTable | Source::DynamicSymbolTable | Source::EhFrame
        )
    }
}

/// A place where the dynamic loader writes an address at startup, such as a
/// function pointer in a table or an entry in the global offset table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Relocation {
    /// Where the pointer is stored.
    pub address: u64,
    /// What it points at.
    pub target: RelocationTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocationTarget {
    /// An address in this file.
    Address(u64),
    /// An import, as an index into [`Binary::imports`].
    Import(usize),
}

impl Binary {
    /// Reads and parses the ELF file at `path`.
    pub fn load(path: &Path) -> Result<Binary> {
        let data =
            std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        Binary::parse(&data).with_context(|| format!("cannot load {}", path.display()))
    }

    /// The whole file, for reading parts of it that elfq doesn't keep, such
    /// as DWARF debug info.
    pub fn file_data(&self) -> &[u8] {
        &self.data
    }

    /// Parses an ELF file that is already in memory.
    pub fn parse(data: &[u8]) -> Result<Binary> {
        if !data.starts_with(b"\x7fELF") {
            bail!("not an ELF file");
        }
        let file = object::File::parse(data).context("malformed ELF file")?;
        let kind = match file.kind() {
            ObjectKind::Executable => FileKind::Executable,
            // Both are position-independent; only executables name the
            // dynamic loader that starts them.
            ObjectKind::Dynamic if file.section_by_name(".interp").is_some() => {
                FileKind::PositionIndependentExecutable
            }
            ObjectKind::Dynamic => FileKind::SharedLibrary,
            ObjectKind::Relocatable => bail!(
                "this is an object file; elfq needs a linked executable, shared library \
                 or firmware image"
            ),
            ObjectKind::Core => bail!("core dumps are not supported"),
            _ => bail!("unknown ELF file type"),
        };

        let architecture = file.architecture();
        let mut binary = Binary {
            architecture,
            pointer_size: if file.is_64() { 8 } else { 4 },
            kind,
            entry: code_address(architecture, file.entry()),
            thumb: architecture == Architecture::Arm && is_thumb(&file),
            has_symbols: file.symbol_table().is_some(),
            has_dwarf: file.section_by_name(".debug_info").is_some(),
            sections: read_sections(&file),
            items: Vec::new(),
            imports: Vec::new(),
            relocations: Vec::new(),
            used_by_unwinder: Vec::new(),
            merged_variables: Vec::new(),
            vector_table: None,
            warnings: Vec::new(),
            data: data.to_vec(),
            big_endian: !file.is_little_endian(),
        };
        (binary.imports, binary.relocations) = relocations::read(&file, architecture);
        if binary.thumb {
            binary.vector_table = vector_table::find(&binary);
        }

        let (mut items, merged_variables) = symbols::items(&file, &binary.sections);
        binary.merged_variables = merged_variables;
        add_new_items(&mut items, plt::stubs(&binary));
        add_new_items(&mut items, section_items(&binary.sections));
        match eh_frame::read(&file) {
            Ok(table) => {
                add_unwind_items(&mut items, &table.functions, &binary.sections);
                for pointer in table.personalities {
                    if let gimli::Pointer::Indirect(stored_at) = pointer {
                        binary.used_by_unwinder.push(stored_at);
                    }
                    binary
                        .used_by_unwinder
                        .extend(binary.personality_address(pointer));
                }
            }
            Err(err) => binary
                .warnings
                .push(format!("cannot read .eh_frame: {err}")),
        }
        add_entry_item(&mut items, binary.entry, &binary.sections);
        items.extend(vector_table::item(&binary, &items));
        let listed = items
            .iter()
            .any(|item| item.kind == ItemKind::Function && item.source.lists_functions());
        if binary.thumb && binary.kind == FileKind::Executable && !listed {
            unnamed_functions::add(&binary, &mut items);
        }

        sort_items(&mut items);
        estimate_missing_sizes(&mut items, &binary.sections);
        binary.items = items;
        // Last, since finding unnamed data needs all the other items.
        let unnamed = unnamed_data::items(&binary);
        binary.items.extend(unnamed);
        sort_items(&mut binary.items);
        Ok(binary)
    }

    /// The bytes at `address`, or `None` if the file doesn't hold all `size`
    /// of them. `.bss`, for example, takes no space in the file.
    pub fn bytes(&self, address: u64, size: u64) -> Option<&[u8]> {
        let section = &self.sections[self.section_at(address)?];
        let (offset, file_size) = section.file_range?;
        let start = address - section.address;
        let end = start.checked_add(size)?;
        if end > file_size {
            return None;
        }
        self.data
            .get(usize::try_from(offset + start).ok()?..usize::try_from(offset + end).ok()?)
    }

    /// The pointer stored at `address`, as the file holds it before the
    /// program runs.
    pub fn pointer_at(&self, address: u64) -> Option<u64> {
        let bytes = self.bytes(address, self.pointer_size)?;
        let mut word = [0; 8];
        if self.big_endian {
            word[8 - bytes.len()..].copy_from_slice(bytes);
            Some(u64::from_be_bytes(word))
        } else {
            word[..bytes.len()].copy_from_slice(bytes);
            Some(u64::from_le_bytes(word))
        }
    }

    /// The section that holds `address` when the program runs, as an index
    /// into [`Binary::sections`].
    pub fn section_at(&self, address: u64) -> Option<usize> {
        self.sections.iter().position(|section| {
            // `.tbss` only describes each thread's copy of its variables,
            // so other sections share its addresses.
            section.allocated
                && section.kind != SectionKind::UninitializedTls
                && section.contains(address)
        })
    }

    /// The item whose bytes include `address`, as an index into
    /// [`Binary::items`].
    pub fn item_at(&self, address: u64) -> Option<usize> {
        // Items don't overlap, except that several can start at one address,
        // such as a function of unknown size and a data object.
        let end = self.items.partition_point(|item| item.address <= address);
        let start = self.items.get(end.checked_sub(1)?)?.address;
        (0..end)
            .rev()
            .take_while(|&index| self.items[index].address == start)
            .find(|&index| self.items[index].contains(address))
    }

    /// Where a personality routine is, given the unwind table's pointer to
    /// it. `None` when it is in a shared library.
    fn personality_address(&self, pointer: gimli::Pointer) -> Option<u64> {
        let stored_at = match pointer {
            gimli::Pointer::Direct(address) => return Some(address),
            gimli::Pointer::Indirect(stored_at) => stored_at,
        };
        match self.relocation_at(stored_at) {
            Some(relocation) => match relocation.target {
                RelocationTarget::Address(address) => Some(address),
                RelocationTarget::Import(_) => None,
            },
            None => self.pointer_at(stored_at),
        }
    }

    /// The relocation that stores a pointer at `address`, if any.
    pub fn relocation_at(&self, address: u64) -> Option<&Relocation> {
        let index = self
            .relocations
            .binary_search_by_key(&address, |relocation| relocation.address)
            .ok()?;
        Some(&self.relocations[index])
    }
}

impl Item {
    /// Whether `address` is the item's first byte or falls inside it.
    pub fn contains(&self, address: u64) -> bool {
        address == self.address || (address > self.address && address - self.address < self.size)
    }

    /// The item's name, then its aliases.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

impl Section {
    /// Whether `address` falls inside the section.
    pub fn contains(&self, address: u64) -> bool {
        address >= self.address && address - self.address < self.size
    }
}

impl fmt::Display for FileKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FileKind::Executable => "executable",
            FileKind::PositionIndependentExecutable => "position-independent executable",
            FileKind::SharedLibrary => "shared library",
        })
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
            Source::Plt => "plt",
            Source::Section => "section",
            Source::Reached => "reached",
            Source::Reference => "reference",
        })
    }
}

/// Sorts items by address. Of several items at one address, the largest
/// comes first.
fn sort_items(items: &mut [Item]) {
    items.sort_by(|a, b| (a.address, b.size, &a.name).cmp(&(b.address, a.size, &b.name)));
}

/// Whether a 32-bit Arm file's code is Thumb: its entry point or any
/// function symbol has bit 0 set.
fn is_thumb(file: &object::File) -> bool {
    file.entry() & 1 == 1
        || file
            .symbols()
            .any(|symbol| symbol.kind() == SymbolKind::Text && symbol.address() & 1 == 1)
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
            file_range: section.file_range(),
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
        file_range: None,
    }
}

/// The executable section containing `address`, if any.
fn code_section(sections: &[Section], address: u64) -> Option<usize> {
    sections
        .iter()
        .position(|s| s.allocated && s.kind == SectionKind::Text && s.contains(address))
}

/// Adds each of `new` unless an item of its kind already starts at its
/// address: symbols always win.
fn add_new_items(items: &mut Vec<Item>, new: Vec<Item>) {
    let known: HashSet<(ItemKind, u64)> =
        items.iter().map(|item| (item.kind, item.address)).collect();
    items.extend(
        new.into_iter()
            .filter(|item| !known.contains(&(item.kind, item.address))),
    );
}

/// Items for the arrays of functions that run at startup and exit, which
/// have no symbols of their own.
fn section_items(sections: &[Section]) -> Vec<Item> {
    const ARRAYS: [&str; 5] = [
        ".preinit_array",
        ".init_array",
        ".fini_array",
        ".ctors",
        ".dtors",
    ];
    sections
        .iter()
        .enumerate()
        .filter(|(_, section)| {
            section.allocated && section.size > 0 && ARRAYS.contains(&section.name.as_str())
        })
        .map(|(index, section)| Item {
            name: section.name.clone(),
            aliases: Vec::new(),
            kind: ItemKind::Data,
            address: section.address,
            size: section.size,
            size_estimated: false,
            section: index,
            source: Source::Section,
        })
        .collect()
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
            file_range: None,
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
