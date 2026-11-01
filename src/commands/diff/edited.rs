//! Which paired items changed while keeping their size and block: those
//! whose bytes differ other than where they say where something is, or
//! whose code now uses different items than before.
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
//! Then, where the distances in the partners' code lead to places that
//! aren't the same place in partners, the code must use the partners of
//! what it used there, in the same ways, through veneers and the PLT stubs
//! of a library's own functions, and the same imports. A use of an item
//! that is new or gone, which has a row of its own, doesn't count. What
//! the words and numbers point at isn't compared again as a use: they are
//! compared already, and the graph takes a pointer to the end of an array
//! for a use of whatever follows it.

use std::borrow::Cow;
use std::collections::HashMap;

use object::{Architecture, Endianness};

use super::Build;
use super::pairing::Pairing;
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
struct Place {
    item: usize,
    offset: u64,
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
    pub fn new(old: &'a Build, new: &'a Build, pairing: &'a Pairing) -> Edits<'a> {
        Edits {
            old,
            new,
            pairing,
            old_veneers: Veneers::new(&old.binary, &old.graph),
            new_veneers: Veneers::new(&new.binary, &new.graph),
        }
    }

    /// Whether item `old` of the old build and its partner `new`, of the
    /// same size, are edited: their bytes differ other than where they say
    /// where something is, or their code now uses different items.
    pub fn edited(&self, old: usize, new: usize) -> bool {
        let (a, b) = (&self.old.binary, &self.new.binary);
        let (old_item, new_item) = (&a.items[old], &b.items[new]);
        let (Some(x), Some(y)) = (loaded(a, old_item), loaded(b, new_item)) else {
            return false;
        };
        if x == y {
            // The same code can still read another import from a global
            // offset table entry, which the loader fills in.
            return imports(self.old, old) != imports(self.new, new);
        }
        if x.imports != y.imports {
            return true;
        }
        let code = old_item.kind == ItemKind::Function;
        // diff refuses builds for two architectures, and reads the new
        // build's code as it reads the old build's: as Thumb when the old
        // build's is, even where the new build doesn't say it is.
        let instruction_sets =
            a.instruction_set()
                .zip(InstructionSet::of(b.architecture, a.thumb, b.endianness));
        let operands = instruction_sets.filter(|_| code).map(|(old_set, new_set)| {
            (
                old_set.operands(&x.bytes, old_item.address),
                new_set.operands(&y.bytes, new_item.address),
            )
        });
        self.bytes_edited(old_item, &x.bytes, new_item, &y.bytes, operands.as_ref())
            || operands.is_some_and(|(old_operands, new_operands)| {
                let sites = self.distances_elsewhere(&old_operands, &new_operands);
                self.uses_edited(old, new, &sites)
            })
    }

    /// Whether the partners' bytes, `old` of `old_item` and `new` of
    /// `new_item`, differ other than in the distances in their code,
    /// `operands`, and in the corresponding addresses that they hold.
    fn bytes_edited(
        &self,
        old_item: &Item,
        old: &[u8],
        new_item: &Item,
        new: &[u8],
        operands: Option<&(Operands, Operands)>,
    ) -> bool {
        let (a, b) = (&self.old.binary, &self.new.binary);
        // The bits of each byte that hold a distance or an address that
        // corresponds.
        let mut explained = vec![0u8; old.len()];
        if let Some((x, y)) = operands {
            for (bits, (x, y)) in explained
                .iter_mut()
                .zip(x.distance_bits.iter().zip(&y.distance_bits))
            {
                *bits = x & y;
            }
            let new_numbers: HashMap<_, u64> = y
                .split
                .iter()
                .map(|number| (&number.bits, number.value))
                .collect();
            for number in &x.split {
                if let Some(&value) = new_numbers.get(&number.bits)
                    && value != number.value
                    && self.correspond(number.value, value)
                {
                    for &(offset, bits) in &number.bits {
                        explained[offset] |= bits;
                    }
                }
            }
        }
        // Data, and Thumb code, which keeps constants and tables of
        // addresses among its instructions, hold addresses in words. An
        // x86-64 jump table holds 32-bit distances from the table instead.
        let code = old_item.kind == ItemKind::Function;
        if !code || a.thumb {
            let pointer = a.pointer_size as usize;
            let entries = !code && a.architecture == Architecture::X86_64;
            let step = if entries { 4 } else { pointer };
            let mut offset = (step - (old_item.address % step as u64) as usize) % step;
            while offset + step <= old.len() {
                // A word where one starts and fits, or else an entry.
                let starts = (old_item.address + offset as u64).is_multiple_of(pointer as u64);
                let size = if starts && offset + pointer <= old.len() {
                    pointer
                } else {
                    step
                };
                let range = offset..offset + size;
                offset += size;
                let (x, y) = (&old[range.clone()], &new[range.clone()]);
                let unexplained = x
                    .iter()
                    .zip(y)
                    .zip(&explained[range.clone()])
                    .any(|((x, y), explained)| (x ^ y) & !explained != 0);
                if !unexplained {
                    continue;
                }
                let word_corresponds = size == pointer
                    && self.correspond(word(x, a.endianness), word(y, b.endianness));
                let entries_correspond = entries
                    && x.chunks(4).zip(y.chunks(4)).all(|(x, y)| {
                        x == y || self.entries_correspond(old_item.address, x, new_item.address, y)
                    });
                // Words don't overlap, so one whose values don't
                // correspond is an edit, whatever the others hold.
                if !word_corresponds && !entries_correspond {
                    return true;
                }
                explained[range].fill(0xff);
            }
        }
        old.iter()
            .zip(new)
            .zip(&explained)
            .any(|((x, y), explained)| (x ^ y) & !explained != 0)
    }

    /// Whether `old` and `new`, the 32-bit entries of a table at `old_table`
    /// and `new_table` in each build, lead to places that correspond, each
    /// entry being a distance from its table, as in an x86-64 jump table.
    fn entries_correspond(&self, old_table: u64, old: &[u8], new_table: u64, new: &[u8]) -> bool {
        let endianness = self.old.binary.endianness;
        let entry = |table: u64, bytes| {
            let distance = word(bytes, endianness) as u32 as i32;
            table.wrapping_add_signed(distance.into())
        };
        self.correspond(entry(old_table, old), entry(new_table, new))
    }

    /// Whether two values that a pair of words or instructions hold are the
    /// same address in each build: (a) the same place in partners (see
    /// [`Edits::same_place_in_partners`]); (b) in items that only one build
    /// has, which have rows of their own (see [`Edits::in_unpaired_items`]);
    /// (c) at the same text (see [`same_text`]); or (d) the same start or
    /// end of a section (see [`same_edge`]).
    fn correspond(&self, old: u64, new: u64) -> bool {
        let (a, b) = (&self.old.binary, &self.new.binary);
        self.same_place_in_partners(old, new)
            || self.in_unpaired_items(old, new)
            || same_text(a, old, b, new)
            || same_edge(a, old, b, new)
    }

    /// Case (a) of [`Edits::correspond`]: whether `old` and `new` point into
    /// partners at the same offset, a function's Thumb bit and all, or just
    /// past the end of partners, as a pointer to the end of an array does.
    fn same_place_in_partners(&self, old: u64, new: u64) -> bool {
        let (a, b) = (&self.old.binary, &self.new.binary);
        let into = match (place(a, old), place(b, new)) {
            (Some(x), Some(y)) => self.partner(x.item) == Some(y.item) && x.offset == y.offset,
            _ => false,
        };
        let past_the_end = match (end(a, old), end(b, new)) {
            (Some(x), Some(y)) => self.partner(x) == Some(y),
            _ => false,
        };
        into || past_the_end
    }

    /// Case (b) of [`Edits::correspond`]: whether `old` and `new` both
    /// point into items without a partner.
    fn in_unpaired_items(&self, old: u64, new: u64) -> bool {
        let (a, b) = (&self.old.binary, &self.new.binary);
        place(a, old).is_some_and(|x| self.partner(x.item).is_none())
            && place(b, new).is_some_and(|y| self.pairing.new_to_old[y.item].is_none())
    }

    /// The partner of the old build's item `old`, as an index into the new
    /// build's items.
    fn partner(&self, old: usize) -> Option<usize> {
        self.pairing.old_to_new[old].map(|(partner, _)| partner)
    }

    /// The offsets of the instructions whose distances, in the partners'
    /// code `old` and `new`, don't lead to the same place in partners, in
    /// order. Only case (a) of [`Edits::correspond`] counts here: a call to
    /// a veneer that only one build has, or a load from a global offset
    /// table, which no item holds, leads elsewhere, and what the code uses
    /// there tells whether it changed.
    fn distances_elsewhere(&self, old: &Operands, new: &Operands) -> Vec<usize> {
        let old_targets: HashMap<usize, u64> = old.targets.iter().copied().collect();
        let new_targets: HashMap<usize, u64> = new.targets.iter().copied().collect();
        let same_place = |site: &usize, target: &u64| {
            new_targets
                .get(site)
                .is_some_and(|&other| self.same_place_in_partners(*target, other))
        };
        let only_new = |site: &usize| !old_targets.contains_key(site);
        let mut sites: Vec<usize> = (old.targets.iter())
            .filter(|(site, target)| !same_place(site, target))
            .map(|&(site, _)| site)
            .chain(new.targets.iter().map(|&(site, _)| site).filter(only_new))
            .collect();
        sites.sort_unstable();
        sites.dedup();
        sites
    }

    /// Whether the partners' code, at the instructions at the offsets
    /// `sites`, uses other items, or in other ways.
    fn uses_edited(&self, old: usize, new: usize, sites: &[usize]) -> bool {
        if sites.is_empty() {
            return false;
        }
        let old_uses = self.uses(self.old, &self.old_veneers, old, sites, |index| {
            self.partner(index)
        });
        let new_uses = self.uses(self.new, &self.new_veneers, new, sites, |index| {
            self.pairing.new_to_old[index].map(|_| index)
        });
        old_uses != new_uses
    }

    /// What the instructions of `item` of `build` at the offsets `sites`
    /// use, each with how, through veneers, in order: of the items that
    /// both builds have, by `paired`, which gives the new build's index for
    /// one, and imports by name.
    fn uses(
        &self,
        build: &'a Build,
        veneers: &Veneers,
        item: usize,
        sites: &[usize],
        paired: impl Fn(usize) -> Option<usize>,
    ) -> Vec<(Used<'a>, ReferenceKind)> {
        let binary = &build.binary;
        let address = binary.items[item].address;
        let at_site = |site: u64| {
            usize::try_from(site.wrapping_sub(address))
                .is_ok_and(|offset| sites.binary_search(&offset).is_ok())
        };
        let import = |index: usize| Used::Import(binary.imports[index].as_str());
        let mut uses: Vec<(Used, ReferenceKind)> = build
            .graph
            .references_from(item)
            .iter()
            .filter(|reference| at_site(reference.site))
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
fn place(binary: &Binary, value: u64) -> Option<Place> {
    let item = binary.item_at(value).or_else(|| {
        let index = binary.item_at(value.checked_sub(1)?)?;
        let thumb = binary.thumb && value % 2 == 1;
        (thumb && binary.items[index].kind == ItemKind::Function).then_some(index)
    })?;
    let offset = value - binary.items[item].address;
    Some(Place { item, offset })
}

/// The item of `binary` that ends just before `value`, as an index into its
/// items.
fn end(binary: &Binary, value: u64) -> Option<usize> {
    let index = binary.item_at(value.checked_sub(1)?)?;
    let item = &binary.items[index];
    (item.address + item.size == value).then_some(index)
}

/// Case (c) of [`Edits::correspond`]: whether `old` and `new` point into
/// sections of the same name, at the same text: a whole string that ends in
/// a 0 byte, or at least 3 characters, as a Rust string has no 0 byte.
fn same_text(a: &Binary, old: u64, b: &Binary, new: u64) -> bool {
    let (Some((x_name, x)), Some((y_name, y))) = (text(a, old), text(b, new)) else {
        return false;
    };
    if x_name != y_name {
        return false;
    }
    let printable = |byte: u8| byte.is_ascii_graphic() || b" \t\n\r".contains(&byte);
    let same = x
        .iter()
        .zip(y)
        .take_while(|&(x, y)| x == y && printable(*x))
        .count();
    let whole = same > 0 && x.get(same) == Some(&0) && y.get(same) == Some(&0);
    whole || same >= 3
}

/// The name of the section that holds `address`, and up to
/// [`TEXT_COMPARED`] of its bytes from there.
fn text(binary: &Binary, address: u64) -> Option<(&str, &[u8])> {
    let section = &binary.sections[binary.section_at(address)?];
    let end = section.address + section.size;
    let bytes = binary.bytes(address, (end - address).min(TEXT_COMPARED))?;
    Some((section.name.as_str(), bytes))
}

/// Case (d) of [`Edits::correspond`]: whether `old` and `new` are the same
/// start or end of a section of the same name, or of its load image.
fn same_edge(a: &Binary, old: u64, b: &Binary, new: u64) -> bool {
    let old_edges = edges(a, old);
    !old_edges.is_empty() && edges(b, new).iter().any(|edge| old_edges.contains(edge))
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
