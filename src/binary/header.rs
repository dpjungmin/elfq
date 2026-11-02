//! The few facts that a file's ELF header and program headers give, read
//! straight from the file before it is loaded, so that a command can
//! refuse a large program for another processor without loading it.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use object::elf::{
    DataEncoding, ELFCLASS32, ELFCLASS64, ELFDATA2LSB, ELFDATA2MSB, FileClass, FileType, Machine,
    PN_XNUM, PT_DYNAMIC, PT_INTERP, ProgramType,
};

/// What a file's ELF header and program headers say about it.
pub(crate) struct ElfHeader {
    /// `e_machine`: the processor the code is for, such as
    /// [`object::elf::EM_ARM`].
    pub(crate) machine: Machine,
    /// `e_type`: the kind of file, such as [`object::elf::ET_DYN`] for a
    /// shared library.
    pub(crate) file_type: FileType,
    /// Whether a program header names a program interpreter, as a program
    /// that an operating system loads has.
    pub(crate) has_interpreter: bool,
    /// Whether a program header gives a dynamic section.
    pub(crate) has_dynamic: bool,
    /// Whether the file has section headers.
    pub(crate) has_sections: bool,
}

impl ElfHeader {
    /// The headers of the ELF file at `path`, or `None` when it isn't one.
    /// Program headers that the file ends before are left out.
    pub(crate) fn read(path: &Path) -> io::Result<Option<ElfHeader>> {
        ElfHeader::read_from(&mut File::open(path)?)
    }

    fn read_from(file: &mut (impl Read + Seek)) -> io::Result<Option<ElfHeader>> {
        // The ELF header is 52 bytes long in a 32-bit file and 64 bytes
        // long in a 64-bit one.
        let mut header = [0; 64];
        let read = file.read(&mut header)?;
        if read < 52 || header[..4] != *b"\x7fELF" {
            return Ok(None);
        }
        let wide = match FileClass(header[4]) {
            ELFCLASS32 => false,
            ELFCLASS64 => true,
            _ => return Ok(None),
        };
        let little = match DataEncoding(header[5]) {
            ELFDATA2LSB => true,
            ELFDATA2MSB => false,
            _ => return Ok(None),
        };
        if wide && read < 64 {
            return Ok(None);
        }
        let number = |field: &[u8]| -> u64 {
            let mut value = 0;
            for (n, &byte) in field.iter().enumerate() {
                let shift = if little { n } else { field.len() - 1 - n };
                value |= u64::from(byte) << (8 * shift);
            }
            value
        };
        let half = |at: usize| number(&header[at..at + 2]) as u16;
        // `e_phoff` and `e_shoff`, which are twice as long in a 64-bit
        // file, and where `e_phentsize` starts, after them and `e_flags`
        // and `e_ehsize`.
        let (program_headers_at, section_headers_at, sizes_at) = if wide {
            (number(&header[32..40]), number(&header[40..48]), 54)
        } else {
            (number(&header[28..32]), number(&header[32..36]), 42)
        };
        let program_header_size = half(sizes_at);
        let program_header_count = half(sizes_at + 2);
        let section_header_size = half(sizes_at + 4);
        let section_header_count = half(sizes_at + 6);
        let mut elf = ElfHeader {
            machine: Machine(half(18)),
            file_type: FileType(half(16)),
            has_interpreter: false,
            has_dynamic: false,
            has_sections: section_headers_at != 0,
        };
        let mut word = [0; 8];
        // With more sections than `e_shnum` holds, it is 0, and the first
        // section header's `sh_size` gives the number.
        if section_header_count == 0 && elf.has_sections {
            let size = if wide { 8 } else { 4 };
            let at = section_headers_at.checked_add(if wide { 32 } else { 20 });
            elf.has_sections = section_header_size > 0
                && at.is_some_and(|at| file.seek(SeekFrom::Start(at)).is_ok())
                && file.read_exact(&mut word[..size]).is_ok()
                && number(&word[..size]) != 0;
        }
        // With more program headers than `e_phnum` holds, it is
        // `PN_XNUM`, and they are left unread.
        if program_header_count != PN_XNUM {
            for n in 0..u64::from(program_header_count) {
                let Some(at) = program_headers_at.checked_add(n * u64::from(program_header_size))
                else {
                    break;
                };
                if file.seek(SeekFrom::Start(at)).is_err()
                    || file.read_exact(&mut word[..4]).is_err()
                {
                    break;
                }
                match ProgramType(number(&word[..4]) as u32) {
                    PT_INTERP => elf.has_interpreter = true,
                    PT_DYNAMIC => elf.has_dynamic = true,
                    _ => {}
                }
            }
        }
        Ok(Some(elf))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use object::elf::{EM_ARM, EM_X86_64, ET_DYN, ET_EXEC, PT_LOAD};

    use super::*;

    /// The bytes of an ELF file with only the fields that
    /// [`ElfHeader::read`] reads filled in.
    struct TestFile {
        wide: bool,
        little: bool,
        bytes: Vec<u8>,
    }

    impl TestFile {
        /// An ELF header with no program or section headers.
        fn new(wide: bool, little: bool, machine: Machine, file_type: FileType) -> TestFile {
            let mut file = TestFile {
                wide,
                little,
                bytes: vec![0; if wide { 64 } else { 52 }],
            };
            file.bytes[..4].copy_from_slice(b"\x7fELF");
            file.bytes[4] = if wide { ELFCLASS64.0 } else { ELFCLASS32.0 };
            file.bytes[5] = if little { ELFDATA2LSB.0 } else { ELFDATA2MSB.0 };
            file.put(16, 2, u64::from(file_type.0));
            file.put(18, 2, u64::from(machine.0));
            file
        }

        /// Puts `value` in the `len` bytes at `at`, in the file's byte
        /// order, making the file longer if it has to.
        fn put(&mut self, at: usize, len: usize, value: u64) {
            if self.bytes.len() < at + len {
                self.bytes.resize(at + len, 0);
            }
            for n in 0..len {
                let shift = if self.little { n } else { len - 1 - n };
                self.bytes[at + n] = (value >> (8 * shift)) as u8;
            }
        }

        /// Where `e_phentsize` is.
        fn sizes_at(&self) -> usize {
            if self.wide { 54 } else { 42 }
        }

        /// Program headers of `types` at the end of the file, with
        /// `count` in `e_phnum`.
        fn program_headers(mut self, types: &[ProgramType], count: u16) -> TestFile {
            let at = self.bytes.len();
            let size = if self.wide { 56 } else { 32 };
            let (offset_at, offset_len) = if self.wide { (32, 8) } else { (28, 4) };
            self.put(offset_at, offset_len, at as u64);
            self.put(self.sizes_at(), 2, size as u64);
            self.put(self.sizes_at() + 2, 2, u64::from(count));
            for (n, &kind) in types.iter().enumerate() {
                self.put(at + n * size, 4, u64::from(kind.0));
                self.put(at + n * size + size - 1, 1, 0);
            }
            self
        }

        /// A first section header at the end of the file whose `sh_size`
        /// is `first_size`, with `count` in `e_shnum`.
        fn section_headers(mut self, count: u16, first_size: u64) -> TestFile {
            let at = self.bytes.len();
            let size = if self.wide { 64 } else { 40 };
            let (offset_at, offset_len) = if self.wide { (40, 8) } else { (32, 4) };
            self.put(offset_at, offset_len, at as u64);
            self.put(self.sizes_at() + 4, 2, size as u64);
            self.put(self.sizes_at() + 6, 2, u64::from(count));
            let (size_at, size_len) = if self.wide { (32, 8) } else { (20, 4) };
            self.put(at + size_at, size_len, first_size);
            self.put(at + size - 1, 1, 0);
            self
        }

        fn read(&self) -> Option<ElfHeader> {
            ElfHeader::read_from(&mut Cursor::new(&self.bytes)).unwrap()
        }
    }

    /// Each of 32- and 64-bit, little- and big-endian.
    const SHAPES: [(bool, bool); 4] = [(false, true), (false, false), (true, true), (true, false)];

    #[test]
    fn the_machine_and_the_kind_of_file_are_read_in_every_shape() {
        for (wide, little) in SHAPES {
            let header = TestFile::new(wide, little, EM_ARM, ET_EXEC).read().unwrap();
            assert_eq!((header.machine, header.file_type), (EM_ARM, ET_EXEC));
            assert!(!header.has_interpreter && !header.has_dynamic && !header.has_sections);
        }
    }

    #[test]
    fn program_headers_say_whether_an_operating_system_loads_the_program() {
        for (wide, little) in SHAPES {
            let file = TestFile::new(wide, little, EM_X86_64, ET_DYN)
                .program_headers(&[PT_LOAD, PT_INTERP, PT_DYNAMIC], 3);
            let header = file.read().unwrap();
            assert!(header.has_interpreter && header.has_dynamic);
            let file = TestFile::new(wide, little, EM_X86_64, ET_DYN)
                .program_headers(&[PT_LOAD, PT_DYNAMIC], 2);
            let header = file.read().unwrap();
            assert!(!header.has_interpreter && header.has_dynamic);
        }
    }

    #[test]
    fn program_headers_that_the_file_ends_before_are_left_out() {
        for (wide, little) in SHAPES {
            // Three program headers, of which the file holds only one.
            let file =
                TestFile::new(wide, little, EM_ARM, ET_EXEC).program_headers(&[PT_INTERP], 3);
            let header = file.read().unwrap();
            assert!(header.has_interpreter && !header.has_dynamic);
        }
    }

    #[test]
    fn program_headers_too_many_for_e_phnum_are_left_unread() {
        for (wide, little) in SHAPES {
            let file =
                TestFile::new(wide, little, EM_ARM, ET_EXEC).program_headers(&[PT_INTERP], PN_XNUM);
            assert!(!file.read().unwrap().has_interpreter);
        }
    }

    #[test]
    fn with_e_shnum_0_the_first_section_header_says_whether_there_are_sections() {
        for (wide, little) in SHAPES {
            let header = |count, first_size| {
                let file = TestFile::new(wide, little, EM_ARM, ET_EXEC);
                file.section_headers(count, first_size).read().unwrap()
            };
            assert!(header(12, 0).has_sections);
            assert!(header(0, 70_000).has_sections);
            assert!(!header(0, 0).has_sections);
        }
    }

    #[test]
    fn a_section_header_past_the_end_of_the_file_is_none() {
        for (wide, little) in SHAPES {
            let mut file = TestFile::new(wide, little, EM_ARM, ET_EXEC).section_headers(0, 9);
            let (offset_at, offset_len) = if wide { (40, 8) } else { (32, 4) };
            file.put(offset_at, offset_len, 0x10_0000);
            assert!(!file.read().unwrap().has_sections);
        }
    }

    #[test]
    fn a_file_that_isnt_elf_or_is_too_short_has_no_header() {
        let mut file = TestFile::new(true, true, EM_ARM, ET_EXEC);
        file.bytes[0] = b'#';
        assert!(file.read().is_none());
        let mut file = TestFile::new(true, true, EM_ARM, ET_EXEC);
        file.bytes[4] = 3;
        assert!(file.read().is_none());
        let mut file = TestFile::new(false, true, EM_ARM, ET_EXEC);
        file.bytes[5] = 0;
        assert!(file.read().is_none());
        // A 64-bit header cut to the length of a 32-bit one.
        let mut file = TestFile::new(true, true, EM_ARM, ET_EXEC);
        file.bytes.truncate(52);
        assert!(file.read().is_none());
        let mut file = TestFile::new(false, true, EM_ARM, ET_EXEC);
        file.bytes.truncate(51);
        assert!(file.read().is_none());
    }
}
