//! The calls and other uses between blocks of memory that are new or gone:
//! for each block, the rows that `elfq NEW callees BLOCK` lists and `elfq
//! OLD callees BLOCK` doesn't, and the other way round, of items in other
//! blocks. A use is the same in both builds when it is between partners,
//! in the same way, through a veneer in both or in neither, and from and to
//! the same blocks. So when an item moves to another block, each of its
//! uses between blocks before is gone, and each after is new.

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
/// and the item it uses, how and whether through a veneer, and the blocks
/// that hold the two, as indices into the blocks. Its numbers are 32 bits,
/// as a library can have hundreds of thousands of uses between blocks.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    caller: End,
    callee: End,
    kind: ReferenceKind,
    veneer: bool,
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
            each(Use {
                key: Key {
                    caller: end(caller),
                    callee: end(callee),
                    kind: row.kind,
                    veneer: row.veneer.is_some(),
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
    use object::SectionKind;

    use super::*;
    use crate::binary::{AddressRange, Binary, Item, ItemKind, Section, Source};
    use crate::commands::diff::pairing::{PairedBy, Pairing};
    use crate::graph::{Graph, Reference};

    /// A build of 16-byte functions, each a name and an address, with calls
    /// between them, from and to their indices.
    fn build(functions: &[(&str, u64)], calls: &[(usize, usize)]) -> Build {
        let text = Section {
            name: ".text".to_string(),
            address: 0,
            size: 0x2000,
            kind: SectionKind::Text,
            allocated: true,
            file_range: None,
            entry_size: 0,
            load_address: None,
        };
        let items: Vec<Item> = functions
            .iter()
            .map(|&(name, address)| Item {
                name: name.to_string(),
                aliases: Vec::new(),
                kind: ItemKind::Function,
                address,
                size: 16,
                size_estimated: false,
                section: 0,
                source: Source::SymbolTable,
            })
            .collect();
        let mut graph = Graph::default();
        for &(from, to) in calls {
            graph.references.push(Reference {
                from,
                to: Node::Item(to),
                kind: ReferenceKind::Call,
                site: items[from].address + 4,
            });
        }
        graph.references.sort_by_key(|r| (r.from, r.site));
        Build {
            binary: Binary::for_tests(vec![text], items, Vec::new()),
            graph,
            tree: None,
        }
    }

    #[test]
    fn a_use_is_the_same_between_partners_in_the_same_blocks() {
        // ITCM at 0x0 and flash at 0x1000. filter moves to flash, where it
        // calls memcpy without crossing, and main calls it there. Each
        // build has a helper that the other doesn't. main's call to memcpy
        // is the same, though the builds number all three otherwise.
        let old = build(
            &[
                ("main", 0x0),
                ("filter", 0x10),
                ("memcpy", 0x1000),
                ("old_helper", 0x1010),
            ],
            &[(0, 1), (0, 2), (1, 2), (0, 3)],
        );
        let new = build(
            &[
                ("memcpy", 0x1000),
                ("filter", 0x1010),
                ("main", 0x0),
                ("new_helper", 0x1020),
            ],
            &[(2, 1), (2, 0), (1, 0), (2, 3)],
        );
        let block = |start, end| Block {
            range: AddressRange { start, end },
            old: Vec::new(),
            new: Vec::new(),
            same_bytes: false,
        };
        let blocks = [block(0x0, 0x100), block(0x1000, 0x1100)];
        let partner = |index| Some((index, PairedBy::Name));
        let items = Items {
            old: &old,
            new: &new,
            pairing: Pairing {
                old_to_new: vec![partner(2), partner(1), partner(0), None],
                new_to_old: vec![Some(2), Some(1), Some(0), None],
            },
            old_blocks: vec![Some(0), Some(0), Some(1), Some(1)],
            new_blocks: vec![Some(1), Some(1), Some(0), Some(1)],
            changes: Vec::new(),
            unchanged: 0,
        };
        let crossings: Vec<(&str, &str, &str)> = compare(&items, &blocks)
            .iter()
            .map(|crossing| {
                let binary = &crossing.build(&items).binary;
                (
                    crossing.status.word(),
                    crossing.row.inside.name(binary),
                    binary.items[crossing.callee].name.as_str(),
                )
            })
            .collect();
        assert_eq!(
            crossings,
            [
                ("new", "main", "filter"),
                ("new", "main", "new_helper"),
                ("gone", "filter", "memcpy"),
                ("gone", "main", "old_helper"),
            ]
        );
    }
}
