//! What a file puts in memory when the program runs, read from its headers
//! alone, without looking for items: `diff` compares two builds by it, and
//! finds them the same when both put the same bytes at the same addresses.
//!
//! The loaded memory is every allocated section at its address, with a
//! section such as `.bss`, which the file doesn't store, as zeros, and every
//! load image at its load address (see [`Section::load_address`]). Symbols,
//! debug info and where the file keeps its bytes don't count, nor do the
//! bytes of the file's loadable segments outside its sections, such as the
//! ELF header. `.tbss` isn't in memory at its addresses (see
//! [`Section::in_memory`]), so its name, address and size are compared
//! apart.

use std::cmp::Ordering;

use anyhow::Result;
use object::{Architecture, Object, ObjectSection, SectionFlags, SectionKind};

use super::{AddressRange, Section, parse_program, read_sections};

/// A file's sections, and the bytes they put in memory.
#[derive(Debug)]
pub struct LoadedMemory<'data> {
    /// The instruction set, such as x86-64 or 32-bit Arm.
    pub architecture: Architecture,
    /// The file's sections, indexed by section number, as
    /// [`Binary::sections`](super::Binary::sections) holds them. Empty when
    /// the file has no section headers.
    pub sections: Vec<Section>,
    /// Problems with the headers that did not stop reading them, as
    /// [`Binary::warnings`](super::Binary::warnings) has them.
    pub warnings: Vec<String>,
    /// Whether the file stores the bytes of the code it loads. A separate
    /// debug file, as `objcopy --only-keep-debug` makes, doesn't: it keeps
    /// the headers of the sections that hold code, but none of their bytes,
    /// so what it puts in memory says nothing of the program. True for a
    /// file with no code.
    pub stores_code: bool,
    /// The whole file.
    data: &'data [u8],
}

/// The bytes that a section, or its load image, puts in memory.
#[derive(Debug, Clone, Copy)]
pub enum Bytes<'data> {
    /// The bytes that the file stores, as far as it goes.
    Stored(&'data [u8]),
    /// So many zero bytes, for a section such as `.bss`, which the file
    /// doesn't store.
    Zeros(u64),
    /// So many bytes that a damaged header puts past the end of the file,
    /// which are the same only as bytes missing in the same place.
    Missing(u64),
}

/// Bytes that start at an address.
#[derive(Debug, Clone, Copy)]
struct Piece<'data> {
    address: u64,
    bytes: Bytes<'data>,
}

/// Pieces that follow on from each other, from `address` to `end`.
#[derive(Debug)]
struct Run<'data> {
    address: u64,
    bytes: Vec<Bytes<'data>>,
    end: u64,
}

impl<'data> LoadedMemory<'data> {
    /// Reads the headers of the ELF file in `data`, which must hold a linked
    /// program or library.
    pub fn read(data: &'data [u8]) -> Result<LoadedMemory<'data>> {
        let (file, _) = parse_program(data)?;
        let mut warnings = Vec::new();
        let sections = read_sections(&file, data.len() as u64, &mut warnings);
        let mut code = file
            .sections()
            .filter(|section| match section.flags() {
                SectionFlags::Elf { sh_flags, .. } => {
                    sh_flags.contains(object::elf::SHF_ALLOC)
                        && sh_flags.contains(object::elf::SHF_EXECINSTR)
                }
                _ => false,
            })
            .peekable();
        let stores_code =
            code.peek().is_none() || code.any(|section| section.file_range().is_some());
        Ok(LoadedMemory {
            architecture: file.architecture(),
            sections,
            warnings,
            stores_code,
            data,
        })
    }

    /// The indexes of the sections that are in memory at their addresses
    /// and hold bytes there: all the allocated ones but `.tbss`, and those of
    /// no size.
    pub fn sections_in_memory(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.sections.len()).filter(|&index| {
            let section = &self.sections[index];
            section.in_memory() && section.size > 0
        })
    }

    /// The bytes that the section at `index` puts in memory: those the
    /// file stores for it, as far as the file goes, or zeros.
    pub fn bytes(&self, index: usize) -> Bytes<'data> {
        let section = &self.sections[index];
        let Some((offset, _)) = section.file_range else {
            return Bytes::Zeros(section.size);
        };
        // A damaged header can put the bytes past the end of the file,
        // which `read_sections` warns about.
        let start = usize::try_from(offset).map_or(self.data.len(), |o| o.min(self.data.len()));
        let rest = &self.data[start..];
        let size = usize::try_from(section.size).unwrap_or(usize::MAX);
        Bytes::Stored(&rest[..rest.len().min(size)])
    }

    /// The bytes that the file puts in memory, from each section in memory
    /// and each load image, sorted by address, then by size and by the
    /// bytes, so that the order of the sections doesn't count.
    fn pieces(&self) -> Vec<Piece<'data>> {
        let mut pieces = Vec::new();
        for index in self.sections_in_memory() {
            let section = &self.sections[index];
            let bytes = self.bytes(index);
            // What the file doesn't hold of a section that runs past its
            // end.
            let missing = section.size - bytes.len();
            for address in [Some(section.address), section.load_address]
                .into_iter()
                .flatten()
            {
                pieces.push(Piece { address, bytes });
                pieces.push(Piece {
                    address: address.saturating_add(bytes.len()),
                    bytes: Bytes::Missing(missing),
                });
            }
        }
        pieces.retain(|piece| !piece.bytes.is_empty());
        pieces.sort_by(|a, b| {
            a.address
                .cmp(&b.address)
                .then(a.bytes.len().cmp(&b.bytes.len()))
                .then_with(|| a.bytes.compare(b.bytes))
        });
        pieces
    }

    /// The sections that hold each thread's own zeroed variables, `.tbss`,
    /// which aren't in memory at their addresses: their names, addresses and
    /// sizes, sorted, but not those of no size, which hold nothing.
    fn thread_local(&self) -> Vec<(&str, u64, u64)> {
        let mut sections: Vec<(&str, u64, u64)> = self
            .sections
            .iter()
            .filter(|section| {
                section.allocated
                    && section.kind == SectionKind::UninitializedTls
                    && section.size > 0
            })
            .map(|section| (section.name.as_str(), section.address, section.size))
            .collect();
        sections.sort_unstable();
        sections
    }
}

/// Whether `old` and `new` put the same bytes at the same addresses, and
/// have the same `.tbss`. How the bytes are split into sections doesn't
/// count, only which byte is at which address.
pub fn same_memory(old: &LoadedMemory, new: &LoadedMemory) -> bool {
    same_runs(&runs(&old.pieces()), &runs(&new.pieces()))
        && old.thread_local() == new.thread_local()
}

/// For each of `ranges`, whether `old` and `new` put the same bytes at the
/// same addresses, from the sections and load images that start in it.
pub fn same_bytes_in(old: &LoadedMemory, new: &LoadedMemory, ranges: &[AddressRange]) -> Vec<bool> {
    // Each file's pieces are sorted once, and each range's found by address.
    let (old, new) = (old.pieces(), new.pieces());
    ranges
        .iter()
        .map(|&range| {
            same_runs(
                &runs(starting_in(&old, range)),
                &runs(starting_in(&new, range)),
            )
        })
        .collect()
}

/// Those of `pieces`, sorted by address, that start in `range`.
fn starting_in<'p, 'data>(pieces: &'p [Piece<'data>], range: AddressRange) -> &'p [Piece<'data>] {
    let start = pieces.partition_point(|piece| piece.address < range.start);
    let end = pieces.partition_point(|piece| piece.address < range.end);
    &pieces[start..end]
}

/// Whether two lists of runs start at the same addresses and hold the same
/// bytes.
fn same_runs(old: &[Run], new: &[Run]) -> bool {
    old.len() == new.len()
        && old
            .iter()
            .zip(new)
            .all(|(old, new)| old.address == new.address && same_bytes(&old.bytes, &new.bytes))
}

/// `pieces`, sorted by address, as runs: each piece that starts where the
/// one before ends goes on with its run. A piece that starts inside the run,
/// as where two sections overlap, adds the bytes it has past the run's end,
/// when the bytes they share are the same. When they aren't, as for
/// overlays, which the program loads at one address in turn, it starts a
/// run of its own.
fn runs<'data>(pieces: &[Piece<'data>]) -> Vec<Run<'data>> {
    let mut runs: Vec<Run> = Vec::new();
    for piece in pieces {
        let end = piece.address.saturating_add(piece.bytes.len());
        match runs.last_mut() {
            Some(run) if run.end == piece.address => {
                run.bytes.push(piece.bytes);
                run.end = end;
            }
            Some(run) if piece.address < run.end && run.agrees_with(piece) => {
                if end > run.end {
                    run.bytes.extend(piece.bytes.after(run.end - piece.address));
                    run.end = end;
                }
            }
            _ => runs.push(Run {
                address: piece.address,
                bytes: vec![piece.bytes],
                end,
            }),
        }
    }
    runs
}

impl Run<'_> {
    /// Whether the run holds the bytes that `piece`, which starts inside
    /// it, starts with, as far as the run goes.
    fn agrees_with(&self, piece: &Piece) -> bool {
        // The run's pieces from the piece's address on, found from the end,
        // where two sections overlap.
        let mut start = self.end;
        let mut first = self.bytes.len();
        while start > piece.address {
            first -= 1;
            start -= self.bytes[first].len();
        }
        let mut shared = self.bytes[first..].iter().copied();
        let head = shared
            .next()
            .and_then(|bytes| bytes.after(piece.address - start));
        let shared: Vec<Bytes> = head.into_iter().chain(shared).collect();
        same_start(&shared, &[piece.bytes])
    }
}

/// Whether two runs of pieces hold the same bytes, however they are split
/// into pieces.
fn same_bytes(old: &[Bytes], new: &[Bytes]) -> bool {
    let length = |pieces: &[Bytes]| pieces.iter().map(|bytes| bytes.len()).sum::<u64>();
    length(old) == length(new) && same_start(old, new)
}

/// Whether two runs of pieces start with the same bytes, as far as the
/// shorter goes, however they are split into pieces.
fn same_start(old: &[Bytes], new: &[Bytes]) -> bool {
    // Compare as much as both pieces at hand have left, then move on in
    // the one that ends, or in both.
    let (mut old, mut new) = (old.iter().copied(), new.iter().copied());
    let (mut old_piece, mut new_piece) = (old.next(), new.next());
    while let (Some(old_bytes), Some(new_bytes)) = (old_piece, new_piece) {
        let shared = old_bytes.len().min(new_bytes.len());
        if !old_bytes.start(shared).same_as(new_bytes.start(shared)) {
            return false;
        }
        old_piece = old_bytes.after(shared).or_else(|| old.next());
        new_piece = new_bytes.after(shared).or_else(|| new.next());
    }
    true
}

impl<'data> Bytes<'data> {
    /// How many bytes there are.
    pub fn len(self) -> u64 {
        match self {
            Bytes::Stored(bytes) => bytes.len() as u64,
            Bytes::Zeros(count) | Bytes::Missing(count) => count,
        }
    }

    /// Whether there are none.
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// Whether both hold the same bytes.
    pub fn same_as(self, other: Bytes) -> bool {
        self.compare(other) == Ordering::Equal
    }

    /// The first `count` bytes, which there must be.
    fn start(self, count: u64) -> Bytes<'data> {
        match self {
            Bytes::Stored(bytes) => Bytes::Stored(&bytes[..count as usize]),
            Bytes::Zeros(_) => Bytes::Zeros(count),
            Bytes::Missing(_) => Bytes::Missing(count),
        }
    }

    /// The bytes after the first `count`, or `None` when there are none.
    fn after(self, count: u64) -> Option<Bytes<'data>> {
        let rest = match self {
            Bytes::Stored(bytes) => Bytes::Stored(&bytes[count as usize..]),
            Bytes::Zeros(total) => Bytes::Zeros(total - count),
            Bytes::Missing(total) => Bytes::Missing(total - count),
        };
        (!rest.is_empty()).then_some(rest)
    }

    /// The order of two runs of bytes, byte by byte, and a shorter one
    /// first when it starts the longer: as `[u8]` sorts, without making
    /// the zeros. Missing bytes come after the others.
    fn compare(self, other: Bytes) -> Ordering {
        match (self, other) {
            (Bytes::Stored(a), Bytes::Stored(b)) => a.cmp(b),
            (Bytes::Zeros(a), Bytes::Zeros(b)) | (Bytes::Missing(a), Bytes::Missing(b)) => {
                a.cmp(&b)
            }
            (Bytes::Zeros(count), Bytes::Stored(bytes)) => compare_zeros(count, bytes),
            (Bytes::Stored(bytes), Bytes::Zeros(count)) => compare_zeros(count, bytes).reverse(),
            (Bytes::Missing(_), _) => Ordering::Greater,
            (_, Bytes::Missing(_)) => Ordering::Less,
        }
    }
}

/// The order of `count` zeros and `bytes`, as [`Bytes::compare`] gives it.
fn compare_zeros(count: u64, bytes: &[u8]) -> Ordering {
    let shared = usize::try_from(count).map_or(bytes.len(), |count| count.min(bytes.len()));
    if bytes[..shared].iter().any(|&byte| byte != 0) {
        Ordering::Less
    } else {
        count.cmp(&(bytes.len() as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/bin")
            .join(name);
        std::fs::read(path).unwrap()
    }

    #[test]
    fn zeros_are_bytes_of_zero() {
        let zeros = Bytes::Zeros(4);
        assert!(zeros.same_as(Bytes::Stored(&[0, 0, 0, 0])));
        assert!(!zeros.same_as(Bytes::Stored(&[0, 0, 1, 0])));
        assert!(!zeros.same_as(Bytes::Stored(&[0, 0, 0])));
        assert!(!zeros.same_as(Bytes::Zeros(5)));
        // They sort as the bytes would.
        assert_eq!(zeros.compare(Bytes::Stored(&[0, 1])), Ordering::Less);
        assert_eq!(zeros.compare(Bytes::Stored(&[0, 0])), Ordering::Greater);
        assert_eq!(Bytes::Stored(&[0, 0]).compare(zeros), Ordering::Less);
    }

    #[test]
    fn only_where_each_byte_is_counts() {
        let piece = |address, bytes: &'static [u8]| Piece {
            address,
            bytes: Bytes::Stored(bytes),
        };
        let same = |one: &[Piece], other: &[Piece]| same_runs(&runs(one), &runs(other));
        // The same bytes, split into sections in other places.
        let one = [
            piece(0x10, &[1, 2]),
            Piece {
                address: 0x12,
                bytes: Bytes::Zeros(2),
            },
            piece(0x20, &[3]),
        ];
        assert_eq!(runs(&one).len(), 2);
        let other = [
            piece(0x10, &[1, 2, 0]),
            piece(0x13, &[0]),
            piece(0x20, &[3]),
        ];
        assert!(same(&one, &other));
        // Sections that overlap, where they hold the same bytes.
        let overlapping = [
            piece(0x10, &[1, 2, 0]),
            piece(0x11, &[2, 0, 0]),
            piece(0x12, &[0]),
            piece(0x20, &[3]),
        ];
        assert!(same(&one, &overlapping));
        // A byte at another address is another run.
        assert!(!same(
            &one,
            &[piece(0x10, &[1, 2, 0, 0]), piece(0x21, &[3])]
        ));
        // Overlays, which hold other bytes at one address, aren't joined.
        let overlays = [piece(0x10, &[1, 2, 0, 0]), piece(0x10, &[1, 2, 0, 9])];
        assert_eq!(runs(&overlays).len(), 2);
        assert!(!same(&one[..2], &overlays));
    }

    #[test]
    fn bytes_past_the_end_of_the_file_are_the_same_only_as_bytes_missing_there() {
        let missing = Bytes::Missing(2);
        assert!(missing.same_as(Bytes::Missing(2)));
        assert!(!missing.same_as(Bytes::Zeros(2)));
        assert!(!Bytes::Stored(&[0, 0]).same_as(missing));
        // .data in c-x86_64.elf holds 64 bytes. Here its bytes start 32
        // before the end of the file, which ends with its first 32.
        let data = sample("c-x86_64.elf");
        let memory = LoadedMemory::read(&data).unwrap();
        let index = memory
            .sections
            .iter()
            .position(|section| section.name == ".data")
            .unwrap();
        let (offset, size) = memory.sections[index].file_range.unwrap();
        assert_eq!(size, 64);
        let mut longer = data.clone();
        longer.extend_from_slice(&data[offset as usize..][..32]);
        let mut cut = LoadedMemory::read(&longer).unwrap();
        cut.sections[index].file_range = Some((data.len() as u64, 64));
        assert!(!same_memory(&memory, &cut));
        assert!(same_memory(&cut, &cut));
        // Nor is it the same as a .data of 32 bytes.
        let mut short = LoadedMemory::read(&data).unwrap();
        short.sections[index].size = 32;
        assert!(!same_memory(&cut, &short));
    }

    #[test]
    fn a_stripped_file_holds_the_memory_of_the_file_it_came_from() {
        for (full, stripped) in [
            ("c-cortex-m7.elf", "c-cortex-m7-stripped.elf"),
            ("c-x86_64.elf", "c-x86_64-stripped.elf"),
            // Its .tbss shares its addresses with .fini_array, .init_array
            // and .data.rel.ro.
            ("rust-x86_64-nodebug.elf", "rust-x86_64-stripped.elf"),
        ] {
            let (full, stripped) = (sample(full), sample(stripped));
            let full = LoadedMemory::read(&full).unwrap();
            let stripped = LoadedMemory::read(&stripped).unwrap();
            assert!(same_memory(&full, &stripped));
        }
    }

    #[test]
    fn tbss_is_compared_apart() {
        let data = sample("rust-x86_64-nodebug.elf");
        let memory = LoadedMemory::read(&data).unwrap();
        let tbss = memory
            .sections
            .iter()
            .position(|section| section.name == ".tbss")
            .unwrap();
        // .tbss shares its first address with .fini_array, which is in
        // memory there.
        let address = memory.sections[tbss].address;
        let in_memory: Vec<&str> = memory
            .sections_in_memory()
            .map(|index| &memory.sections[index])
            .filter(|section| section.contains(address))
            .map(|section| section.name.as_str())
            .collect();
        assert_eq!(in_memory, [".fini_array"]);
        // Its size counts, though it puts no bytes in memory.
        let mut larger = LoadedMemory::read(&data).unwrap();
        larger.sections[tbss].size += 8;
        let everywhere = AddressRange {
            start: 0,
            end: u64::MAX,
        };
        assert_eq!(same_bytes_in(&memory, &larger, &[everywhere]), [true]);
        assert!(!same_memory(&memory, &larger));
        // A .tbss of no size holds nothing, wherever it is.
        let mut empty = LoadedMemory::read(&data).unwrap();
        empty.sections[tbss].size = 0;
        let mut moved = LoadedMemory::read(&data).unwrap();
        moved.sections[tbss].size = 0;
        moved.sections[tbss].address += 8;
        assert!(same_memory(&empty, &moved));
    }

    #[test]
    fn only_bytes_in_sections_count() {
        // The ELF header is in the first loadable segment of an x86-64
        // program, but in no section.
        let data = sample("c-x86_64.elf");
        let mut padded = data.clone();
        padded[9..16].fill(0xff);
        let memory = LoadedMemory::read(&data).unwrap();
        assert!(same_memory(&memory, &LoadedMemory::read(&padded).unwrap()));
        // A byte of code does count.
        let text = memory
            .sections
            .iter()
            .find(|section| section.name == ".text")
            .unwrap();
        let mut edited = data.clone();
        edited[text.file_range.unwrap().0 as usize] ^= 1;
        assert!(!same_memory(&memory, &LoadedMemory::read(&edited).unwrap()));
    }
}
