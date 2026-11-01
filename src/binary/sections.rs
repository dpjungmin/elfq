use anyhow::{Context, Result, bail};
use object::read::elf::{ElfFile, FileHeader, ProgramHeader, SectionHeader};
use object::{Object, ObjectKind, ObjectSection, SectionFlags, SectionIndex, SectionKind};

use super::{FileKind, Section};

/// Parses `data` as an ELF file that holds a linked program or library,
/// and says which kind.
pub(super) fn parse_program(data: &[u8]) -> Result<(object::File<'_>, FileKind)> {
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
pub(super) fn read_sections(
    file: &object::File,
    file_size: u64,
    warnings: &mut Vec<String>,
) -> Vec<Section> {
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
pub(super) fn fitting_size(address: u64, size: u64) -> Option<u64> {
    if size > 0 && address.checked_add(size - 1).is_none() {
        return None;
    }
    Some(size.min(u64::MAX - address))
}

pub(super) fn null_section() -> Section {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
