//! Loading an ELF file and finding the items in it.
//!
//! An *item* is a function or a data object: a named range of addresses that
//! the rest of elfq reasons about. [`Binary::load`] first reads what it
//! needs before it looks for items: its sections, the dynamic relocations
//! (see `relocations.rs`), which say where the program stores pointers and
//! which functions and variables it imports from shared libraries, and in
//! Cortex-M firmware the vector table (see `vector_table.rs`). Then it finds
//! items in these places, each one only adding what the earlier ones missed:
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
//! Before the last, with every function found, it settles their sizes and
//! scans each function's code once (see [`Binary::code_references`]).
//! `load.rs` takes these steps, in this order.
//!
//! # How the files are organized
//!
//! - This file has the types, such as [`Binary`], [`Section`] and
//!   [`Item`], the queries that the rest of elfq asks of them, and the
//!   reading of the bytes and pointers that the file stores at an address.
//! - `load.rs` loads a file: it reads everything but the items, then takes
//!   the steps above in order. It holds the steps, or parts of them, that
//!   have no file of their own.
//! - `sections.rs` parses the file, refusing one that isn't a linked program
//!   or library, and reads the section headers, and from the program
//!   headers, where each section's load image is, for `load.rs` and
//!   `memory.rs`.
//! - `symbols.rs`, `plt.rs`, `vector_table.rs`, `unnamed_functions.rs` and
//!   `unnamed_data.rs` find the items of the steps that name them.
//!   `eh_frame.rs` reads the unwind table's function ranges and personality
//!   routines, `relocations.rs` the dynamic relocations, and `attributes.rs`
//!   the version of the Arm architecture that the code is built for.
//! - `names.rs` makes mangled names readable, and works out the short
//!   names that people type for long ones.
//! - `memory.rs` has [`LoadedMemory`], which reads only a file's section
//!   headers and the bytes that its sections put in memory, which `diff`
//!   compares before it needs any item. `header.rs` reads only the ELF
//!   header and program headers, for `startup` to refuse a file for
//!   another processor before loading it.
//! - `range.rs` has address ranges, and the blocks of addresses that the
//!   sections and their load images occupy.
//! - `startup.rs` gives the startup walk (`arch::startup::Program`) the
//!   file's code, the words that memory holds at reset, as in flash or a
//!   load image, and where its functions are.
//!
//! The unit tests of `names.rs` and `vector_table.rs` are in
//! `names/tests.rs` and `vector_table/tests.rs`.

mod attributes;
mod eh_frame;
mod header;
mod load;
mod memory;
pub mod names;
mod plt;
mod range;
mod relocations;
mod sections;
mod startup;
mod symbols;
mod unnamed_data;
mod unnamed_functions;
mod vector_table;

pub use attributes::ArmArchitecture;
pub use header::ElfHeader;
pub use memory::{Bytes, LoadedMemory, same_bytes_in, same_memory};
pub use range::{AddressRange, MemoryBlock, hex_number, memory_blocks, range_hint};
pub use vector_table::{VectorTable, exception_name, is_reserved};

use std::collections::HashMap;
use std::fmt;

use object::{Architecture, Endianness, SectionKind};

use crate::arch::{CodeReference, InstructionSet};

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
    /// functions have bit 0 set in their symbols and in the entry point, and
    /// a file built for a Cortex-M core holds only Thumb code.
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
    /// see [`Binary::code_references`]. Not by item index, since unnamed
    /// data is added, and the items sorted again, after the scan.
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
    /// The whole file, for reading parts of it that elfq doesn't keep, such
    /// as DWARF debug info.
    pub fn file_data(&self) -> &[u8] {
        &self.data
    }

    /// The references that the code of `function` makes, as
    /// `InstructionSet::scan` finds them, and a jump to the next function
    /// when the code runs on into it (see `end_where_functions_start`):
    /// none when elfq can't read the code. Scanning code is much of what
    /// elfq does, so it scans each function once, as it loads the file.
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
            let (stop, bytes) = self.words_run(slot, end);
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
    fn words_run(&self, slot: u64, end: u64) -> (u64, &[u8]) {
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

    /// The instruction set that elfq reads the file's code in, or `None`
    /// when it can't read it yet (see `InstructionSet::of`).
    pub fn instruction_set(&self) -> Option<InstructionSet> {
        InstructionSet::of(self.architecture, self.thumb, self.endianness)
    }

    /// Where the reset handler starts, which the vector table names, when
    /// the file has one: the code a Cortex-M processor runs from reset.
    pub fn reset_handler(&self) -> Option<u64> {
        (self.vector_table.as_ref()).and_then(|table| table.reset_handler())
    }

    /// The allocated sections that have bytes in `range`.
    pub fn sections_in(&self, range: AddressRange) -> Vec<&Section> {
        self.sections
            .iter()
            .filter(|section| section.allocated && range.overlap(section.address, section.size) > 0)
            .collect()
    }

    /// The sections whose load images have bytes in `range`.
    pub fn load_images_in(&self, range: AddressRange) -> Vec<&Section> {
        self.sections
            .iter()
            .filter(|section| {
                section
                    .load_address
                    .is_some_and(|address| range.overlap(address, section.size) > 0)
            })
            .collect()
    }

    /// Whether the entry point is in the vector table, as in firmware linked
    /// with no entry point, which is where `.text` starts, when `.text`
    /// begins with the table. It is data then, not code.
    pub fn entry_in_vector_table(&self) -> bool {
        (self.vector_table.as_ref()).is_some_and(|table| table.contains(self.entry))
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
    /// [`Binary::items`]: of the items that start at the last address at or
    /// before `address`, the smallest that holds it, as when a function of
    /// unknown size and a data object start at one address. An item that
    /// starts before that address isn't found, though it may hold `address`
    /// too: a function whose code goes on after one that starts inside it
    /// stays whole (see `end_where_functions_start`), and after the inner
    /// one ends, `item_at` doesn't find the outer one: it finds an item that
    /// starts after the inner one, such as unnamed data that step 7 adds
    /// there, or nothing.
    pub fn item_at(&self, address: u64) -> Option<usize> {
        let end = self.items.partition_point(|item| item.address <= address);
        let start = self.items.get(end.checked_sub(1)?)?.address;
        (0..end)
            .rev()
            .take_while(|&index| self.items[index].address == start)
            .find(|&index| self.items[index].contains(address))
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

    /// The addresses the item occupies, where an item of unknown size
    /// counts as its first byte.
    pub fn range(&self) -> AddressRange {
        AddressRange {
            start: self.address,
            end: self.address.saturating_add(self.size.max(1)),
        }
    }

    /// Whether any of the item's bytes, as [`Item::range`] gives them, are
    /// in one of `ranges`.
    pub fn is_in(&self, ranges: &[AddressRange]) -> bool {
        let own = self.range();
        let size = own.end - own.start;
        ranges
            .iter()
            .any(|range| range.overlap(own.start, size) > 0)
    }

    /// The item's name, then its aliases.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str()).chain(self.aliases.iter().map(String::as_str))
    }

    /// Whether the item is a function whose end elfq doesn't know, so that
    /// it can't read its code.
    pub fn end_unknown(&self) -> bool {
        self.kind == ItemKind::Function && self.size == 0
    }

    /// Whether the item is a function whose code is in this file and can be
    /// read: not a PLT stub, whose code is a library's, nor a function whose
    /// end elfq doesn't know.
    pub fn is_own_function(&self) -> bool {
        self.kind == ItemKind::Function && self.source != Source::Plt && self.size > 0
    }
}

/// The bytes of functions and the bytes of data objects, counted apart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodeAndData {
    pub code: u64,
    pub data: u64,
}

impl CodeAndData {
    /// Counts `item`'s bytes, as code or as data. Sums stop at the largest
    /// `u64` rather than overflow.
    pub fn add(&mut self, item: &Item) {
        let bytes = match item.kind {
            ItemKind::Function => &mut self.code,
            ItemKind::Data => &mut self.data,
        };
        *bytes = bytes.saturating_add(item.size);
    }

    /// The bytes of code and data together.
    pub fn total(self) -> u64 {
        self.code.saturating_add(self.data)
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

/// Clears the Thumb bit that 32-bit Arm sets in the addresses of Thumb
/// functions: the code itself starts at the even address.
fn code_address(architecture: Architecture, address: u64) -> u64 {
    if architecture == Architecture::Arm {
        address & !1
    } else {
        address
    }
}

/// The sum of `sizes`, which stops at the largest `u64` rather than
/// overflow: in a damaged file, items or inlined copies can overlap, and
/// their sizes add up to more than a `u64` holds.
pub fn total(sizes: impl Iterator<Item = u64>) -> u64 {
    sizes.fold(0, u64::saturating_add)
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

/// The executable section containing `address`, if any.
fn code_section(sections: &[Section], address: u64) -> Option<usize> {
    sections
        .iter()
        .position(|s| s.allocated && s.kind == SectionKind::Text && s.contains(address))
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
