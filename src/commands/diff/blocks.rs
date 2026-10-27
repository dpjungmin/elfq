//! The blocks of memory that `diff` compares: the overview's blocks, worked
//! out once over both builds' sections and load images together, so that
//! both builds share each block's edges, and what changed in each block.

use std::collections::{HashMap, VecDeque};

use object::SectionKind;

use crate::binary::{AddressRange, LoadedMemory, memory_blocks, same_bytes_in};

/// A block of memory that the two builds share, such as an ITCM or flash.
#[derive(Debug)]
pub(super) struct Block {
    /// From the start of the first section or load image in it, in either
    /// build, to the furthest end of any of them, gaps included.
    pub range: AddressRange,
    /// The old build's sections and load images in the block, in address
    /// order.
    pub old: Vec<Place>,
    /// The new build's.
    pub new: Vec<Place>,
    /// Whether both builds put the same bytes at the same addresses in the
    /// block.
    pub same_bytes: bool,
}

/// Where a section, or its load image, is in a block.
#[derive(Debug, Clone, Copy)]
pub(super) struct Place {
    /// The section, as an index into its build's sections.
    pub section: usize,
    /// Whether this is the section's load image, where the file stores the
    /// section's bytes for the startup code to copy, rather than the
    /// section itself.
    pub load_image: bool,
    pub address: u64,
    pub size: u64,
}

/// A section or load image in a block, with where it is in each build: the
/// first of a name in one build goes with the first of that name in the
/// other, and so on.
#[derive(Debug)]
pub(super) struct SectionChange<'a> {
    pub name: &'a str,
    /// Whether these are the section's load images.
    pub load_image: bool,
    /// Where it is in the old build, or `None` when it isn't in the block
    /// there.
    pub old: Option<Place>,
    /// Where it is in the new build.
    pub new: Option<Place>,
}

/// The blocks of memory that the sections in memory and the load images of
/// both builds occupy, in address order: [`memory_blocks`] over the
/// sections of both. `.tbss` isn't in any, since it isn't in memory at its
/// addresses. When the builds hold the same memory, `same_memory` says so,
/// and no block's bytes are compared again.
pub(super) fn shared_blocks(
    old: &LoadedMemory,
    new: &LoadedMemory,
    same_memory: bool,
) -> Vec<Block> {
    // Both builds' sections in one list, and for each, whether it is the
    // new build's and its index in that build's sections.
    let mut sections = Vec::new();
    let mut origins = Vec::new();
    for (is_new, memory) in [(false, old), (true, new)] {
        for index in memory.sections_in_memory() {
            sections.push(memory.sections[index].clone());
            origins.push((is_new, index));
        }
    }
    let blocks = memory_blocks(&sections);
    let same_bytes = if same_memory {
        vec![true; blocks.len()]
    } else {
        let ranges: Vec<AddressRange> = blocks.iter().map(|block| block.range).collect();
        same_bytes_in(old, new, &ranges)
    };
    blocks
        .into_iter()
        .zip(same_bytes)
        .map(|(block, same_bytes)| {
            // Where each section in the block is, and each load image.
            let section_places = block
                .sections
                .iter()
                .map(|&index| (index, false, sections[index].address));
            let load_image_places = block
                .load_images
                .iter()
                .filter_map(|&index| Some((index, true, sections[index].load_address?)));
            let (mut old_places, mut new_places) = (Vec::new(), Vec::new());
            for (index, load_image, address) in section_places.chain(load_image_places) {
                let (is_new, section) = origins[index];
                let place = Place {
                    section,
                    load_image,
                    address,
                    size: sections[index].size,
                };
                if is_new {
                    new_places.push(place);
                } else {
                    old_places.push(place);
                }
            }
            for places in [&mut old_places, &mut new_places] {
                places.sort_by_key(|place| (place.address, place.load_image));
            }
            Block {
                range: block.range,
                old: old_places,
                new: new_places,
                same_bytes,
            }
        })
        .collect()
}

impl Block {
    /// The bytes from the old build's first section or load image in the
    /// block to its furthest end, gaps included, as the overview counts a
    /// block, or `None` when the old build has none there.
    pub fn old_size(&self) -> Option<u64> {
        span(&self.old)
    }

    /// The same for the new build.
    pub fn new_size(&self) -> Option<u64> {
        span(&self.new)
    }

    /// How many bytes the block grew by, from the old build to the new:
    /// less than 0 when it shrank. A build that has nothing in the block
    /// counts as 0.
    pub fn change(&self) -> i128 {
        let size = |size: Option<u64>| i128::from(size.unwrap_or(0));
        size(self.new_size()) - size(self.old_size())
    }

    /// The block's sections and load images, each with where it is in
    /// each build, in address order.
    pub fn sections<'a>(
        &self,
        old: &'a LoadedMemory,
        new: &'a LoadedMemory,
    ) -> Vec<SectionChange<'a>> {
        let name =
            |memory: &'a LoadedMemory, place: &Place| memory.sections[place.section].name.as_str();
        let mut changes: Vec<SectionChange> = self
            .old
            .iter()
            .map(|place| SectionChange {
                name: name(old, place),
                load_image: place.load_image,
                old: Some(*place),
                new: None,
            })
            .collect();
        // The old build's of each name, first first, for the new build's
        // to pair with.
        let mut unpaired: HashMap<(&str, bool), VecDeque<usize>> = HashMap::new();
        for (index, change) in changes.iter().enumerate() {
            unpaired
                .entry((change.name, change.load_image))
                .or_default()
                .push_back(index);
        }
        for place in &self.new {
            let name = name(new, place);
            let paired = unpaired
                .get_mut(&(name, place.load_image))
                .and_then(VecDeque::pop_front);
            match paired {
                Some(index) => changes[index].new = Some(*place),
                None => changes.push(SectionChange {
                    name,
                    load_image: place.load_image,
                    old: None,
                    new: Some(*place),
                }),
            }
        }
        changes.sort_by_key(|change| {
            [change.old, change.new]
                .into_iter()
                .flatten()
                .map(|place| place.address)
                .min()
        });
        changes
    }
}

impl SectionChange<'_> {
    /// How many bytes it grew by: less than 0 when it shrank, and all of
    /// its bytes when only one build has it.
    pub fn change(&self) -> i128 {
        let size = |place: Option<Place>| i128::from(place.map_or(0, |place| place.size));
        size(self.new) - size(self.old)
    }
}

/// The bytes from the first of `places` to the furthest end of any, or
/// `None` when there are none.
fn span(places: &[Place]) -> Option<u64> {
    let start = places.iter().map(|place| place.address).min()?;
    let end = places
        .iter()
        .map(|place| place.address.saturating_add(place.size))
        .max()?;
    Some(end - start)
}

/// The names of the allocated sections whose address, size, load image or
/// bytes differ between the builds, or that only one has, in address order,
/// by the lower of a section's two addresses, as in a block. As in a block,
/// the first of a name in one build goes with the first in the other.
/// `.tbss` counts by its address and size, and sections of no size don't
/// count, as they hold no memory.
pub(super) fn sections_that_differ<'a>(
    old: &'a LoadedMemory,
    new: &'a LoadedMemory,
) -> Vec<&'a str> {
    let allocated = |memory: &'a LoadedMemory| {
        (0..memory.sections.len())
            .filter(|&index| memory.sections[index].allocated && memory.sections[index].size > 0)
    };
    let thread_local = |kind: SectionKind| kind == SectionKind::UninitializedTls;
    // The old build's sections of each name, first first, for the new
    // build's to pair with.
    let mut unpaired: HashMap<&str, VecDeque<usize>> = HashMap::new();
    for index in allocated(old) {
        unpaired
            .entry(old.sections[index].name.as_str())
            .or_default()
            .push_back(index);
    }
    // Each section that differs, with the address it is listed by.
    let mut differ: Vec<(u64, &str)> = Vec::new();
    for new_index in allocated(new) {
        let section = &new.sections[new_index];
        let paired = unpaired
            .get_mut(section.name.as_str())
            .and_then(VecDeque::pop_front);
        let Some(old_index) = paired else {
            differ.push((section.address, section.name.as_str()));
            continue;
        };
        let old_section = &old.sections[old_index];
        let same = old_section.address == section.address
            && old_section.size == section.size
            && old_section.load_address == section.load_address
            && thread_local(old_section.kind) == thread_local(section.kind)
            && old.bytes(old_index).same_as(new.bytes(new_index));
        if !same {
            differ.push((
                old_section.address.min(section.address),
                section.name.as_str(),
            ));
        }
    }
    // Those that only the old build has, in the order of its sections.
    let mut gone: Vec<usize> = unpaired.into_values().flatten().collect();
    gone.sort_unstable();
    for index in gone {
        let section = &old.sections[index];
        differ.push((section.address, section.name.as_str()));
    }
    differ.sort_by_key(|&(address, _)| address);
    differ.into_iter().map(|(_, name)| name).collect()
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

    /// Each block's start and end, and the names of what each build has
    /// in it, load images marked.
    fn describe(memory: &LoadedMemory, places: &[Place]) -> Vec<String> {
        places
            .iter()
            .map(|place| {
                let name = &memory.sections[place.section].name;
                match place.load_image {
                    true => format!("load image of {name}"),
                    false => name.clone(),
                }
            })
            .collect()
    }

    #[test]
    fn both_builds_share_the_blocks() {
        // .data and .bss went from DTCM at 0x20000000 to 0x80000000.
        let (old, new) = (sample("c-cortex-m7.elf"), sample("c-cortex-m7-sdram.elf"));
        let old = LoadedMemory::read(&old).unwrap();
        let new = LoadedMemory::read(&new).unwrap();
        let blocks = shared_blocks(&old, &new, false);
        let starts: Vec<u64> = blocks.iter().map(|block| block.range.start).collect();
        assert_eq!(
            starts,
            [0x0, 0x2000_0000, 0x2024_0000, 0x3000_0000, 0x8000_0000]
        );
        let dtcm = &blocks[1];
        assert_eq!(describe(&old, &dtcm.old), [".data", ".bss"]);
        assert!(dtcm.new.is_empty());
        assert_eq!(
            (dtcm.old_size(), dtcm.new_size(), dtcm.change()),
            (Some(1104), None, -1104)
        );
        let sdram = &blocks[4];
        assert!(sdram.old.is_empty());
        assert_eq!(describe(&new, &sdram.new), [".data", ".bss"]);
        assert_eq!(sdram.change(), 1104);
        // The code and flash hold DTCM's addresses; OCRAM is the same.
        let same: Vec<bool> = blocks.iter().map(|block| block.same_bytes).collect();
        assert_eq!(same, [false, false, true, false, false]);
        assert_eq!(
            describe(&new, &blocks[3].new),
            [
                ".isr_vector",
                "load image of .itcm",
                ".text",
                "load image of .data",
                ".rodata",
                ".ARM.exidx"
            ]
        );
    }

    #[test]
    fn sections_pair_by_name_in_each_block() {
        let (old, new) = (sample("c-cortex-m7.elf"), sample("c-cortex-m7-moved.elf"));
        let old = LoadedMemory::read(&old).unwrap();
        let new = LoadedMemory::read(&new).unwrap();
        let blocks = shared_blocks(&old, &new, false);
        let flash = &blocks[3];
        let changes: Vec<(&str, bool, i128)> = flash
            .sections(&old, &new)
            .iter()
            .map(|section| (section.name, section.load_image, section.change()))
            .collect();
        assert_eq!(
            changes,
            [
                (".isr_vector", false, 0),
                (".itcm", true, -16),
                (".text", false, 0),
                (".data", true, 0),
                (".rodata", false, 16),
                (".ARM.exidx", false, 0),
            ]
        );
        assert_eq!(
            (flash.old_size(), flash.new_size()),
            (Some(3804), Some(3820))
        );
        // .rodata grew, so .ARM.exidx is 16 bytes on.
        let exidx = &flash.sections(&old, &new)[5];
        assert_eq!(
            (exidx.old.unwrap().address, exidx.new.unwrap().address),
            (0x3000_0ed4, 0x3000_0ee4)
        );
        // When the memory is the same, the blocks say so without comparing.
        let same = shared_blocks(&old, &old, true);
        assert!(
            same.iter()
                .all(|block| block.same_bytes && block.change() == 0)
        );
    }

    #[test]
    fn sections_differ_by_address_size_or_bytes() {
        let (old, new) = (sample("c-x86_64.elf"), sample("c-x86_64-moved.elf"));
        let old = LoadedMemory::read(&old).unwrap();
        let new = LoadedMemory::read(&new).unwrap();
        assert_eq!(
            sections_that_differ(&old, &new),
            [
                ".note.gnu.build-id",
                ".text",
                ".fini",
                ".rodata",
                ".eh_frame_hdr",
                ".eh_frame",
                ".dynamic"
            ]
        );
        // A section that only one build has counts, where it was.
        let (old, new) = (sample("c-x86_64.elf"), sample("c-x86_64.so"));
        let old = LoadedMemory::read(&old).unwrap();
        let new = LoadedMemory::read(&new).unwrap();
        let differ = sections_that_differ(&old, &new);
        assert_eq!(
            differ[..5],
            [
                ".note.gnu.property",
                ".note.gnu.build-id",
                ".gnu.hash",
                ".interp",
                ".dynsym"
            ]
        );
        assert!(sections_that_differ(&old, &old).is_empty());
    }
}
