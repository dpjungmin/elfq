//! Which paired items changed while keeping their size and block: those
//! whose bytes differ other than where they say where something is, or
//! whose code uses different items in the new build than in the old.
//!
//! Only partners whose bytes differ are compared, as the program starts
//! with them: the file's bytes, with the addresses and imports that the
//! loader writes at relocations, since LLD leaves 0 in the file there.
//! Partners whose bytes are the same are the same, unless their code reads
//! another import from a global offset table entry. When something moves,
//! the bytes that say where it is change in every item that uses it,
//! though nothing else in those items does. So a byte that differs between
//! two partners is no edit when it is in one of these:
//!
//! - the distance from an instruction to what it calls, jumps to, loads or
//!   takes the address of outside the function, as
//!   `InstructionSet::operands` finds it;
//! - an aligned word of data, or of Thumb code, which keeps constants among
//!   its instructions, a number that Thumb's `movw` and `movt` put
//!   together, or an entry of an x86-64 jump table, which is a 32-bit
//!   distance from the table, whose values in the two builds correspond
//!   (see [`Edits::correspond`]).
//!
//! Then, where the distances in the partners' code don't lead to the same
//! offset in partners, the code must use the partners of what it used there,
//! in the same ways, through veneers and the PLT stubs of a library's own
//! functions, and the same imports. A use of an item that is new or gone,
//! which has a row of its own, doesn't count. What the words and numbers
//! point at isn't compared again as a use: they are compared already, and
//! the graph takes a pointer to the end of an array for a use of whatever
//! follows it.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

use object::{Architecture, Endianness};

use super::Build;
use super::pairing::{Pairing, is_text_byte};
use crate::arch::{InstructionSet, Operands};
use crate::binary::{Binary, Item, ItemKind, RelocationTarget, word};
use crate::graph::{Node, ReferenceKind, VeneerTarget, Veneers};

/// How many veneers a call is followed through, as a veneer can jump to
/// another.
const VENEERS_FOLLOWED: usize = 4;

/// How many bytes of text two pointers are compared at.
const TEXT_COMPARED: u64 = 64;

/// Two builds whose items are paired, to tell which partners are edited.
pub(super) struct Edits<'a> {
    old: &'a Build,
    new: &'a Build,
    pairing: &'a Pairing,
    old_veneers: Veneers<'a>,
    new_veneers: Veneers<'a>,
}

/// What an item uses, as the same in both builds: a new item, or an old
/// item's partner, or an import by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Used<'a> {
    Item(usize),
    Import(&'a str),
}

/// Where an address is: in which item, as an index into a build's items,
/// and how far into it.
#[derive(Debug, Clone, Copy)]
struct ItemOffset {
    item: usize,
    offset: u64,
}

/// What [`InstructionSet::operands`] finds in the code of partners, in
/// each build.
struct PartnerOperands {
    old: Operands,
    new: Operands,
}

/// What may be text at an address, for [`same_text`]: the name of the
/// section that holds it, and up to [`TEXT_COMPARED`] of the section's
/// bytes from there.
struct TextAt<'a> {
    section_name: &'a str,
    bytes: &'a [u8],
}

/// The bytes of an item that the program starts with: the file's, with
/// the addresses that the loader writes at relocations, and the imports
/// that it writes, by name and offset.
#[derive(PartialEq)]
struct Loaded<'a> {
    bytes: Cow<'a, [u8]>,
    imports: Vec<(u64, &'a str)>,
}

impl<'a> Edits<'a> {
    pub(super) fn new(old: &'a Build, new: &'a Build, pairing: &'a Pairing) -> Edits<'a> {
        Edits {
            old,
            new,
            pairing,
            old_veneers: Veneers::new(&old.binary, &old.graph),
            new_veneers: Veneers::new(&new.binary, &new.graph),
        }
    }

    /// Whether item `old_item` of the old build and its partner `new_item`,
    /// of the same size, are edited: their bytes differ other than where
    /// they say where something is, or their code uses different items in
    /// the new build than in the old.
    pub(super) fn edited(&self, old_item: usize, new_item: usize) -> bool {
        let (old_binary, new_binary) = (&self.old.binary, &self.new.binary);
        let (old, new) = (&old_binary.items[old_item], &new_binary.items[new_item]);
        let (Some(old_loaded), Some(new_loaded)) =
            (loaded(old_binary, old), loaded(new_binary, new))
        else {
            return false;
        };
        if old_loaded == new_loaded {
            // The same code can still read another import from a global
            // offset table entry, which the loader fills in.
            return imports(self.old, old_item) != imports(self.new, new_item);
        }
        if old_loaded.imports != new_loaded.imports {
            return true;
        }
        let code = old.kind == ItemKind::Function;
        // diff refuses builds for two architectures, and reads the new
        // build's code as it reads the old build's: as Thumb when the old
        // build's is, even where the new build doesn't say it is.
        let instruction_sets = old_binary.instruction_set().zip(InstructionSet::of(
            new_binary.architecture,
            old_binary.thumb,
            new_binary.endianness,
        ));
        let (old_bytes, new_bytes) = (&old_loaded.bytes, &new_loaded.bytes);
        let operands = instruction_sets.filter(|_| code).map(|(old_set, new_set)| {
            let old_operands = old_set.operands(old_bytes, old.address);
            let new_operands = new_set.operands(new_bytes, new.address);
            PartnerOperands {
                old: old_operands,
                new: new_operands,
            }
        });
        self.bytes_edited(old, old_bytes, new, new_bytes, operands.as_ref())
            || operands.is_some_and(|operands| {
                let offsets = self.distances_elsewhere(&operands);
                self.uses_edited(old_item, new_item, &offsets)
            })
    }

    /// Whether the partners' bytes, `old_bytes` of `old` and `new_bytes` of
    /// `new`, differ other than in the distances and numbers in their code
    /// that `operands` explains (see [`Edits::explained_by_operands`]), and
    /// in the words that hold addresses that correspond.
    fn bytes_edited(
        &self,
        old: &Item,
        old_bytes: &[u8],
        new: &Item,
        new_bytes: &[u8],
        operands: Option<&PartnerOperands>,
    ) -> bool {
        // The bits of each byte that hold a distance or an address that
        // corresponds.
        let mut explained = match operands {
            Some(operands) => self.explained_by_operands(operands, old_bytes.len()),
            None => vec![0; old_bytes.len()],
        };
        // Data, and Thumb code, which keeps constants and tables of
        // addresses among its instructions, hold addresses in words. An
        // x86-64 jump table holds 32-bit distances from the table instead.
        let old_binary = &self.old.binary;
        let code = old.kind == ItemKind::Function;
        if !code || old_binary.thumb {
            let pointer = old_binary.pointer_size as usize;
            let entries = !code && old_binary.architecture == Architecture::X86_64;
            for range in words(old.address, old_bytes.len(), pointer, entries) {
                let (old_word, new_word) = (&old_bytes[range.clone()], &new_bytes[range.clone()]);
                if !differs_unexplained(old_word, new_word, &explained[range.clone()]) {
                    continue;
                }
                let is_pointer = range.len() == pointer;
                let corresponds = (is_pointer && self.pointers_correspond(old_word, new_word))
                    || (entries
                        && self.entries_correspond(old.address, old_word, new.address, new_word));
                // Words don't overlap, so one whose values don't
                // correspond is an edit, whatever the others hold.
                if !corresponds {
                    return true;
                }
                explained[range].fill(0xff);
            }
        }
        differs_unexplained(old_bytes, new_bytes, &explained)
    }

    /// The bits of each of the partners' `len` bytes that `operands` says
    /// hold a distance in the code of both, or a number that two
    /// instructions put together whose values in the two builds
    /// correspond.
    fn explained_by_operands(&self, operands: &PartnerOperands, len: usize) -> Vec<u8> {
        let mut explained = vec![0u8; len];
        let distance_bits = (operands.old.distance_bits.iter()).zip(&operands.new.distance_bits);
        for (bits, (old_bits, new_bits)) in explained.iter_mut().zip(distance_bits) {
            *bits = old_bits & new_bits;
        }
        let new_numbers: HashMap<_, u64> = (operands.new.split.iter())
            .map(|number| (&number.bits, number.value))
            .collect();
        for old_number in &operands.old.split {
            if let Some(&new_value) = new_numbers.get(&old_number.bits)
                && new_value != old_number.value
                && self.correspond(old_number.value, new_value)
            {
                for &(offset, bits) in &old_number.bits {
                    explained[offset] |= bits;
                }
            }
        }
        explained
    }

    /// Whether `old_pointer` and `new_pointer`, the bytes of a pointer at
    /// the same offset in partners, hold addresses that correspond.
    fn pointers_correspond(&self, old_pointer: &[u8], new_pointer: &[u8]) -> bool {
        self.correspond(
            word(old_pointer, self.old.binary.endianness),
            word(new_pointer, self.new.binary.endianness),
        )
    }

    /// Whether the 32-bit entries in `old_word` and `new_word`, at the same
    /// offset in tables at `old_table` and `new_table` in each build, lead
    /// to addresses that correspond where they differ, each entry being a
    /// distance from its table, as in an x86-64 jump table.
    fn entries_correspond(
        &self,
        old_table: u64,
        old_word: &[u8],
        new_table: u64,
        new_word: &[u8],
    ) -> bool {
        let endianness = self.old.binary.endianness;
        let target = |table: u64, entry| {
            let distance = word(entry, endianness) as u32 as i32;
            table.wrapping_add_signed(distance.into())
        };
        (old_word.chunks(4).zip(new_word.chunks(4))).all(|(old_entry, new_entry)| {
            old_entry == new_entry
                || self.correspond(target(old_table, old_entry), target(new_table, new_entry))
        })
    }

    /// Whether two values that a pair of words or instructions hold are the
    /// same address in each build: (a) the same offset in partners (see
    /// [`Edits::same_offset_in_partners`]); (b) in items that only one build
    /// has, which have rows of their own (see [`Edits::in_unpaired_items`]);
    /// (c) at the same text (see [`same_text`]); or (d) the same start or
    /// end of a section (see [`same_edge`]).
    fn correspond(&self, old_address: u64, new_address: u64) -> bool {
        let (old_binary, new_binary) = (&self.old.binary, &self.new.binary);
        self.same_offset_in_partners(old_address, new_address)
            || self.in_unpaired_items(old_address, new_address)
            || same_text(old_binary, old_address, new_binary, new_address)
            || same_edge(old_binary, old_address, new_binary, new_address)
    }

    /// Case (a) of [`Edits::correspond`]: whether `old_address` and
    /// `new_address` point into partners at the same offset, a function's
    /// Thumb bit and all, or just past the end of partners, as a pointer to
    /// the end of an array does.
    fn same_offset_in_partners(&self, old_address: u64, new_address: u64) -> bool {
        let (old_binary, new_binary) = (&self.old.binary, &self.new.binary);
        let into = match (
            item_offset_of(old_binary, old_address),
            item_offset_of(new_binary, new_address),
        ) {
            (Some(old), Some(new)) => {
                self.partner(old.item) == Some(new.item) && old.offset == new.offset
            }
            _ => false,
        };
        let past_the_end = match (
            item_ending_at(old_binary, old_address),
            item_ending_at(new_binary, new_address),
        ) {
            (Some(old_item), Some(new_item)) => self.partner(old_item) == Some(new_item),
            _ => false,
        };
        into || past_the_end
    }

    /// Case (b) of [`Edits::correspond`]: whether `old_address` and
    /// `new_address` both point into items without a partner.
    fn in_unpaired_items(&self, old_address: u64, new_address: u64) -> bool {
        let (old_binary, new_binary) = (&self.old.binary, &self.new.binary);
        item_offset_of(old_binary, old_address).is_some_and(|old| self.partner(old.item).is_none())
            && item_offset_of(new_binary, new_address)
                .is_some_and(|new| self.pairing.new_to_old[new.item].is_none())
    }

    /// The partner of the old build's item `old_item`, as an index into the
    /// new build's items.
    fn partner(&self, old_item: usize) -> Option<usize> {
        self.pairing.old_to_new[old_item].map(|(partner, _)| partner)
    }

    /// The offsets of the instructions whose distances, in the partners'
    /// code, as `operands` gives them, don't lead to the same offset in
    /// partners, in order. Only case (a) of [`Edits::correspond`]
    /// counts here: a call to a veneer that only one build has, or a load
    /// from a global offset table, which no item holds, leads elsewhere,
    /// and what the code uses there tells whether it changed.
    fn distances_elsewhere(&self, operands: &PartnerOperands) -> Vec<usize> {
        let (old_operands, new_operands) = (&operands.old, &operands.new);
        let old_targets: HashMap<usize, u64> = old_operands.targets.iter().copied().collect();
        let new_targets: HashMap<usize, u64> = new_operands.targets.iter().copied().collect();
        let same_offset = |offset: &usize, old_target: &u64| {
            new_targets
                .get(offset)
                .is_some_and(|&new_target| self.same_offset_in_partners(*old_target, new_target))
        };
        let only_new = |offset: &usize| !old_targets.contains_key(offset);
        let new_offsets = new_operands.targets.iter().map(|&(offset, _)| offset);
        let mut offsets: Vec<usize> = (old_operands.targets.iter())
            .filter(|(offset, target)| !same_offset(offset, target))
            .map(|&(offset, _)| offset)
            .chain(new_offsets.filter(only_new))
            .collect();
        offsets.sort_unstable();
        offsets.dedup();
        offsets
    }

    /// Whether the partners' code, at the instructions at `offsets`, uses
    /// other items, or in other ways.
    fn uses_edited(&self, old_item: usize, new_item: usize, offsets: &[usize]) -> bool {
        if offsets.is_empty() {
            return false;
        }
        let old_uses = self.uses(self.old, &self.old_veneers, old_item, offsets, |index| {
            self.partner(index)
        });
        let new_uses = self.uses(self.new, &self.new_veneers, new_item, offsets, |index| {
            self.pairing.new_to_old[index].map(|_| index)
        });
        old_uses != new_uses
    }

    /// What the instructions of `item` of `build` at `offsets` into it use,
    /// each with how, through veneers, in order: of the items that both
    /// builds have, by `paired`, which gives the new build's index for one,
    /// and imports by name.
    fn uses(
        &self,
        build: &'a Build,
        veneers: &Veneers,
        item: usize,
        offsets: &[usize],
        paired: impl Fn(usize) -> Option<usize>,
    ) -> Vec<(Used<'a>, ReferenceKind)> {
        let binary = &build.binary;
        let address = binary.items[item].address;
        // Whether the instruction at the address `site` is at one of
        // `offsets`.
        let at_offsets = |site: u64| {
            usize::try_from(site.wrapping_sub(address))
                .is_ok_and(|offset| offsets.binary_search(&offset).is_ok())
        };
        let import = |index: usize| Used::Import(binary.imports[index].as_str());
        let mut uses: Vec<(Used, ReferenceKind)> = build
            .graph
            .references_from(item)
            .iter()
            .filter(|reference| at_offsets(reference.site))
            .filter_map(|reference| {
                let used = match reference.to {
                    Node::Item(index) if reference.kind.is_call() => {
                        match through(veneers, index) {
                            Node::Item(index) => Used::Item(paired(index)?),
                            Node::Import(index) => import(index),
                        }
                    }
                    Node::Item(index) => Used::Item(paired(index)?),
                    Node::Import(index) => import(index),
                };
                Some((used, reference.kind))
            })
            .collect();
        uses.sort_unstable();
        uses.dedup();
        uses
    }
}

/// The byte ranges of the words in `len` bytes at `address` that may hold
/// an address: a pointer of `pointer` bytes wherever one starts at a
/// multiple of its size and fits, and with `entries`, as in x86-64 data, a
/// 32-bit jump table entry, at a multiple of 4, wherever none does.
fn words(
    address: u64,
    len: usize,
    pointer: usize,
    entries: bool,
) -> impl Iterator<Item = Range<usize>> {
    let step = if entries { 4 } else { pointer };
    let mut offset = (step - (address % step as u64) as usize) % step;
    std::iter::from_fn(move || {
        if offset + step > len {
            return None;
        }
        // A pointer where one starts and fits, or else an entry.
        let starts = (address + offset as u64).is_multiple_of(pointer as u64);
        let size = if starts && offset + pointer <= len {
            pointer
        } else {
            step
        };
        let range = offset..offset + size;
        offset += size;
        Some(range)
    })
}

/// Whether `old_bytes` and `new_bytes` differ in a bit that `explained`
/// doesn't mark.
fn differs_unexplained(old_bytes: &[u8], new_bytes: &[u8], explained: &[u8]) -> bool {
    (old_bytes.iter())
        .zip(new_bytes)
        .zip(explained)
        .any(|((old_byte, new_byte), explained)| (old_byte ^ new_byte) & !explained != 0)
}

/// The imports that `item` of `build` uses itself, by name, each with how,
/// in order.
fn imports(build: &Build, item: usize) -> Vec<(&str, ReferenceKind)> {
    let mut imports: Vec<_> = build
        .graph
        .references_from(item)
        .iter()
        .filter_map(|reference| match reference.to {
            Node::Import(index) => Some((build.binary.imports[index].as_str(), reference.kind)),
            Node::Item(_) => None,
        })
        .collect();
    imports.sort_unstable();
    imports.dedup();
    imports
}

/// The bytes of `item` that the program starts with, or `None` when the
/// file holds none, as for `.bss`.
fn loaded<'a>(binary: &'a Binary, item: &Item) -> Option<Loaded<'a>> {
    let mut bytes = Cow::Borrowed(binary.item_bytes(item)?);
    let mut imports = Vec::new();
    for relocation in binary.relocations_in(item.address, item.size) {
        let offset = relocation.address - item.address;
        match relocation.target {
            RelocationTarget::Address(address) => {
                let start = offset as usize;
                let end = start + binary.pointer_size as usize;
                if let Some(slot) = bytes.to_mut().get_mut(start..end) {
                    put_word(slot, address, binary.endianness);
                }
            }
            RelocationTarget::Import(index) => {
                imports.push((offset, binary.imports[index].as_str()));
            }
        }
    }
    Some(Loaded { bytes, imports })
}

/// Writes `value` into `slot`, the bytes of a pointer.
fn put_word(slot: &mut [u8], value: u64, endianness: Endianness) {
    let size = slot.len();
    match endianness {
        Endianness::Little => slot.copy_from_slice(&value.to_le_bytes()[..size]),
        Endianness::Big => slot.copy_from_slice(&value.to_be_bytes()[8 - size..]),
    }
}

/// What a call to `item` reaches: the item, or what the veneer that it is
/// jumps to, through up to [`VENEERS_FOLLOWED`] veneers.
fn through(veneers: &Veneers, mut item: usize) -> Node {
    for _ in 0..VENEERS_FOLLOWED {
        match veneers.target(item) {
            Some(VeneerTarget::Node(Node::Item(target))) => item = target,
            Some(VeneerTarget::Node(import)) => return import,
            _ => break,
        }
    }
    Node::Item(item)
}

/// Where `value` points into an item of `binary`. On Thumb, a function's
/// address with the Thumb bit set points into the function, even when its
/// size is 0.
fn item_offset_of(binary: &Binary, value: u64) -> Option<ItemOffset> {
    let item = binary.item_at(value).or_else(|| {
        let index = binary.item_at(value.checked_sub(1)?)?;
        let thumb = binary.thumb && value % 2 == 1;
        (thumb && binary.items[index].kind == ItemKind::Function).then_some(index)
    })?;
    let offset = value - binary.items[item].address;
    Some(ItemOffset { item, offset })
}

/// The item of `binary` that ends just before `value`, as an index into its
/// items.
fn item_ending_at(binary: &Binary, value: u64) -> Option<usize> {
    let index = binary.item_at(value.checked_sub(1)?)?;
    let item = &binary.items[index];
    (item.address + item.size == value).then_some(index)
}

/// Case (c) of [`Edits::correspond`]: whether `old_address` and
/// `new_address` point into sections of the same name, at the same text: a
/// whole string that ends in a 0 byte, or at least 3 characters, as a Rust
/// string has no 0 byte.
fn same_text(old_binary: &Binary, old_address: u64, new_binary: &Binary, new_address: u64) -> bool {
    let (Some(old_text), Some(new_text)) = (
        text_at(old_binary, old_address),
        text_at(new_binary, new_address),
    ) else {
        return false;
    };
    if old_text.section_name != new_text.section_name {
        return false;
    }
    let (old_bytes, new_bytes) = (old_text.bytes, new_text.bytes);
    let same = (old_bytes.iter())
        .zip(new_bytes)
        .take_while(|&(old_byte, new_byte)| old_byte == new_byte && is_text_byte(*old_byte))
        .count();
    let whole = same > 0 && old_bytes.get(same) == Some(&0) && new_bytes.get(same) == Some(&0);
    whole || same >= 3
}

/// The bytes at `address` that [`same_text`] reads as text, with the name
/// of the section that holds them.
fn text_at(binary: &Binary, address: u64) -> Option<TextAt<'_>> {
    let section = &binary.sections[binary.section_at(address)?];
    let end = section.address + section.size;
    Some(TextAt {
        section_name: section.name.as_str(),
        bytes: binary.bytes(address, (end - address).min(TEXT_COMPARED))?,
    })
}

/// Case (d) of [`Edits::correspond`]: whether `old_address` and
/// `new_address` are the same start or end of a section of the same name,
/// or of its load image.
fn same_edge(old_binary: &Binary, old_address: u64, new_binary: &Binary, new_address: u64) -> bool {
    let old_edges = edges(old_binary, old_address);
    !old_edges.is_empty()
        && edges(new_binary, new_address)
            .iter()
            .any(|edge| old_edges.contains(edge))
}

/// The starts and ends of sections and load images that are at `value`, as
/// (the section's name, whether it is the load image, whether it is the
/// end).
fn edges(binary: &Binary, value: u64) -> Vec<(&str, bool, bool)> {
    let mut edges = Vec::new();
    for section in &binary.sections {
        if !section.allocated || section.size == 0 {
            continue;
        }
        let starts = [(Some(section.address), false), (section.load_address, true)];
        for (start, load_image) in starts {
            let Some(start) = start else { continue };
            if value == start {
                edges.push((section.name.as_str(), load_image, false));
            }
            if Some(value) == start.checked_add(section.size) {
                edges.push((section.name.as_str(), load_image, true));
            }
        }
    }
    edges
}

#[cfg(test)]
mod tests;
