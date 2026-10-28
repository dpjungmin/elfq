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
//!   takes the address of outside the function, as `arch::operands` finds
//!   it;
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
use crate::arch::{self, Operands};
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
        let operands = (code && arch::is_supported(a.architecture, a.thumb)).then(|| {
            let operands = |binary: &Binary, bytes, address| {
                arch::operands(binary.architecture, binary.endianness, bytes, address)
            };
            (
                operands(a, &x.bytes, old_item.address),
                operands(b, &y.bytes, new_item.address),
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
            for (bits, (x, y)) in explained.iter_mut().zip(x.relative.iter().zip(&y.relative)) {
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
mod tests {
    use object::{Architecture, SectionKind};

    use super::*;
    use crate::binary::{Relocation, Section, Source};
    use crate::commands::diff::pairing::PairedBy;
    use crate::graph::{Graph, Reference};

    /// The sections of a build: code, constants and variables, each stored
    /// in the file at its address.
    const SECTIONS: [(&str, u64, u64); 3] = [
        (".text", 0x100, 0x100),
        (".rodata", 0x400, 0x100),
        (".data", 0x800, 0x100),
    ];

    /// A Cortex-M build with `sections` and `items`, each a name, an
    /// address and a size, in address order: a function in `.text`, and
    /// data elsewhere. Its bytes are 0 but for `bytes`, each at an address,
    /// and its graph has `calls`, from and to items by name, where a
    /// veneer's call is its jump.
    fn build(
        sections: &[(&str, u64, u64)],
        items: &[(&str, u64, u64)],
        bytes: &[(u64, &[u8])],
        calls: &[(&str, &str)],
    ) -> Build {
        let sections: Vec<Section> = sections
            .iter()
            .map(|&(name, address, size)| Section {
                name: name.to_string(),
                address,
                size,
                kind: SectionKind::Data,
                allocated: true,
                file_range: Some((address, size)),
                entry_size: 0,
                load_address: None,
            })
            .collect();
        let items: Vec<Item> = items
            .iter()
            .map(|&(name, address, size)| {
                let section = sections.iter().position(|s| s.contains(address)).unwrap();
                Item {
                    name: name.to_string(),
                    aliases: Vec::new(),
                    kind: [ItemKind::Function, ItemKind::Data][usize::from(section > 0)],
                    address,
                    size,
                    size_estimated: false,
                    section,
                    source: Source::SymbolTable,
                }
            })
            .collect();
        let mut data = vec![0; 0x900];
        for &(address, bytes) in bytes {
            data[address as usize..][..bytes.len()].copy_from_slice(bytes);
        }
        let index = |name: &str| items.iter().position(|item| item.name == name).unwrap();
        let mut graph = Graph::default();
        for &(from, to) in calls {
            graph.references.push(Reference {
                from: index(from),
                to: Node::Item(index(to)),
                kind: match from.ends_with("_veneer") {
                    true => ReferenceKind::TailCall,
                    false => ReferenceKind::Call,
                },
                site: items[index(from)].address,
            });
        }
        graph.references.sort_by_key(|r| (r.from, r.site));
        let mut binary = Binary::for_tests(sections, items, data);
        binary.architecture = Architecture::Arm;
        binary.pointer_size = 4;
        binary.thumb = true;
        Build {
            binary,
            graph,
            tree: None,
        }
    }

    /// `build` as an x86-64 build.
    fn x86(mut build: Build) -> Build {
        build.binary.architecture = Architecture::X86_64;
        build.binary.pointer_size = 8;
        build.binary.thumb = false;
        build
    }

    /// Whether `name` is edited from `old` to `new`, whose items pair by
    /// name.
    fn edited(old: &Build, new: &Build, name: &str) -> bool {
        let partners = |from: &Build, to: &Build| -> Vec<Option<usize>> {
            let items = &to.binary.items;
            from.binary
                .items
                .iter()
                .map(|item| items.iter().position(|other| other.name == item.name))
                .collect()
        };
        let pairing = Pairing {
            old_to_new: partners(old, new)
                .into_iter()
                .map(|partner| partner.map(|partner| (partner, PairedBy::Name)))
                .collect(),
            new_to_old: partners(new, old),
        };
        let index = |build: &Build| {
            let items = &build.binary.items;
            items.iter().position(|item| item.name == name).unwrap()
        };
        Edits::new(old, new, &pairing).edited(index(old), index(new))
    }

    /// Whether `table`, a word at 0x400 that holds `old` in the old build
    /// and `new` in the new one, is edited. The functions f and g swap
    /// places, f at 0x100 and g at 0x110 in the old build; `gone` and
    /// `new_item` are only in one build, at 0x480 and 0x490; and text that
    /// no item holds moved: "hello" from 0x4c0 to 0x4e0, "hi!" at 0x4d0
    /// became "hi?" at 0x4f0, and "okay", which "then" follows with no NUL
    /// between, as Rust's strings do, is at 0x4d8 and then 0x4f8, before
    /// "now". The new build also has "hello" in .data.
    fn table_edited(old: u32, new: u32) -> bool {
        let old_bytes = old.to_le_bytes();
        let new_bytes = new.to_le_bytes();
        let old = build(
            &SECTIONS,
            &[
                ("f", 0x100, 0x10),
                ("g", 0x110, 0x10),
                ("table", 0x400, 4),
                ("gone", 0x480, 8),
            ],
            &[
                (0x400, &old_bytes),
                (0x4c0, b"hello\0"),
                (0x4d0, b"hi!"),
                (0x4d8, b"okaythen"),
            ],
            &[],
        );
        let new = build(
            &SECTIONS,
            &[
                ("g", 0x100, 0x10),
                ("f", 0x110, 0x10),
                ("table", 0x400, 4),
                ("new_item", 0x490, 8),
            ],
            &[
                (0x400, &new_bytes),
                (0x4e0, b"hello\0"),
                (0x4f0, b"hi?"),
                (0x4f8, b"okaynow"),
                (0x880, b"hello"),
            ],
            &[],
        );
        edited(&old, &new, "table")
    }

    #[test]
    fn a_pointer_into_partners_at_the_same_offset_is_the_same() {
        // Case (a): f's address, with the Thumb bit, and 4 bytes into it.
        assert!(!table_edited(0x101, 0x111));
        assert!(!table_edited(0x104, 0x114));
        assert!(!table_edited(0x111, 0x101));
        // f's Thumb bit is lost.
        assert!(table_edited(0x101, 0x110));
    }

    #[test]
    fn a_pointer_just_past_the_end_of_partners_is_the_same() {
        // Case (a): the end of f, which is where g starts in the old build
        // and where no item is in the new one, as for the end of an array.
        assert!(!table_edited(0x110, 0x120));
    }

    #[test]
    fn a_number_that_changed_is_an_edit_where_it_reads_as_an_address_too() {
        // A coefficient, 5 to 7, and a table's 0x104 to 0x116: f's 4th byte
        // and 6th.
        assert!(table_edited(5, 7));
        assert!(table_edited(0x104, 0x116));
    }

    #[test]
    fn pointers_into_items_that_only_one_build_has_are_the_same() {
        // Case (b): such items have rows of their own, but a pointer into a
        // partner that now points into one of them is an edit.
        assert!(!table_edited(0x480, 0x490));
        assert!(table_edited(0x484, 0x101));
        assert!(table_edited(0x101, 0x494));
    }

    #[test]
    fn a_pointer_to_the_same_text_is_the_same() {
        // Case (c): text that no item holds, as a Rust string's. A whole
        // string, or 3 characters of one, are the same text, but not 2 that
        // go on otherwise, nor text in another section.
        assert!(!table_edited(0x4c0, 0x4e0));
        assert!(!table_edited(0x4c2, 0x4e2));
        assert!(!table_edited(0x4c3, 0x4e3));
        assert!(!table_edited(0x4d8, 0x4f8));
        assert!(table_edited(0x4d0, 0x4f0));
        assert!(table_edited(0x4c0, 0x880));
    }

    #[test]
    fn a_pointer_to_the_same_end_of_a_section_is_the_same() {
        // Case (d): the end of .data, which shrank, as a linker script's
        // _edata.
        let data_ending_at = |end: u64| {
            let mut sections = SECTIONS;
            sections[2].2 = end - 0x800;
            let bytes = (end as u32).to_le_bytes();
            build(&sections, &[("table", 0x400, 4)], &[(0x400, &bytes)], &[])
        };
        let (old, new) = (data_ending_at(0x900), data_ending_at(0x880));
        assert!(!edited(&old, &new, "table"));
        let other = build(
            &SECTIONS,
            &[("table", 0x400, 4)],
            &[(0x400, &[0x80, 8, 0, 0])],
            &[],
        );
        assert!(edited(&old, &other, "table"));
    }

    #[test]
    fn a_pointer_that_the_loader_writes_is_compared_as_it_writes_it() {
        // limit_ptr is 0 in the file, as LLD leaves it, and the loader
        // writes &limits[1], then &limits[2]; and f's address, which moved.
        let pointer = |to: u64| {
            let items = [
                ("f", 0x100, 0x10),
                ("g", 0x110, 0x10),
                ("limits", 0x400, 16),
                ("limit_ptr", 0x800, 8),
            ];
            let mut build = x86(build(&SECTIONS, &items, &[], &[]));
            build.binary.relocations = vec![Relocation {
                address: 0x800,
                target: RelocationTarget::Address(to),
            }];
            build
        };
        assert!(edited(&pointer(0x404), &pointer(0x408), "limit_ptr"));
        assert!(!edited(&pointer(0x404), &pointer(0x404), "limit_ptr"));
        let (mut old, new) = (pointer(0x100), pointer(0x110));
        old.binary.items.swap(0, 1);
        (old.binary.items[0].address, old.binary.items[1].address) = (0x100, 0x110);
        assert!(!edited(&old, &new, "limit_ptr"));
    }

    #[test]
    fn x86_jump_table_entries_are_distances_from_the_table() {
        // A table at 0x404 of 3 entries, each a distance from it into f,
        // which moved from 0x100 to 0x120, while the table didn't.
        let table = |f: u64, offsets: [u64; 3]| {
            let entries: Vec<u8> = offsets
                .iter()
                .flat_map(|offset| ((f + offset).wrapping_sub(0x404) as u32).to_le_bytes())
                .collect();
            let items = [("f", f, 0x40), ("table", 0x404, 12)];
            x86(build(&SECTIONS, &items, &[(0x404, &entries)], &[]))
        };
        let old = table(0x100, [0x10, 0x20, 0x30]);
        assert!(!edited(&old, &table(0x120, [0x10, 0x20, 0x30]), "table"));
        // A case that now goes elsewhere in f.
        assert!(edited(&old, &table(0x120, [0x10, 0x28, 0x30]), "table"));
    }

    /// Thumb's `bl` at `from` to `to`.
    fn bl(from: u64, to: u64) -> [u8; 4] {
        let offset = to.wrapping_sub(from + 4) as u32;
        let s = (offset >> 24) & 1;
        let j1 = !((offset >> 23) ^ s) & 1;
        let j2 = !((offset >> 22) ^ s) & 1;
        let first = 0xf000 | (s << 10) | ((offset >> 12) & 0x3ff);
        let second = 0xd000 | (j1 << 13) | (j2 << 11) | ((offset >> 1) & 0x7ff);
        halfwords(first, second)
    }

    /// Thumb's `movw r0, #value`, or with `top`, `movt r0, #value`.
    fn mov(top: bool, value: u32) -> [u8; 4] {
        let first = [0xf240, 0xf2c0][usize::from(top)]
            | ((value >> 12) & 0xf)
            | (((value >> 11) & 1) << 10);
        halfwords(first, (((value >> 8) & 7) << 12) | (value & 0xff))
    }

    fn halfwords(first: u32, second: u32) -> [u8; 4] {
        let [a, b] = (first as u16).to_le_bytes();
        let [c, d] = (second as u16).to_le_bytes();
        [a, b, c, d]
    }

    /// A build where f, at 0x100, calls g at `g` and puts `number` in r0
    /// with `movw`, and with `movt`, and `var` is at `var`.
    fn code(g: u64, number: u32, movt: bool, var: u64) -> Build {
        let mut code = bl(0x100, g).to_vec();
        code.extend(mov(false, number & 0xffff));
        if movt {
            code.extend(mov(true, number >> 16));
        }
        let items = [("f", 0x100, 0x10), ("g", g, 0x10), ("var", var, 4)];
        build(&SECTIONS, &items, &[(0x100, &code)], &[("f", "g")])
    }

    #[test]
    fn thumb_code_is_the_same_where_only_distances_and_addresses_changed() {
        // g and var moved: the distance in bl to g and the address of var
        // that movw and movt put together changed.
        let old = code(0x120, 0x800, true, 0x800);
        assert!(!edited(&old, &code(0x130, 0x810, true, 0x810), "f"));
    }

    #[test]
    fn a_number_that_movw_and_movt_put_together_is_an_edit_when_it_changed() {
        // The number that f compares a header with, 0xe1f0 to 0xe1f2.
        let magic = |number| code(0x120, number, true, 0x800);
        assert!(edited(&magic(0xe1f0), &magic(0xe1f2), "f"));
    }

    #[test]
    fn movw_alone_puts_no_address_together() {
        let alone = |number| code(0x120, number, false, number.into());
        assert!(edited(&alone(0x800), &alone(0x810), "f"));
    }

    #[test]
    fn a_word_in_a_literal_pool_is_compared_as_a_word_but_not_as_a_use() {
        // f calls code at 0x1f0, where no item is, then loads the end of
        // ring from the pool after it, where `after` starts in the old
        // build and `extra`, a new item, in the new one, so the graph says
        // that f uses another item; or a number.
        let loading = |word: u32, items: &[(&str, u64, u64)]| {
            let mut code = bl(0x100, 0x1f0).to_vec();
            code.extend([0x00, 0x48, 0x00, 0xbf]);
            code.extend(word.to_le_bytes());
            let mut build = build(&SECTIONS, items, &[(0x100, &code)], &[]);
            if let Some(used) = items.iter().position(|item| item.1 == u64::from(word)) {
                build.graph.references.push(Reference {
                    from: 0,
                    to: Node::Item(used),
                    kind: ReferenceKind::Address,
                    site: 0x104,
                });
            }
            build
        };
        let old = [("f", 0x100, 12), ("ring", 0x800, 8), ("after", 0x808, 4)];
        let new = [
            ("f", 0x100, 12),
            ("ring", 0x810, 8),
            ("extra", 0x818, 4),
            ("after", 0x81c, 4),
        ];
        assert!(!edited(&loading(0x808, &old), &loading(0x818, &new), "f"));
        assert!(edited(&loading(0x808, &old), &loading(5, &old), "f"));
    }

    #[test]
    fn partners_with_the_same_code_are_the_same() {
        // f's bl leads to 0x110 in both builds, where g is in the old one
        // and h in the new one. Only partners whose bytes differ are
        // compared.
        let calling = |items: &[(&str, u64, u64)], to: &str| {
            build(
                &SECTIONS,
                items,
                &[(0x100, &bl(0x100, 0x110))],
                &[("f", to)],
            )
        };
        let old = [("f", 0x100, 8), ("g", 0x110, 0x10), ("h", 0x120, 0x10)];
        let new = [("f", 0x100, 8), ("h", 0x110, 0x10), ("g", 0x120, 0x10)];
        assert!(!edited(&calling(&old, "g"), &calling(&new, "h"), "f"));
    }

    #[test]
    fn x86_code_is_the_same_where_only_distances_changed() {
        // f calls g, and puts the address of var in eax.
        let code = |g: u64, var: u32| {
            let mut code = vec![0xe8];
            code.extend((g.wrapping_sub(0x105) as u32).to_le_bytes());
            code.push(0xb8);
            code.extend(var.to_le_bytes());
            code.push(0xc3);
            let items = [("f", 0x100, 11), ("g", g, 0x10), ("var", var.into(), 4)];
            x86(build(&SECTIONS, &items, &[(0x100, &code)], &[("f", "g")]))
        };
        let old = code(0x120, 0x800);
        assert!(!edited(&old, &code(0x130, 0x800), "f"));
        // An address in an instruction is compared as a number, as any.
        assert!(edited(&old, &code(0x120, 0x810), "f"));
    }

    #[test]
    fn code_that_calls_other_items_is_edited() {
        let functions = [
            ("f", 0x100, 0x10),
            ("g", 0x110, 0x10),
            ("h", 0x120, 0x10),
            ("__g_veneer", 0x130, 8),
        ];
        // f's bl, and ldr.w pc, [pc, #-0], as GNU ld's veneer.
        let calling = |to: &str, calls: &[(&str, &str)]| {
            let at = functions.iter().find(|item| item.0 == to).unwrap().1;
            let bytes: &[(u64, &[u8])] =
                &[(0x100, &bl(0x100, at)), (0x130, &[0x5f, 0xf8, 0x00, 0xf0])];
            build(&SECTIONS, &functions, bytes, calls)
        };
        let old = calling("g", &[("f", "g")]);
        assert!(edited(&old, &calling("h", &[("f", "h")]), "f"));
        // Through a veneer, f still calls g.
        let through = calling("__g_veneer", &[("f", "__g_veneer"), ("__g_veneer", "g")]);
        assert!(!edited(&old, &through, "f"));
    }

    #[test]
    fn a_call_from_an_item_that_only_one_build_has_to_another_doesnt_count() {
        // f calls gone in the old build and new in the new one, each with
        // a row of its own.
        let calling = |to: &str, at: u64| {
            let items = [("f", 0x100, 0x10), (to, at, 0x10)];
            build(&SECTIONS, &items, &[(0x100, &bl(0x100, at))], &[("f", to)])
        };
        assert!(!edited(
            &calling("gone", 0x110),
            &calling("new", 0x120),
            "f"
        ));
    }

    #[test]
    fn imports_are_compared_by_name() {
        // f's bytes are the same, but the global offset table entry it
        // reads holds another import, which only one build has.
        let importing = |name: &str| {
            let mut build = x86(build(&SECTIONS, &[("f", 0x100, 0x10)], &[], &[]));
            build.binary.imports = vec![name.to_string()];
            build.graph.references.push(Reference {
                from: 0,
                to: Node::Import(0),
                kind: ReferenceKind::Call,
                site: 0x100,
            });
            build
        };
        assert!(edited(&importing("unlink"), &importing("rmdir"), "f"));
        assert!(!edited(&importing("unlink"), &importing("unlink"), "f"));
    }
}
