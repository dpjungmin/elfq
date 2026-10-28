//! Loading an ELF file and finding the items in it.
//!
//! An *item* is a function or a data object: a named range of addresses that
//! the rest of elfq reasons about. [`Binary::load`] finds items in these
//! places, each one only adding what the earlier ones missed:
//!
//! 1. the symbol tables, `.symtab` and `.dynsym` (see `symbols.rs`);
//! 2. in x86-64 files, the PLT stubs that call imported functions, such as
//!    `write@plt`, or the file's own (see `plt.rs`);
//! 3. the arrays of startup and shutdown functions, such as `.init_array`,
//!    which have no symbols;
//! 4. the unwind table, `.eh_frame`, which still describes most functions in
//!    stripped x86-64 files (see `eh_frame.rs`);
//! 5. the entry point, when no function covers it, and in firmware the
//!    vector table, when no item covers any of it;
//! 6. in firmware that nothing lists functions for, the code itself,
//!    followed from the entry point, the vector table's handlers, the
//!    functions in arrays such as `.init_array`, and the Thumb addresses
//!    that code loads or data holds (see `unnamed_functions.rs`);
//! 7. references to data that has no symbol, such as vtables and string
//!    literals (see `unnamed_data.rs`).
//!
//! Loading also reads the dynamic relocations (see `relocations.rs`), which
//! say where the program stores pointers and which functions and variables
//! it imports from shared libraries, and in Cortex-M firmware the vector
//! table (see `vector_table.rs`).
//!
//! [`LoadedMemory`] reads only a file's section headers, and the bytes that
//! its sections put in memory, which `diff` compares before it needs any
//! item (see `memory.rs`).

mod attributes;
mod eh_frame;
mod memory;
mod names;
mod plt;
mod range;
mod relocations;
mod symbols;
mod unnamed_data;
mod unnamed_functions;
mod vector_table;

pub use attributes::ArmArchitecture;
pub use memory::{Bytes, LoadedMemory, same_bytes_in, same_memory};
pub use names::{
    can_be_shorter, demangle, ends_with_short_name, for_each_short_name, has_short_name,
    may_join_words, short_name_key, short_name_keys, short_names, short_names_with_key,
    without_copy_suffixes, words,
};
pub use range::{AddressRange, MemoryBlock, hex_number, memory_blocks, range_hint};
pub use vector_table::{VectorTable, exception_name, is_reserved};

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use object::read::elf::{ElfFile, FileHeader, ProgramHeader, SectionHeader};
use object::{
    Architecture, Endianness, Object, ObjectKind, ObjectSection, ObjectSymbol, SectionFlags,
    SectionIndex, SectionKind, SymbolKind,
};

use crate::arch::{self, CodeReference, CodeReferenceKind};

/// Arrays of pointers to the functions that the C runtime calls at startup
/// and exit.
pub(crate) const STARTUP_ARRAYS: [&str; 5] = [
    ".preinit_array",
    ".init_array",
    ".fini_array",
    ".ctors",
    ".dtors",
];

/// An ELF file that has been read, with the items found in it.
#[derive(Debug)]
pub struct Binary {
    /// The instruction set, such as x86-64 or 32-bit Arm.
    pub architecture: Architecture,
    /// The size of a pointer in bytes: 4 or 8.
    pub pointer_size: u64,
    /// What kind of program the file holds. Only in an `Executable`, linked
    /// to run at one address, does elfq read pointers from the words in
    /// data; the others list them in their dynamic relocations.
    pub kind: FileKind,
    /// The address where execution starts.
    pub entry: u64,
    /// For 32-bit Arm, whether the code is Thumb, the instruction set that
    /// Cortex-M runs, rather than the older Arm (A32) instruction set. Thumb
    /// functions have bit 0 set in their symbols and in the entry point.
    pub thumb: bool,
    /// The order of the bytes in the numbers the file holds. In big-endian
    /// Arm firmware (BE8), only data is big-endian: instructions are still
    /// little-endian.
    pub endianness: Endianness,
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
    /// For 32-bit Arm, the version of the architecture that the code is
    /// built for, as the build attributes in `.ARM.attributes` say.
    pub arm_architecture: Option<ArmArchitecture>,
    /// The stretches of code that the file's `$d` symbols mark as data,
    /// such as literal pools, as `(start, end)`, sorted. Only 32-bit Arm
    /// files have them, unless they are stripped (see
    /// [`Binary::data_in_code`]).
    data_in_code: Vec<(u64, u64)>,
    /// Problems that did not stop loading but may make answers incomplete.
    pub warnings: Vec<String>,
    /// The references in each function's code, by its address and size;
    /// see [`Binary::code_references`].
    code_references: HashMap<(u64, u64), Vec<CodeReference>>,
    /// The whole file.
    data: Vec<u8>,
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

/// A section of the file, such as `.text` or `.rodata`. An allocated
/// section's end, `address + size`, always fits in a `u64`.
#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub address: u64,
    pub size: u64,
    /// What the section holds, as the `object` crate tells from its type and
    /// flags, such as `Text` for code or `UninitializedData` for `.bss`.
    pub kind: SectionKind,
    /// Whether the section is loaded into memory when the program runs.
    /// Only allocated sections hold items.
    pub allocated: bool,
    /// Where the section's bytes are in the file, as (offset, size). `None`
    /// for sections such as `.bss` that take no space in the file.
    pub file_range: Option<(u64, u64)>,
    /// The size of each entry, for a section that holds a table of them,
    /// such as `.plt.got`, as the section's header gives it; 0 when it
    /// gives none.
    pub entry_size: u64,
    /// Where the file puts the section's bytes in memory, when that isn't
    /// where the program uses them: the address of its load image, which
    /// the program copies to `address` before it uses it. Firmware stores
    /// the starting values of `.data`, and code that runs from RAM, in
    /// flash this way, and its startup code copies them. `None` for most
    /// sections, whose bytes are stored where the program uses them, and
    /// for sections such as `.bss` that take no space in the file.
    pub load_address: Option<u64>,
}

/// A function or data object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The item's symbol name, demangled if it is a mangled Rust or C++
    /// symbol. Without a symbol, a name made from where it is, such as
    /// `fn_0x1690` or `.rodata@0x2004`.
    pub name: String,
    /// Other names for the item: the symbol as the file has it when `name`
    /// is demangled, and other symbols at the same address, such as weak
    /// aliases.
    pub aliases: Vec<String>,
    pub kind: ItemKind,
    /// Where the item starts. On Arm, without the Thumb bit that symbols
    /// and pointers set on a Thumb function's address (see `code_address`).
    pub address: u64,
    /// Size in bytes, or 0 when it is unknown. The item's end,
    /// `address + size`, always fits in a `u64`.
    pub size: u64,
    /// Whether `size` is a guess, made when the file doesn't record the
    /// size: the distance to the next item, or for unnamed data, to the next
    /// address that something refers to. A function found by following the
    /// code leaves out the padding before the next.
    pub size_estimated: bool,
    /// The section holding the item, as an index into [`Binary::sections`].
    pub section: usize,
    /// Where elfq found the item.
    pub source: Source,
}

/// Whether an item is code or data. elfq reads the references in a
/// function's code, and the pointers stored in a data item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemKind {
    /// Code.
    Function,
    /// Anything that isn't code: a variable, a constant, a table or a
    /// string.
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
    /// The procedure linkage table. The item is a stub named after what it
    /// jumps to, an import (`write@plt`) or one of the file's own functions
    /// (`log_msg@plt`), or the table's header named after its section.
    Plt,
    /// A section with a known purpose and no symbol, such as `.init_array`
    /// or the vector table's. The item is named after the section, or when
    /// the vector table is only part of it, after the section and the
    /// table's address, such as `.text@0x8000000`.
    Section,
    /// Following the code from the entry point, the vector table's handlers
    /// or the startup and shutdown arrays, or from a Thumb address that code
    /// loads or data holds, leads to the address, in firmware that nothing
    /// lists functions for
    /// (see `unnamed_functions.rs`). The item is a function named after its
    /// address, such as `fn_0x8000040`.
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

/// What the dynamic loader stores at a relocation.
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
        Binary::from_vec(data).with_context(|| format!("cannot load {}", path.display()))
    }

    /// The whole file, for reading parts of it that elfq doesn't keep, such
    /// as DWARF debug info.
    pub fn file_data(&self) -> &[u8] {
        &self.data
    }

    /// Parses an ELF file that is already in memory.
    pub fn parse(data: &[u8]) -> Result<Binary> {
        Binary::from_vec(data.to_vec())
    }

    /// Parses an ELF file, keeping `data` as the file's bytes rather than a
    /// copy of them, since a large file takes hundreds of megabytes.
    pub(crate) fn from_vec(data: Vec<u8>) -> Result<Binary> {
        let (file, kind) = parse_program(&data)?;
        let architecture = file.architecture();
        let mut warnings = Vec::new();
        let mut binary = Binary {
            architecture,
            pointer_size: if file.is_64() { 8 } else { 4 },
            kind,
            entry: code_address(architecture, file.entry()),
            // Only a damaged header puts 32-bit Arm code in a 64-bit file.
            thumb: architecture == Architecture::Arm && !file.is_64() && is_thumb(&file),
            endianness: if file.is_little_endian() {
                Endianness::Little
            } else {
                Endianness::Big
            },
            has_symbols: file.symbol_table().is_some(),
            has_dwarf: file.section_by_name(".debug_info").is_some(),
            sections: read_sections(&file, data.len() as u64, &mut warnings),
            items: Vec::new(),
            imports: Vec::new(),
            relocations: Vec::new(),
            used_by_unwinder: Vec::new(),
            merged_variables: Vec::new(),
            vector_table: None,
            arm_architecture: None,
            data_in_code: Vec::new(),
            warnings,
            code_references: HashMap::new(),
            data,
        };
        // The bytes have moved into `binary`, so read them there. This
        // reads only the headers again.
        let file = object::File::parse(binary.data.as_slice()).context("malformed ELF file")?;
        (binary.imports, binary.relocations) = relocations::read(&file, architecture);
        if binary.thumb {
            binary.vector_table = vector_table::find(&binary);
        }
        if architecture == Architecture::Arm {
            binary.arm_architecture = attributes::arm_architecture(&file);
            binary.data_in_code = symbols::data_in_code(&file, &binary.sections);
        }

        let (mut items, merged_variables) = symbols::items(&file, &binary.sections);
        binary.merged_variables = merged_variables;
        let stubs = plt::stubs(&binary, &items);
        add_new_items(&mut items, stubs);
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

        let run_on = end_where_functions_start(&mut items);
        sort_items(&mut items);
        estimate_missing_sizes(&mut items, &binary.sections);
        binary.items = items;
        binary.code_references = scan_functions(&binary, &run_on);
        // Last, since finding unnamed data needs all the other items.
        let unnamed = unnamed_data::items(&binary);
        binary.items.extend(unnamed);
        sort_items(&mut binary.items);
        Ok(binary)
    }

    /// The references that the code of `function` makes, as `arch::scan`
    /// finds them, and a jump to the next function when the code runs on
    /// into it (see `end_where_functions_start`): none when elfq can't read
    /// the code. Scanning code is much of what elfq does, so it scans each
    /// function once, as it loads the file.
    pub fn code_references(&self, function: &Item) -> &[CodeReference] {
        self.code_references
            .get(&(function.address, function.size))
            .map_or(&[], Vec::as_slice)
    }

    /// The stretches of the `size` bytes at `address` that the file's `$d`
    /// symbols mark as data among code, as `(start, end)`, in order. Arm's
    /// ELF ABI has assemblers mark data, such as a literal pool, with `$d`,
    /// and code again with `$t` or `$a`. Stripped files have none.
    pub fn data_in_code(&self, address: u64, size: u64) -> impl Iterator<Item = (u64, u64)> + '_ {
        let end = address.saturating_add(size);
        let first = self
            .data_in_code
            .partition_point(|&(_, stretch_end)| stretch_end <= address);
        self.data_in_code[first..]
            .iter()
            .take_while(move |&&(start, _)| start < end)
            .map(move |&(start, stretch_end)| (start.max(address), stretch_end.min(end)))
    }

    /// The bytes at `address`, or `None` if the file doesn't hold all `size`
    /// of them. `.bss`, for example, takes no space in the file.
    pub fn bytes(&self, address: u64, size: u64) -> Option<&[u8]> {
        let section = &self.sections[self.section_at(address)?];
        let bytes = self.section_bytes(section, address, size)?;
        (bytes.len() as u64 == size).then_some(bytes)
    }

    /// The bytes of `item`, as [`Binary::bytes`] reads them, but from the
    /// section that holds it, without looking for it among all of them.
    pub fn item_bytes(&self, item: &Item) -> Option<&[u8]> {
        let section = &self.sections[item.section];
        if !(section.in_memory() && section.contains(item.address)) {
            return self.bytes(item.address, item.size);
        }
        let bytes = self.section_bytes(section, item.address, item.size)?;
        (bytes.len() as u64 == item.size).then_some(bytes)
    }

    /// The pointer stored at `address`, as the file holds it before the
    /// program runs.
    pub fn pointer_at(&self, address: u64) -> Option<u64> {
        let bytes = self.bytes(address, self.pointer_size)?;
        Some(word(bytes, self.endianness))
    }

    /// The pointer-sized words stored one after another from `start` up to
    /// `end`, as `(address, value)`: each one [`Binary::pointer_at`] reads,
    /// from the section that holds it. Words whose bytes the file doesn't
    /// hold are left out, so `.bss`, which takes no space in the file, has
    /// none, however large it is.
    pub fn words(&self, start: u64, end: u64) -> impl Iterator<Item = (u64, u64)> + '_ {
        let size = self.pointer_size;
        let mut next = Some(start);
        // A run of words at a time, all from one section: they are read in
        // one go, and a stretch the file holds no bytes for is skipped in
        // one step.
        let runs = std::iter::from_fn(move || {
            let slot = next.filter(|&slot| slot < end)?;
            let (stop, bytes) = self.run(slot, end);
            let count = (stop - slot).div_ceil(size);
            next = count
                .checked_mul(size)
                .and_then(|length| slot.checked_add(length));
            let words = bytes.chunks_exact(size as usize).take(count as usize);
            Some((slot..stop).step_by(size as usize).zip(words))
        });
        runs.flatten()
            .map(|(address, bytes)| (address, word(bytes, self.endianness)))
    }

    /// The words that [`Binary::words`] reads, but only at addresses that
    /// are multiples of their size, where compilers put pointers in data.
    pub fn aligned_words(&self, start: u64, end: u64) -> impl Iterator<Item = (u64, u64)> + '_ {
        let first = start.checked_next_multiple_of(self.pointer_size);
        self.words(first.unwrap_or(end), end)
    }

    /// Where the words from `slot` up to `end` stop coming from the section
    /// that holds `slot`, and the bytes the file holds for them: none when
    /// no section holds `slot`, or the file holds none of its bytes.
    fn run(&self, slot: u64, end: u64) -> (u64, &[u8]) {
        // Sections can overlap, as GNU ld's OVERLAY makes them, and the
        // first that holds an address is the one that counts, so the run
        // ends where any other one starts too.
        let next_start = self
            .sections
            .iter()
            .filter(|section| section.in_memory() && section.address > slot)
            .map(|section| section.address)
            .min();
        let stop = next_start.map_or(end, |next_start| next_start.min(end));
        match self.section_at(slot) {
            Some(index) => {
                let section = &self.sections[index];
                let stop = stop.min(section.address + section.size);
                (
                    stop,
                    self.section_bytes(section, slot, end - slot)
                        .unwrap_or_default(),
                )
            }
            None => (stop, &[]),
        }
    }

    /// Up to `len` of the bytes the file holds for `section` from
    /// `address`, which the section holds: fewer when the section's bytes,
    /// or the file itself, end first.
    fn section_bytes(&self, section: &Section, address: u64, len: u64) -> Option<&[u8]> {
        let (offset, file_size) = section.file_range?;
        let start = address - section.address;
        let len = len.min(file_size.saturating_sub(start));
        let rest = self
            .data
            .get(usize::try_from(offset.checked_add(start)?).ok()?..)?;
        Some(&rest[..rest.len().min(usize::try_from(len).unwrap_or(usize::MAX))])
    }

    /// The section that holds `address` when the program runs, as an index
    /// into [`Binary::sections`].
    pub fn section_at(&self, address: u64) -> Option<usize> {
        self.sections
            .iter()
            .position(|section| section.in_memory() && section.contains(address))
    }

    /// Where the load image of `item` starts, when its section has one
    /// (see [`Section::load_address`]): where the file stores its bytes for
    /// the program to copy to where it uses them.
    pub fn load_address(&self, item: &Item) -> Option<u64> {
        let section = &self.sections[item.section];
        let offset = item.address.checked_sub(section.address)?;
        section.load_address?.checked_add(offset)
    }

    /// The bytes of `item` that sizes count: all of them, or with a range,
    /// those in it. A range counts the item's load image too, where the
    /// file stores it, so a range that holds both copies counts both.
    pub fn bytes_in(&self, item: &Item, range: Option<AddressRange>) -> u64 {
        match range {
            Some(range) => range
                .overlap(item.address, item.size)
                .saturating_add(self.load_image_bytes_in(item, range)),
            None => item.size,
        }
    }

    /// How many bytes of `item`'s load image `range` holds: 0 when it has
    /// none.
    pub fn load_image_bytes_in(&self, item: &Item, range: AddressRange) -> u64 {
        self.load_address(item)
            .map_or(0, |address| range.overlap(address, item.size))
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

    /// The relocations that store pointers in the `size` bytes at
    /// `address`, in address order.
    pub fn relocations_in(&self, address: u64, size: u64) -> &[Relocation] {
        let start = self.relocations.partition_point(|r| r.address < address);
        let end = address.saturating_add(size);
        let count = self.relocations[start..].partition_point(|r| r.address < end);
        &self.relocations[start..start + count]
    }

    /// The PLT stubs for functions in this file, each with its function, as
    /// indices into [`Binary::items`]. A shared library calls the functions
    /// it exports through stubs such as `log_msg@plt`, and any file calls an
    /// ifunc through one, whose function is the ifunc's resolver (see
    /// `plt.rs`).
    pub fn own_function_stubs(&self) -> HashMap<usize, usize> {
        plt::own_function_stubs(self)
    }
}

impl Item {
    /// Whether `address` is the item's first byte or falls inside it.
    pub fn contains(&self, address: u64) -> bool {
        address == self.address || (address > self.address && address - self.address < self.size)
    }

    /// Whether any of the item's bytes are in one of `ranges`, where an item
    /// of unknown size counts as its first byte.
    pub fn is_in(&self, ranges: &[AddressRange]) -> bool {
        ranges
            .iter()
            .any(|range| range.overlap(self.address, self.size.max(1)) > 0)
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

    /// Whether the section is in the program's memory at its addresses.
    /// `.tbss` isn't: it only describes each thread's copy of its
    /// variables, so other sections share its addresses.
    fn in_memory(&self) -> bool {
        self.allocated && self.kind != SectionKind::UninitializedTls
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

/// The number in `bytes`, a pointer of up to 8 bytes.
pub(crate) fn word(bytes: &[u8], endianness: Endianness) -> u64 {
    let mut word = [0; 8];
    if endianness == Endianness::Big {
        word[8 - bytes.len()..].copy_from_slice(bytes);
        u64::from_be_bytes(word)
    } else {
        word[..bytes.len()].copy_from_slice(bytes);
        u64::from_le_bytes(word)
    }
}

/// Parses `data` as an ELF file that holds a linked program or library,
/// and says which kind.
fn parse_program(data: &[u8]) -> Result<(object::File<'_>, FileKind)> {
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
        ObjectKind::Relocatable => {
            bail!("this is an object file, not a linked program or library")
        }
        ObjectKind::Core => bail!("core dumps are not supported"),
        _ => bail!("unknown ELF file type"),
    };
    Ok((file, kind))
}

/// The file's sections. An allocated section, whose addresses and bytes
/// elfq reads, is ignored if it would run past the last address, which only
/// a damaged header can make it do, and read as far as the file goes if it
/// runs past the end of the file, each with a warning.
fn read_sections(file: &object::File, file_size: u64, warnings: &mut Vec<String>) -> Vec<Section> {
    let segments = load_segments(file);
    let mut sections = Vec::new();
    for section in file.sections() {
        let index = section.index().0;
        if sections.len() <= index {
            sections.resize_with(index + 1, null_section);
        }
        let name = String::from_utf8_lossy(section.name_bytes().unwrap_or_default()).into_owned();
        // A section without a name is shown by its number, as readelf does.
        let shown = if name.is_empty() {
            format!("[{index}]")
        } else {
            name.clone()
        };
        let allocated = match section.flags() {
            SectionFlags::Elf { sh_flags, .. } => sh_flags.contains(object::elf::SHF_ALLOC),
            _ => false,
        };
        let mut size = section.size();
        if allocated {
            let Some(fitting) = fitting_size(section.address(), size) else {
                warnings.push(format!(
                    "ignoring section {shown}: it runs past the last address"
                ));
                sections[index] = Section {
                    name,
                    ..null_section()
                };
                continue;
            };
            size = fitting;
        }
        let file_range = section.file_range();
        let past_the_end = file_range.is_some_and(|(offset, size)| {
            size > 0 && offset.checked_add(size).is_none_or(|end| end > file_size)
        });
        if allocated && past_the_end {
            warnings.push(format!("section {shown} runs past the end of the file"));
        }
        let load_address = match file_range {
            Some((offset, _)) if allocated && size > 0 => {
                load_address(&segments, section.address(), size, offset)
            }
            _ => None,
        };
        sections[index] = Section {
            name,
            address: section.address(),
            size,
            kind: section.kind(),
            allocated,
            file_range,
            entry_size: entry_size(file, section.index()),
            load_address,
        };
    }
    sections
}

/// A loadable segment, as the file's program headers describe it: the bytes
/// of the file that are put in memory.
struct Segment {
    /// Where its bytes are in the file, as (offset, size).
    file_range: (u64, u64),
    /// Where the program uses it.
    address: u64,
    /// Where its bytes are put in memory, which the program copies them
    /// from when that isn't `address`.
    load_address: u64,
    /// Its size in memory, which takes in any `.bss` after its bytes.
    size: u64,
}

/// The file's loadable segments. When the program headers give several
/// segments the load address 0 and the others none, as some linkers do,
/// their load images would overlap, so those addresses say nothing, and
/// each segment is taken to be stored where the program uses it, as GNU's
/// `objdump` and `objcopy` take it. One segment's 0 can be true, as for a
/// program that is stored at 0 and copied to RAM to run.
fn load_segments(file: &object::File) -> Vec<Segment> {
    fn read<Elf: FileHeader>(elf: &ElfFile<'_, Elf>) -> Vec<Segment> {
        let endian = elf.endian();
        elf.elf_program_headers()
            .iter()
            .filter(|header| header.p_type(endian) == object::elf::PT_LOAD)
            .map(|header| Segment {
                file_range: header.file_range(endian),
                address: header.p_vaddr(endian).into(),
                load_address: header.p_paddr(endian).into(),
                size: header.p_memsz(endian).into(),
            })
            .collect()
    }
    let segments = match file {
        object::File::Elf32(elf) => read(elf),
        object::File::Elf64(elf) => read(elf),
        _ => Vec::new(),
    };
    with_load_addresses(segments)
}

/// `segments`, each stored where the program uses it when the program
/// headers give several the load address 0 and the others none (see
/// [`load_segments`]).
fn with_load_addresses(mut segments: Vec<Segment>) -> Vec<Segment> {
    let several = segments.iter().filter(|segment| segment.size > 0).count() > 1;
    if several && segments.iter().all(|segment| segment.load_address == 0) {
        for segment in &mut segments {
            segment.load_address = segment.address;
        }
    }
    segments
}

/// Where the bytes of a section that the program uses at `address`, and
/// that are at `offset` in the file, are put in memory, when that isn't
/// `address`: the segment that holds them says, as GNU's tools read it.
/// `None` when no segment holds all `size` of them where the program uses
/// them, or when the load image would run past the last address, which only
/// a damaged header can make it do.
fn load_address(segments: &[Segment], address: u64, size: u64, offset: u64) -> Option<u64> {
    let end = offset.checked_add(size)?;
    let segment = segments.iter().find(|segment| {
        let (start, length) = segment.file_range;
        offset >= start
            && start.checked_add(length).is_some_and(|stop| end <= stop)
            && address >= segment.address
            && address - segment.address < segment.size
    })?;
    let load_address = segment
        .load_address
        .checked_add(offset - segment.file_range.0)?;
    if fitting_size(load_address, size) != Some(size) {
        return None;
    }
    (load_address != address).then_some(load_address)
}

/// The size of each entry that the header of the section at `index` gives.
fn entry_size(file: &object::File, index: SectionIndex) -> u64 {
    match file {
        object::File::Elf32(elf) => elf.section_by_index(index).map_or(0, |section| {
            section.elf_section_header().sh_entsize(elf.endian()).into()
        }),
        object::File::Elf64(elf) => elf.section_by_index(index).map_or(0, |section| {
            section.elf_section_header().sh_entsize(elf.endian())
        }),
        _ => 0,
    }
}

/// `size`, for something that starts at `address`, made to fit: one byte
/// less when it ends at the very last address, so that its end,
/// `address + size`, fits in a `u64`. `None` when it would end past the
/// last address, which only a damaged header can make it do.
fn fitting_size(address: u64, size: u64) -> Option<u64> {
    if size > 0 && address.checked_add(size - 1).is_none() {
        return None;
    }
    Some(size.min(u64::MAX - address))
}

fn null_section() -> Section {
    Section {
        name: String::new(),
        address: 0,
        size: 0,
        kind: SectionKind::Unknown,
        allocated: false,
        file_range: None,
        entry_size: 0,
        load_address: None,
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
    sections
        .iter()
        .enumerate()
        .filter(|(_, section)| {
            section.allocated && section.size > 0 && STARTUP_ARRAYS.contains(&section.name.as_str())
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

/// Adds a function for each unwind-table range that the items found so far,
/// from symbols and PLT stubs, missed, and fills in sizes that the symbol
/// table left as 0.
fn add_unwind_items(items: &mut Vec<Item>, ranges: &[(u64, u64)], sections: &[Section]) {
    // The functions found so far, as (address, size, index into items).
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
        // A range that would end past the last address is damaged.
        let Some(length) = fitting_size(start, length) else {
            continue;
        };
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
    let covered = items
        .iter()
        .any(|item| item.kind == ItemKind::Function && item.contains(entry));
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

/// Ends each function where the next one starts, when that is inside it
/// and the functions that start inside it run to its end. Hand-written
/// code can give a function several entry points that share its end, each
/// with a symbol whose size runs to that end: libgcc's `__aeabi_dsub` flips
/// the sign of its second argument and runs on into `__adddf3`. Ending each
/// entry where the next starts counts each byte once. A function whose
/// code goes on after the functions inside it stays whole. Returns the
/// functions it ended, by address and size, as the code of each may run on
/// into the next.
fn end_where_functions_start(items: &mut [Item]) -> HashSet<(u64, u64)> {
    // Each function's section, start and end, sorted.
    let mut functions: Vec<(usize, u64, u64)> = items
        .iter()
        .filter(|item| item.kind == ItemKind::Function)
        .map(|item| (item.section, item.address, item.address + item.size))
        .collect();
    functions.sort_unstable();
    let mut ended = HashSet::new();
    for item in items
        .iter_mut()
        .filter(|item| item.kind == ItemKind::Function)
    {
        let end = item.address + item.size;
        let after = functions.partition_point(|&(section, address, _)| {
            (section, address) <= (item.section, item.address)
        });
        let mut inside = functions[after..]
            .iter()
            .take_while(|&&(section, address, _)| section == item.section && address < end)
            .peekable();
        let Some(&&(_, next, _)) = inside.peek() else {
            continue;
        };
        if inside.all(|&(_, _, inner_end)| inner_end < end) {
            continue;
        }
        item.size = next - item.address;
        ended.insert((item.address, item.size));
    }
    ended
}

/// The references in the code of each function in `binary.items`, by the
/// function's address and size; see [`Binary::code_references`]. Each of
/// `run_on`, the functions that `end_where_functions_start` ended, also
/// jumps to the function after it if its code runs on into it, as a tail
/// call does.
fn scan_functions(
    binary: &Binary,
    run_on: &HashSet<(u64, u64)>,
) -> HashMap<(u64, u64), Vec<CodeReference>> {
    if !arch::is_supported(binary.architecture, binary.thumb) {
        return HashMap::new();
    }
    let functions = binary
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Function);
    functions
        .filter_map(|function| {
            let key = (function.address, function.size);
            let code = binary.bytes(function.address, function.size)?;
            let mut references = arch::scan(
                binary.architecture,
                binary.endianness,
                code,
                function.address,
            );
            if run_on.contains(&key) {
                references.extend(run_on_jump(binary, function, code, &references));
            }
            Some((key, references))
        })
        .collect()
}

/// The jump from the last instruction of `function`, whose `code` makes
/// `references`, to the function that starts where it ends, when the code
/// runs on into it (see `arch::runs_on`). `None` when it doesn't, or when
/// that instruction is a conditional jump there, which `references`
/// already has.
fn run_on_jump(
    binary: &Binary,
    function: &Item,
    code: &[u8],
    references: &[CodeReference],
) -> Option<CodeReference> {
    let next = function.address + function.size;
    let site = arch::runs_on(
        binary.architecture,
        binary.endianness,
        code,
        function.address,
    )?;
    let jumps_there = references.iter().any(|reference| {
        reference.site == site
            && matches!(
                reference.kind,
                CodeReferenceKind::Jump(to) | CodeReferenceKind::TailJump(to) if to == next
            )
    });
    (!jumps_there).then_some(CodeReference {
        site,
        kind: CodeReferenceKind::TailJump(next),
    })
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

impl Binary {
    /// An x86-64 executable for unit tests, with `sections` and `items`,
    /// whose file is `data`.
    #[cfg(test)]
    pub(crate) fn for_tests(sections: Vec<Section>, items: Vec<Item>, data: Vec<u8>) -> Binary {
        Binary {
            architecture: Architecture::X86_64,
            pointer_size: 8,
            kind: FileKind::Executable,
            entry: 0x1000,
            thumb: false,
            endianness: Endianness::Little,
            has_symbols: true,
            has_dwarf: false,
            sections,
            items,
            imports: Vec::new(),
            relocations: Vec::new(),
            used_by_unwinder: Vec::new(),
            merged_variables: Vec::new(),
            vector_table: None,
            arm_architecture: None,
            data_in_code: Vec::new(),
            warnings: Vec::new(),
            code_references: HashMap::new(),
            data,
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
            entry_size: 0,
            load_address: None,
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
            (0x1000, 0x30),     // same start as a symbol: the symbol's size wins
            (0x1010, 0x08),     // inside a symbol, such as a split-off cold part
            (0x1100, 0x40),     // the symbol has no size: take the table's
            (0x1200, 0x10),     // no symbol at all: a new, unnamed function
            (0x9000, 0x10),     // outside any code section: ignored
            (0x1300, u64::MAX), // would end past the last address: ignored
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
    fn entry_points_end_where_the_next_starts() {
        // As libgcc's __gedf2, __ledf2 and __cmpdf2: each symbol runs to
        // the end they share.
        let mut items = vec![
            function("__cmpdf2", 0x1010, 0x70),
            function("__gedf2", 0x1000, 0x80),
            function("__ledf2", 0x1008, 0x78),
            // A function of unknown size, a function at the same address,
            // and data inside a function, change nothing.
            function("unsized", 0x2000, 0),
            function("whole", 0x3000, 0x20),
            function("alias", 0x3000, 0x10),
            Item {
                kind: ItemKind::Data,
                ..function("table", 0x3004, 4)
            },
            // Nor does a function in another section, at an address inside
            // one.
            Item {
                section: 1,
                ..function("elsewhere", 0x3010, 4)
            },
            // A function whose code goes on after the one inside it stays
            // whole, as does one with a function of unknown size inside.
            function("outer", 0x4000, 0x20),
            function("inner", 0x4008, 8),
            function("around", 0x5000, 0x20),
            function("unknown", 0x5010, 0),
        ];
        let ended = end_where_functions_start(&mut items);
        let sizes: Vec<(&str, u64)> = items
            .iter()
            .map(|item| (item.name.as_str(), item.size))
            .collect();
        assert_eq!(
            sizes,
            [
                ("__cmpdf2", 0x70),
                ("__gedf2", 8),
                ("__ledf2", 8),
                ("unsized", 0),
                ("whole", 0x20),
                ("alias", 0x10),
                ("table", 4),
                ("elsewhere", 4),
                ("outer", 0x20),
                ("inner", 8),
                ("around", 0x20),
                ("unknown", 0),
            ]
        );
        assert_eq!(ended, HashSet::from([(0x1000, 8), (0x1008, 8)]));
    }

    /// An x86-64 file whose `.text` at 0x1000 is `code`, holding `items`.
    fn x86_64_binary(code: Vec<u8>, items: Vec<Item>) -> Binary {
        let text = Section {
            size: code.len() as u64,
            file_range: Some((0, code.len() as u64)),
            ..text_section()
        };
        let items = items
            .into_iter()
            .map(|item| Item { section: 1, ..item })
            .collect();
        Binary::for_tests(vec![null_section(), text], items, code)
    }

    #[test]
    fn an_entry_point_that_runs_on_tail_calls_the_next() {
        // neg esi, which runs on into add's lea eax, [rdi + rsi]; ret.
        let mut items = vec![function("subtract", 0x1000, 6), function("add", 0x1002, 4)];
        let ended = end_where_functions_start(&mut items);
        let binary = x86_64_binary(vec![0xf7, 0xde, 0x8d, 0x04, 0x37, 0xc3], items);
        let references = scan_functions(&binary, &ended);
        let run_on = CodeReference {
            site: 0x1000,
            kind: CodeReferenceKind::TailJump(0x1002),
        };
        assert_eq!(references[&(0x1000, 2)], [run_on]);
        assert_eq!(references[&(0x1002, 4)], []);

        // test esi, esi; jne 0x1004, a conditional jump to the next
        // entry, is one reference to it, not two; and ret runs on nowhere.
        let mut items = vec![
            function("skip", 0x1000, 9),
            function("add", 0x1004, 5),
            function("done", 0x1008, 1),
        ];
        let ended = end_where_functions_start(&mut items);
        let code = vec![0x85, 0xf6, 0x75, 0x00, 0x8d, 0x04, 0x37, 0xc3, 0xc3];
        let binary = x86_64_binary(code, items);
        let references = scan_functions(&binary, &ended);
        let jump = CodeReference {
            site: 0x1002,
            kind: CodeReferenceKind::Jump(0x1004),
        };
        assert_eq!(references[&(0x1000, 4)], [jump]);
        assert_eq!(references[&(0x1004, 4)], []);
    }

    #[test]
    fn a_segment_says_where_its_sections_are_stored() {
        // As in firmware: code in flash, then .data and .bss in RAM, whose
        // bytes the file stores in flash after the code.
        let segment = |file_range, address, load_address, size| Segment {
            file_range,
            address,
            load_address,
            size,
        };
        let segments = [
            segment((0x1000, 0x800), 0x0800_0000, 0x0800_0000, 0x800),
            segment((0x2000, 0x20), 0x2000_0000, 0x0800_0800, 0x48),
        ];
        // .text is stored where it runs, and .data's bytes in flash.
        assert_eq!(load_address(&segments, 0x0800_0000, 0x800, 0x1000), None);
        assert_eq!(
            load_address(&segments, 0x2000_0000, 0x20, 0x2000),
            Some(0x0800_0800)
        );
        // Bytes that run past the segment's in the file, or that the
        // program uses outside it, aren't in it.
        assert_eq!(load_address(&segments, 0x2000_0000, 0x21, 0x2000), None);
        assert_eq!(load_address(&segments, 0x2000_0048, 0x10, 0x2000), None);
        // When every load address is 0, none of them says anything.
        let unset = with_load_addresses(vec![
            segment((0x1000, 0x800), 0x0800_0000, 0, 0x800),
            segment((0x2000, 0x20), 0x2000_0000, 0, 0x48),
        ]);
        assert_eq!(load_address(&unset, 0x2000_0000, 0x20, 0x2000), None);
        // Unless there is only one: a program stored at 0 that runs from
        // RAM.
        let stored_at_0 = with_load_addresses(vec![segment((0x1000, 0x84), 0x2000_0000, 0, 0xa4)]);
        assert_eq!(
            load_address(&stored_at_0, 0x2000_0044, 0x40, 0x1044),
            Some(0x44)
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
