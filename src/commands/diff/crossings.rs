//! The calls and other uses between blocks of memory that are new or gone:
//! for each block, the rows that `elfq NEW callees BLOCK` lists, of items
//! in other blocks, for a use that `elfq OLD callees BLOCK` doesn't list,
//! and the other way round. A use is the same in both builds when it is
//! between partners, in the same way, and from and to the same blocks,
//! where a call and a tail call, direct or through a veneer or a PLT stub,
//! are one way. So a call that becomes a tail call or stops being one, or
//! gains or loses a veneer in the caller's or the callee's block, is
//! neither new nor gone, but when an item moves to another block, each of
//! its uses between blocks before is gone, and each after is new. When the
//! caller and the callee share a block and the veneer is in another, as a
//! PLT can be in a library with two blocks of code, the call to the veneer
//! crosses blocks itself (see `graph/boundary.rs`), and is new or gone with
//! the veneer.

use std::collections::HashSet;

use super::Build;
use super::blocks::Block;
use super::rows::{Items, Status};
use crate::commands::ranges::{self, CrossingRow};
use crate::graph::{Boundary, Node, ReferenceKind};

/// A use between blocks that only one build has, as a row of `callees
/// BLOCK` in that build lists it: the new build's for a new use, and the
/// old build's for a gone one.
pub(super) struct Crossing {
    /// `New` or `Gone`.
    pub status: Status,
    /// The item that is used, as an index into the build's items.
    pub callee: usize,
    /// The item in another block that uses it, how, through which veneer,
    /// and where.
    pub row: CrossingRow,
}

impl Crossing {
    /// The build whose row it is.
    pub fn build<'a>(&self, items: &Items<'a>) -> &'a Build {
        match self.status {
            Status::Gone => items.old,
            _ => items.new,
        }
    }

    /// Whether it is a call or a tail call, which the text lists, rather
    /// than a use of an item's address or bytes, which it only counts.
    pub fn is_call(&self) -> bool {
        self.row.kind.is_call()
    }
}

/// What a use between blocks is, alike in both builds: the item that uses
/// and the item it uses, how, where a tail call counts as a call, and the
/// blocks that hold the two, as indices into the blocks. A call through a
/// veneer or a PLT stub has the key of a direct one, and only calls go
/// through them. Its numbers are 32 bits, as a library can have hundreds
/// of thousands of uses between blocks.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    caller: End,
    callee: End,
    kind: ReferenceKind,
    blocks: (u32, u32),
}

/// An item, as a key names it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum End {
    /// An item of the new build, or an old item's partner there.
    New(u32),
    /// An item of the old build that has no partner, which only its own
    /// uses can have.
    Old(u32),
}

/// The uses between blocks that are new, then those that are gone, each in
/// the order of the blocks and as `callees BLOCK` lists them in each. Of
/// the new build's uses, it keeps only those that are new, to hold less.
pub(super) fn compare(items: &Items, blocks: &[Block]) -> Vec<Crossing> {
    let mut old = Vec::new();
    let partner = |index| match items.pairing.old_to_new[index] {
        Some((partner, _)) => End::New(partner as u32),
        None => End::Old(index as u32),
    };
    uses(items.old, &items.old_blocks, blocks, partner, |used| {
        old.push(used);
    });
    let old_keys: HashSet<Key> = old.iter().map(|used| used.key).collect();
    let mut new_keys = HashSet::new();
    let mut crossings = Vec::new();
    let new = |index| End::New(index as u32);
    uses(items.new, &items.new_blocks, blocks, new, |used| {
        new_keys.insert(used.key);
        if !old_keys.contains(&used.key) {
            crossings.push(used.crossing(Status::New));
        }
    });
    drop(old_keys);
    let gone = old.into_iter().filter(|used| !new_keys.contains(&used.key));
    crossings.extend(gone.map(|used| used.crossing(Status::Gone)));
    crossings
}

/// A row of `callees BLOCK`, with its key.
struct Use {
    key: Key,
    callee: usize,
    row: CrossingRow,
}

impl Use {
    fn crossing(self, status: Status) -> Crossing {
        Crossing {
            status,
            callee: self.callee,
            row: self.row,
        }
    }
}

/// Gives `each` the uses between blocks in `build`: for each block, the
/// rows that `callees BLOCK` lists, of items that start in it, of items in
/// other blocks, but not of imports. `item_blocks` gives the block of each
/// of the build's items, and `end` names them as keys do.
fn uses(
    build: &Build,
    item_blocks: &[Option<usize>],
    blocks: &[Block],
    end: impl Fn(usize) -> End,
    mut each: impl FnMut(Use),
) {
    let binary = &build.binary;
    for (block, range) in blocks.iter().map(|block| block.range).enumerate() {
        let leaving = Boundary::new(binary, &build.graph, &[range]).leaving();
        for (callee, row) in ranges::callee_rows(binary, &leaving) {
            let (Node::Item(callee), Node::Item(caller)) = (callee, row.inside) else {
                continue;
            };
            let Some(callee_block) = item_blocks[callee] else {
                continue;
            };
            if item_blocks[caller] != Some(block) || callee_block == block {
                continue;
            }
            let kind = match row.kind {
                ReferenceKind::TailCall => ReferenceKind::Call,
                kind => kind,
            };
            each(Use {
                key: Key {
                    caller: end(caller),
                    callee: end(callee),
                    kind,
                    blocks: (block as u32, callee_block as u32),
                },
                callee,
                row,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use object::{Architecture, SectionKind};

    use super::*;
    use crate::binary::{AddressRange, Binary, Item, ItemKind, Section, Source};
    use crate::commands::diff::pairing::{PairedBy, Pairing};
    use crate::commands::json::how_field;
    use crate::graph::{Graph, Reference};
    use ReferenceKind::{Call, TailCall};

    /// The blocks: ITCM at 0x0, flash at 0x1000 and SDRAM at 0x2000.
    const BLOCKS: [(u64, u64); 3] = [(0x0, 0x100), (0x1000, 0x1100), (0x2000, 0x2100)];

    /// A Cortex-M build of 16-byte functions, each a name and an address,
    /// with calls and tail calls between them, from and to their indices. A
    /// function whose name ends in `_veneer` is 8 bytes of a GNU ld veneer,
    /// which jumps on to the function it tail-calls.
    fn build(functions: &[(&str, u64)], calls: &[(usize, usize, ReferenceKind)]) -> Build {
        let text = Section {
            name: ".text".to_string(),
            address: 0,
            size: 0x3000,
            kind: SectionKind::Text,
            allocated: true,
            file_range: Some((0, 0x3000)),
            entry_size: 0,
            load_address: None,
        };
        let mut data = vec![0; 0x3000];
        let items: Vec<Item> = functions
            .iter()
            .map(|&(name, address)| {
                let veneer = name.ends_with("_veneer");
                if veneer {
                    // ldr.w pc, [pc, #-0]. The word after it is left 0, as
                    // is_veneer reads only the instruction, and the graph
                    // gives the target.
                    data[address as usize..][..4].copy_from_slice(&[0x5f, 0xf8, 0x00, 0xf0]);
                }
                Item {
                    name: name.to_string(),
                    aliases: Vec::new(),
                    kind: ItemKind::Function,
                    address,
                    size: if veneer { 8 } else { 16 },
                    size_estimated: false,
                    section: 0,
                    source: Source::SymbolTable,
                }
            })
            .collect();
        let mut graph = Graph::default();
        for &(from, to, kind) in calls {
            graph.references.push(Reference {
                from,
                to: Node::Item(to),
                kind,
                site: items[from].address + 4,
            });
        }
        graph.references.sort_by_key(|r| (r.from, r.site));
        let mut binary = Binary::for_tests(vec![text], items, data);
        binary.architecture = Architecture::Arm;
        Build {
            binary,
            graph,
            tree: None,
        }
    }

    /// The uses between blocks that are new or gone from `old` to `new`,
    /// whose items are paired by name, as "new call via veneer main ->
    /// far".
    fn new_and_gone(old: &Build, new: &Build) -> Vec<String> {
        let blocks: Vec<Block> = BLOCKS
            .iter()
            .map(|&(start, end)| Block {
                range: AddressRange { start, end },
                old: Vec::new(),
                new: Vec::new(),
                same_bytes: false,
            })
            .collect();
        let partners = |from: &Build, to: &Build| -> Vec<Option<usize>> {
            let items = &to.binary.items;
            from.binary
                .items
                .iter()
                .map(|item| items.iter().position(|other| other.name == item.name))
                .collect()
        };
        let in_blocks = |build: &Build| {
            let items = build.binary.items.iter();
            items
                .map(|item| blocks.iter().position(|b| b.range.contains(item.address)))
                .collect()
        };
        let old_to_new = partners(old, new).into_iter();
        let items = Items {
            old,
            new,
            pairing: Pairing {
                old_to_new: old_to_new.map(|p| p.map(|p| (p, PairedBy::Name))).collect(),
                new_to_old: partners(new, old),
            },
            old_blocks: in_blocks(old),
            new_blocks: in_blocks(new),
            changes: Vec::new(),
            unchanged: 0,
        };
        compare(&items, &blocks)
            .iter()
            .map(|crossing| {
                let binary = &crossing.build(&items).binary;
                let row = &crossing.row;
                let via = row.veneer.map_or("", |_| " via veneer");
                format!(
                    "{} {}{via} {} -> {}",
                    crossing.status.word(),
                    how_field(row.kind),
                    row.inside.name(binary),
                    binary.items[crossing.callee].name,
                )
            })
            .collect()
    }

    #[test]
    fn a_use_is_the_same_between_partners_in_the_same_blocks() {
        // filter moves from ITCM to flash, where it calls memcpy without
        // crossing, and main calls it there. Each build has a helper that
        // the other doesn't. main's call to memcpy is the same, though the
        // builds number all three otherwise.
        let old = build(
            &[
                ("main", 0x0),
                ("filter", 0x10),
                ("memcpy", 0x1000),
                ("old_helper", 0x1010),
            ],
            &[(0, 1, Call), (0, 2, Call), (1, 2, Call), (0, 3, Call)],
        );
        let new = build(
            &[
                ("memcpy", 0x1000),
                ("filter", 0x1010),
                ("main", 0x0),
                ("new_helper", 0x1020),
            ],
            &[(2, 1, Call), (2, 0, Call), (1, 0, Call), (2, 3, Call)],
        );
        assert_eq!(
            new_and_gone(&old, &new),
            [
                "new call main -> filter",
                "new call main -> new_helper",
                "gone call filter -> memcpy",
                "gone call main -> old_helper",
            ]
        );
    }

    #[test]
    fn a_call_is_the_same_however_it_is_made_but_not_from_another_block() {
        // main, in ITCM, calls three functions in flash, and poll, in
        // SDRAM, calls tick. In the new build, main tail-calls tick and
        // calls far through a veneer, log is in SDRAM, and poll is in ITCM.
        let old = build(
            &[
                ("main", 0x0),
                ("tick", 0x1000),
                ("far", 0x1010),
                ("log", 0x1020),
                ("poll", 0x2010),
            ],
            &[(0, 1, Call), (0, 2, Call), (0, 3, Call), (4, 1, Call)],
        );
        let new = build(
            &[
                ("main", 0x0),
                ("__far_veneer", 0x10),
                ("tick", 0x1000),
                ("far", 0x1010),
                ("log", 0x2000),
                ("poll", 0x20),
            ],
            &[
                (0, 2, TailCall),
                (0, 1, Call),
                (1, 3, TailCall),
                (0, 4, Call),
                (5, 2, Call),
            ],
        );
        // The new build lists main's calls to tick and far in other ways,
        // but they are the same. main's call to log now goes to SDRAM, and
        // poll's call to tick comes from ITCM, so both are gone and new.
        let nothing = build(&[], &[]);
        assert_eq!(
            new_and_gone(&nothing, &new),
            [
                "new call via veneer main -> far",
                "new call main -> log",
                "new tail call main -> tick",
                "new call poll -> tick",
            ]
        );
        assert_eq!(
            new_and_gone(&old, &new),
            [
                "new call main -> log",
                "new call poll -> tick",
                "gone call main -> log",
                "gone call poll -> tick",
            ]
        );
        // The other way round, main's tail call becomes a call and its
        // veneer goes.
        assert_eq!(new_and_gone(&new, &old), new_and_gone(&old, &new));
    }
}
