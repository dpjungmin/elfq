//! Changing a sample's bytes, to make damaged and unusual files.
//!
//! The tests read the files it saves. [`ElfBytes`] changes the fields of
//! the ELF header, the section headers, the program headers and the symbol
//! table by name. Each field's place in a 32-bit or a 64-bit file comes
//! from the `object` crate's structs, and a test sets it to one of
//! `object::elf`'s constants where there is one.

use std::mem::offset_of;
use std::ops::Deref;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use object::elf::{
    self, FileHeader32, FileHeader64, ProgramHeader32, ProgramHeader64, Rela64, SectionHeader32,
    SectionHeader64, Sym32, Sym64,
};
use object::{LittleEndian, Object, ObjectSection, ObjectSymbol};

use crate::support::{SAMPLE, sample_path};

/// A sample's bytes, to change and save for elfq to read. The samples are
/// little-endian, and each field is read and written so.
pub struct ElfBytes {
    bytes: Vec<u8>,
    /// Whether it is a 64-bit file, whose fields are placed differently.
    is_64: bool,
}

/// A field of the ELF header.
#[derive(Clone, Copy)]
pub enum HeaderField {
    /// `e_type`: what kind of file it is, as `elf::ET_EXEC`.
    Type,
    /// `e_machine`: the processor that its code is for, as `elf::EM_ARM`.
    Machine,
    /// `e_entry`: the entry point, where the program starts.
    EntryPoint,
    /// `e_phoff`: where the program headers are in the file.
    ProgramHeaders,
    /// `e_phentsize`: the size of each program header.
    ProgramHeaderSize,
    /// `e_phnum`: how many program headers there are.
    ProgramHeaderCount,
    /// `e_shoff`: where the section headers are in the file.
    SectionHeaders,
    /// `e_shentsize`: the size of each section header.
    SectionHeaderSize,
    /// `e_shnum`: how many section headers there are.
    SectionHeaderCount,
    /// `e_shstrndx`: the index of the section holding the sections' names.
    SectionNames,
}

/// A field of a section header.
#[derive(Clone, Copy)]
pub enum SectionField {
    /// `sh_type`: what the section holds, as `elf::SHT_PROGBITS`.
    Type,
    /// `sh_flags`, such as `elf::SHF_ALLOC` for a section the program loads.
    Flags,
    /// `sh_addr`: the section's address.
    Address,
    /// `sh_offset`: where the section's bytes are in the file.
    Offset,
    /// `sh_size`: the section's size.
    Size,
}

/// A field of a program header, which describes a segment.
#[derive(Clone, Copy)]
pub enum SegmentField {
    /// `p_type`: what the segment is, as `elf::PT_LOAD`.
    Type,
    /// `p_vaddr`: where the program uses the segment.
    Address,
    /// `p_paddr`: the segment's load address.
    ///
    /// It is the address where the file stores the segment's bytes. Where
    /// that isn't its address, those bytes are a load image, which startup
    /// code copies to its address.
    LoadAddress,
    /// `p_filesz`: how many of the segment's bytes the file holds.
    FileSize,
    /// `p_memsz`: the segment's size in memory.
    MemorySize,
}

/// What a field can be set to: a number, or a constant.
///
/// The constants are `object::elf`'s, such as `elf::EM_ARM`.
pub trait FieldValue {
    fn number(self) -> u64;
}

impl FieldValue for u64 {
    fn number(self) -> u64 {
        self
    }
}

/// [`FieldValue`] for the types of `object::elf`'s constants.
macro_rules! constant_values {
    ($($constant:ident),*) => {
        $(impl FieldValue for elf::$constant {
            fn number(self) -> u64 {
                self.0.into()
            }
        })*
    };
}

constant_values!(FileType, Machine, SectionType, SectionFlags, ProgramType);

/// Where a field is in a 32-bit file, and in a 64-bit one.
///
/// Each is its offset in a header and its width in bytes.
#[derive(Clone, Copy)]
struct Place {
    in_32: (usize, usize),
    in_64: (usize, usize),
}

/// The width of the field of `S` that `field` borrows.
fn width<S, F>(_field: fn(&S) -> &F) -> usize {
    size_of::<F>()
}

/// The [`Place`] of `$field` in `$struct32` and `$struct64`.
///
/// Both are `object::elf`'s structs.
macro_rules! place {
    ($struct32:ident, $struct64:ident, $field:ident) => {
        Place {
            in_32: (
                offset_of!($struct32<LittleEndian>, $field),
                width(|header: &$struct32<LittleEndian>| &header.$field),
            ),
            in_64: (
                offset_of!($struct64<LittleEndian>, $field),
                width(|header: &$struct64<LittleEndian>| &header.$field),
            ),
        }
    };
}

impl HeaderField {
    fn place(self) -> Place {
        match self {
            Self::Type => place!(FileHeader32, FileHeader64, e_type),
            Self::Machine => place!(FileHeader32, FileHeader64, e_machine),
            Self::EntryPoint => place!(FileHeader32, FileHeader64, e_entry),
            Self::ProgramHeaders => place!(FileHeader32, FileHeader64, e_phoff),
            Self::ProgramHeaderSize => place!(FileHeader32, FileHeader64, e_phentsize),
            Self::ProgramHeaderCount => place!(FileHeader32, FileHeader64, e_phnum),
            Self::SectionHeaders => place!(FileHeader32, FileHeader64, e_shoff),
            Self::SectionHeaderSize => place!(FileHeader32, FileHeader64, e_shentsize),
            Self::SectionHeaderCount => place!(FileHeader32, FileHeader64, e_shnum),
            Self::SectionNames => place!(FileHeader32, FileHeader64, e_shstrndx),
        }
    }
}

impl SectionField {
    fn place(self) -> Place {
        match self {
            Self::Type => place!(SectionHeader32, SectionHeader64, sh_type),
            Self::Flags => place!(SectionHeader32, SectionHeader64, sh_flags),
            Self::Address => place!(SectionHeader32, SectionHeader64, sh_addr),
            Self::Offset => place!(SectionHeader32, SectionHeader64, sh_offset),
            Self::Size => place!(SectionHeader32, SectionHeader64, sh_size),
        }
    }
}

impl SegmentField {
    fn place(self) -> Place {
        match self {
            Self::Type => place!(ProgramHeader32, ProgramHeader64, p_type),
            Self::Address => place!(ProgramHeader32, ProgramHeader64, p_vaddr),
            Self::LoadAddress => place!(ProgramHeader32, ProgramHeader64, p_paddr),
            Self::FileSize => place!(ProgramHeader32, ProgramHeader64, p_filesz),
            Self::MemorySize => place!(ProgramHeader32, ProgramHeader64, p_memsz),
        }
    }
}

impl Deref for ElfBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

impl ElfBytes {
    /// The sample at `path`, as bytes to change.
    pub fn sample(path: &str) -> Self {
        let bytes = std::fs::read(sample_path(path)).unwrap();
        let file = object::File::parse(&*bytes).unwrap_or_else(|err| panic!("{path}: {err}"));
        assert!(file.is_little_endian(), "{path}");
        let is_64 = file.is_64();
        Self { bytes, is_64 }
    }

    /// Saves the bytes as `name` and returns its path.
    ///
    /// The file goes in a folder for the tests' files. Tests run in
    /// parallel, and some save the same file, so it is written under a
    /// name of its own first and then renamed: elfq never reads a file
    /// that another test is still writing.
    pub fn save(&self, name: &str) -> String {
        static SAVED: AtomicUsize = AtomicUsize::new(0);
        let folder = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let partial = folder.join(format!(
            "{name}.{}.{}.part",
            std::process::id(),
            SAVED.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&partial, &self.bytes).unwrap();
        let path = folder.join(name);
        std::fs::rename(&partial, &path).unwrap();
        path.to_str().unwrap().to_string()
    }

    /// `field` in the ELF header.
    pub fn header(&self, field: HeaderField) -> u64 {
        self.read(0, field.place())
    }

    /// Sets `field` in the ELF header to `value`.
    pub fn set(&mut self, field: HeaderField, value: impl FieldValue) -> &mut Self {
        self.write(0, field.place(), value.number())
    }

    /// Sets `field` in the header of the section `section` to `value`.
    pub fn set_section(
        &mut self,
        section: &str,
        field: SectionField,
        value: impl FieldValue,
    ) -> &mut Self {
        let index = self.section_index(section);
        self.set_section_at(index, field, value)
    }

    /// `field` in the header of the section at `index`, counting from 0.
    pub fn section_at(&self, index: usize, field: SectionField) -> u64 {
        self.read(self.section_header(index), field.place())
    }

    /// Sets `field` in the header of the section at `index` to `value`.
    ///
    /// The index counts from 0.
    pub fn set_section_at(
        &mut self,
        index: usize,
        field: SectionField,
        value: impl FieldValue,
    ) -> &mut Self {
        let header = self.section_header(index);
        self.write(header, field.place(), value.number())
    }

    /// The bytes of the section named `section`, to change in place.
    pub fn section_bytes_mut(&mut self, section: &str) -> &mut [u8] {
        let index = self.section_index(section);
        let offset = self.section_at(index, SectionField::Offset) as usize;
        let size = self.section_at(index, SectionField::Size) as usize;
        &mut self.bytes[offset..offset + size]
    }

    /// Points the section named `section` at `bytes`, put at the file's end.
    pub fn replace_section(&mut self, section: &str, bytes: &[u8]) -> &mut Self {
        let offset = self.bytes.len() as u64;
        self.bytes.extend_from_slice(bytes);
        self.set_section(section, SectionField::Offset, offset)
            .set_section(section, SectionField::Size, bytes.len() as u64)
    }

    /// The address that the symbol table gives `symbol`.
    pub fn symbol_address(&self, symbol: &str) -> u64 {
        let file = object::File::parse(&*self.bytes).unwrap();
        file.symbol_by_name(symbol).unwrap().address()
    }

    /// Sets the size that `.symtab` gives `symbol` to `size`.
    pub fn set_symbol_size(&mut self, symbol: &str, size: u64) -> &mut Self {
        let file = object::File::parse(&*self.bytes).unwrap();
        let index = file.symbol_by_name(symbol).unwrap().index().0;
        let table = self.section_at(self.section_index(".symtab"), SectionField::Offset) as usize;
        let entry_size = if self.is_64 {
            size_of::<Sym64<LittleEndian>>()
        } else {
            size_of::<Sym32<LittleEndian>>()
        };
        let st_size = place!(Sym32, Sym64, st_size);
        self.write(table + index * entry_size, st_size, size)
    }

    /// Gives the symbol named `old` the name `new`, which is as long.
    ///
    /// It changes the name in `.strtab`, where it is.
    pub fn rename_symbol(&mut self, old: &str, new: &str) -> &mut Self {
        assert_eq!(old.len(), new.len(), "{old} and {new}");
        let strtab = self.section_bytes_mut(".strtab");
        let name = [b"\0", old.as_bytes(), b"\0"].concat();
        let at = strtab
            .windows(name.len())
            .position(|bytes| bytes == name)
            .unwrap_or_else(|| panic!("no {old} in .strtab"));
        strtab[at + 1..at + 1 + new.len()].copy_from_slice(new.as_bytes());
        self
    }

    /// The relocations that the section `section` lists, to change in place.
    ///
    /// The file is 64-bit, and they are relocations with addends.
    pub fn relocations_mut(&mut self, section: &str) -> &mut [Rela64<LittleEndian>] {
        assert!(self.is_64);
        object::pod::slice_from_all_bytes_mut(self.section_bytes_mut(section)).unwrap()
    }

    /// Sets entry `entry` of the vector table to `value`.
    ///
    /// The vector table is the section `.isr_vector`. Its entries are
    /// numbered from the stack pointer's, as exceptions are.
    pub fn set_vector_entry(&mut self, entry: usize, value: u64) -> &mut Self {
        assert!(!self.is_64, "Cortex-M firmware is 32-bit");
        let table = self.section_at(self.section_index(".isr_vector"), SectionField::Offset);
        self.write_number(table as usize + entry * 4, 4, value);
        self
    }

    /// `field` in the program header at `index`, counting from 0.
    pub fn segment_at(&self, index: usize, field: SegmentField) -> u64 {
        self.read(self.program_header(index), field.place())
    }

    /// Sets `field` in the program header at `index` to `value`.
    ///
    /// The index counts from 0.
    pub fn set_segment_at(
        &mut self,
        index: usize,
        field: SegmentField,
        value: impl FieldValue,
    ) -> &mut Self {
        let header = self.program_header(index);
        self.write(header, field.place(), value.number())
    }

    /// The index of the program header of the load image for `address`.
    ///
    /// It is the segment that the program uses at `address` and that the
    /// file stores elsewhere, as a load image: the one whose load address
    /// isn't `address`.
    pub fn load_image_segment(&self, address: u64) -> usize {
        (0..self.header(HeaderField::ProgramHeaderCount) as usize)
            .find(|&index| {
                self.segment_at(index, SegmentField::Address) == address
                    && self.segment_at(index, SegmentField::LoadAddress) != address
            })
            .unwrap_or_else(|| panic!("no load image for {address:#x}"))
    }

    /// Marks the file as AArch64 code, which elfq can't read yet. When elfq
    /// learns to read it, this must name another machine, and the tests
    /// that name AArch64 must change with it.
    pub fn mark_code_unreadable(&mut self) -> &mut Self {
        self.set(HeaderField::Machine, elf::EM_AARCH64)
    }

    /// The index of the section named `section`.
    fn section_index(&self, section: &str) -> usize {
        let file = object::File::parse(&*self.bytes).unwrap();
        (file.section_by_name(section))
            .unwrap_or_else(|| panic!("no section {section}"))
            .index()
            .0
    }

    /// Where the header of the section at `index` is in the file.
    fn section_header(&self, index: usize) -> usize {
        let headers = self.header(HeaderField::SectionHeaders);
        let size = self.header(HeaderField::SectionHeaderSize);
        (headers + index as u64 * size) as usize
    }

    /// Where the program header at `index` is in the file.
    fn program_header(&self, index: usize) -> usize {
        let headers = self.header(HeaderField::ProgramHeaders);
        let size = self.header(HeaderField::ProgramHeaderSize);
        (headers + index as u64 * size) as usize
    }

    /// The field at `place` in the header at `header` in the file.
    fn read(&self, header: usize, place: Place) -> u64 {
        let (offset, width) = if self.is_64 { place.in_64 } else { place.in_32 };
        let mut bytes = [0; 8];
        bytes[..width].copy_from_slice(&self.bytes[header + offset..header + offset + width]);
        u64::from_le_bytes(bytes)
    }

    /// Sets the field at `place` in the header at `header` to `value`.
    fn write(&mut self, header: usize, place: Place, value: u64) -> &mut Self {
        let (offset, width) = if self.is_64 { place.in_64 } else { place.in_32 };
        self.write_number(header + offset, width, value);
        self
    }

    /// Writes `value` over the `width` bytes at `at`, which must hold it.
    fn write_number(&mut self, at: usize, width: usize, value: u64) {
        assert!(
            width == 8 || value >> (8 * width) == 0,
            "{value:#x} takes more than {width} bytes"
        );
        self.bytes[at..at + width].copy_from_slice(&value.to_le_bytes()[..width]);
    }
}

/// The sample at `path`, with build attributes for Arm version `version`.
///
/// The attributes say only that its code is for that version of the Arm
/// architecture (Tag_CPU_arch), on a Cortex-M core; or, when `version` is
/// `None`, `.ARM.attributes` is empty, which says nothing.
pub fn for_arm_version(path: &str, version: Option<u8>) -> ElfBytes {
    let mut section = Vec::new();
    if let Some(version) = version {
        // Version `A`, then the `aeabi` vendor's attributes, then those of
        // the whole file (tag 1): Tag_CPU_arch (6) and
        // Tag_CPU_arch_profile (7), each with its length first.
        let file = [1, 9, 0, 0, 0, 6, version, 7, b'M'];
        section.push(b'A');
        section.extend((file.len() as u32 + 10).to_le_bytes());
        section.extend(b"aeabi\0");
        section.extend(file);
    }
    let mut data = ElfBytes::sample(path);
    data.replace_section(".ARM.attributes", &section);
    data
}

/// The x86-64 sample, with the bytes of each of `sections` past the end
/// of the file.
pub fn past_the_end(sections: &[&str]) -> ElfBytes {
    let mut data = ElfBytes::sample(SAMPLE);
    for section in sections {
        data.set_section(section, SectionField::Offset, u64::MAX - 15);
    }
    data
}

/// The sample at `path` without section headers.
///
/// Its `e_shoff`, `e_shnum` and `e_shstrndx` are 0.
pub fn without_section_headers(path: &str) -> ElfBytes {
    let mut data = ElfBytes::sample(path);
    data.set(HeaderField::SectionHeaders, 0)
        .set(HeaderField::SectionHeaderCount, 0)
        .set(HeaderField::SectionNames, 0);
    data
}

/// Stripped firmware whose code is `count` Thumb functions, in place of
/// `.text`, that each call the one before. It starts at the last. The first
/// function takes 2 bytes and the others 8, since elfq leaves out the
/// padding after the first.
pub fn chain_of_calls(count: usize) -> ElfBytes {
    // bx lr, then three nops as padding.
    let mut code: Vec<u16> = vec![0x4770, 0xbf00, 0xbf00, 0xbf00];
    for _ in 1..count {
        // push {lr}; bl to the function before; pop {pc}
        code.extend([0xb500, 0xf7ff, 0xfff9, 0xbd00]);
    }
    let code: Vec<u8> = code.iter().flat_map(|half| half.to_le_bytes()).collect();

    let mut data = ElfBytes::sample("testdata/bin/c-cortex-m4-stripped.elf");
    let address = 0x0810_0000;
    // The last function, as Thumb code.
    let last = address + 8 * (count as u64 - 1);
    data.replace_section(".text", &code)
        .set_section(".text", SectionField::Address, address)
        .set(HeaderField::EntryPoint, last | 1);
    data
}
