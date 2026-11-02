//! Changing a sample's bytes, to make damaged and unusual files for tests.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use object::{Object, ObjectSection, ObjectSymbol};

use crate::support::{SAMPLE, sample_path};

/// Where a field is in a section header: in a 64-bit file, and in a
/// 32-bit one.
pub type Field = (usize, usize);

/// `sh_addr`, the section's address.
pub const ADDRESS: Field = (0x10, 0x0c);
/// `sh_offset`, where the section's bytes are in the file.
pub const OFFSET: Field = (0x18, 0x10);
/// `sh_size`, the section's size.
pub const SIZE: Field = (0x20, 0x14);

/// The sample at `path`, as bytes to change.
pub fn sample(path: &str) -> Vec<u8> {
    std::fs::read(sample_path(path)).unwrap()
}

/// Writes `value` over the `width` bytes at `at`. The samples are
/// little-endian.
pub fn write(data: &mut [u8], at: usize, width: usize, value: u64) {
    data[at..at + width].copy_from_slice(&value.to_le_bytes()[..width]);
}

/// Where the header of `section` is in the file, and whether the file is a
/// 64-bit one.
pub fn section_header(data: &[u8], section: &str) -> (usize, bool) {
    let file = object::File::parse(data).unwrap();
    let index = file.section_by_name(section).unwrap().index().0;
    let is_64 = file.is_64();
    let number = |at: usize, width: usize| {
        let mut bytes = [0; 8];
        bytes[..width].copy_from_slice(&data[at..at + width]);
        u64::from_le_bytes(bytes) as usize
    };
    // `e_shoff` and `e_shentsize` in the file's header, which say where the
    // section headers are.
    let (headers, header_size) = if is_64 {
        (number(0x28, 8), number(0x3a, 2))
    } else {
        (number(0x20, 4), number(0x2e, 2))
    };
    (headers + index * header_size, is_64)
}

/// Sets `field` in the header of `section` to `value`.
pub fn set_section_field(data: &mut [u8], section: &str, field: Field, value: u64) {
    let (header, is_64) = section_header(data, section);
    let (at, width) = if is_64 { (field.0, 8) } else { (field.1, 4) };
    write(data, header + at, width, value);
}

/// Sets the size that `.symtab` gives `symbol` to `size`.
pub fn set_symbol_size(data: &mut [u8], symbol: &str, size: u64) {
    let file = object::File::parse(&*data).unwrap();
    let index = file.symbol_by_name(symbol).unwrap().index().0;
    let (table, _) = file
        .section_by_name(".symtab")
        .unwrap()
        .file_range()
        .unwrap();
    // In a 64-bit file each symbol takes 24 bytes, and `st_size` is the
    // last 8; in a 32-bit one, 16 bytes, with `st_size` the 4 after
    // `st_name` and `st_value`.
    let (entry_size, at, width) = if file.is_64() {
        (24, 16, 8)
    } else {
        (16, 8, 4)
    };
    write(data, table as usize + index * entry_size + at, width, size);
}

/// Puts `bytes` at the end of the file and points `section` at them.
pub fn replace_section(data: &mut Vec<u8>, section: &str, bytes: &[u8]) {
    let offset = data.len() as u64;
    data.extend_from_slice(bytes);
    set_section_field(data, section, OFFSET, offset);
    set_section_field(data, section, SIZE, bytes.len() as u64);
}

/// The sample at `path`, with build attributes for Arm version `version`.
///
/// The attributes say only that its code is for that version of the Arm
/// architecture (Tag_CPU_arch), on a Cortex-M core; or, when `version` is
/// `None`, `.ARM.attributes` is empty, which says nothing.
pub fn for_arm_version(path: &str, version: Option<u8>) -> Vec<u8> {
    let mut data = sample(path);
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
    replace_section(&mut data, ".ARM.attributes", &section);
    data
}

/// Saves `data` as `name` in a folder for the tests' files, and returns its
/// path.
///
/// Tests run in parallel, and some save the same file, so it is written
/// under a name of its own first and then renamed: elfq never reads a file
/// that another test is still writing.
pub fn save(name: &str, data: &[u8]) -> String {
    static SAVED: AtomicUsize = AtomicUsize::new(0);
    let folder = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let partial = folder.join(format!(
        "{name}.{}.{}.part",
        std::process::id(),
        SAVED.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&partial, data).unwrap();
    let path = folder.join(name);
    std::fs::rename(&partial, &path).unwrap();
    path.to_str().unwrap().to_string()
}

/// The x86-64 sample, with the bytes of each of `sections` past the end
/// of the file.
pub fn past_the_end(sections: &[&str]) -> Vec<u8> {
    let mut data = sample(SAMPLE);
    for section in sections {
        set_section_field(&mut data, section, OFFSET, u64::MAX - 15);
    }
    data
}

/// Where the bytes of `section` are in the file.
pub fn file_range(data: &[u8], section: &str) -> (usize, usize) {
    let file = object::File::parse(data).unwrap();
    let (offset, size) = file.section_by_name(section).unwrap().file_range().unwrap();
    (offset as usize, size as usize)
}

/// The offset of each section header in a 32-bit file, with the section's
/// `sh_flags`.
pub fn section_headers_32(data: &[u8]) -> Vec<(usize, u32)> {
    let number = |at: usize, width: usize| {
        let mut bytes = [0; 4];
        bytes[..width].copy_from_slice(&data[at..at + width]);
        u32::from_le_bytes(bytes) as usize
    };
    // `e_shoff`, `e_shentsize` and `e_shnum`.
    let (headers, size, count) = (number(0x20, 4), number(0x2e, 2), number(0x30, 2));
    (0..count)
        .map(|index| headers + index * size)
        .map(|header| (header, number(header + 8, 4) as u32))
        .collect()
}

/// `SHF_ALLOC`, set in the flags of a section that the program loads.
pub const ALLOCATED: u32 = 0x2;

/// Stripped firmware whose code is `count` Thumb functions, in place of
/// `.text`, that each call the one before. It starts at the last. The first
/// function takes 2 bytes and the others 8, since elfq leaves out the
/// padding after the first.
pub fn chain_of_calls(count: usize) -> Vec<u8> {
    // bx lr, then three nops as padding.
    let mut code: Vec<u16> = vec![0x4770, 0xbf00, 0xbf00, 0xbf00];
    for _ in 1..count {
        // push {lr}; bl to the function before; pop {pc}
        code.extend([0xb500, 0xf7ff, 0xfff9, 0xbd00]);
    }
    let code: Vec<u8> = code.iter().flat_map(|half| half.to_le_bytes()).collect();

    let mut data = sample("testdata/bin/c-cortex-m4-stripped.elf");
    let address = 0x0810_0000;
    replace_section(&mut data, ".text", &code);
    set_section_field(&mut data, ".text", ADDRESS, address);
    // `e_entry`, in the file's header: the last function, as Thumb code.
    let last = address + 8 * (count as u64 - 1);
    write(&mut data, 0x18, 4, last | 1);
    data
}

/// The 4 bytes at `at`. The samples are little-endian.
pub fn word(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

pub fn set_word(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Where the vector table, the section `.isr_vector`, is in the file
/// `data`.
pub fn isr_vector(data: &[u8]) -> usize {
    let file = object::File::parse(data).unwrap();
    let (table, _) = (file.section_by_name(".isr_vector").unwrap())
        .file_range()
        .unwrap();
    table as usize
}

/// Where the program header of the load image for `address` is in the
/// 32-bit file `data`: of the segment there whose load address isn't its
/// address.
pub fn load_image_header(data: &[u8], address: u32) -> usize {
    let half = |at: usize| usize::from(u16::from_le_bytes([data[at], data[at + 1]]));
    // e_phoff, e_phentsize and e_phnum; and in a program header, p_vaddr
    // and p_paddr.
    let (headers, size, count) = (word(data, 0x1c) as usize, half(0x2a), half(0x2c));
    (0..count)
        .map(|n| headers + n * size)
        .find(|&at| word(data, at + 8) == address && word(data, at + 12) != address)
        .unwrap()
}
