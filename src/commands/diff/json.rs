//! diff's answer as JSON: whether the builds load the same memory, the
//! sections that differ, a row for each block of memory, with each section
//! and load image in it and how the items, the sections without items and
//! the padding make up its change, and a row for each item that changed.
//! The keys that count items are `null`, and the items empty, when diff
//! compared none, as `items_compared` says.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::io::Write;

use anyhow::Result;
use serde::ser::SerializeSeq;
use serde::{Serialize, Serializer};

use super::blocks::{Block, Place};
use super::pairing::PairedBy;
use super::rows::{Change, Status};
use super::{Answer, Build, Compared};
use crate::binary::Binary;
use crate::commands::json::{self, address};
use crate::commands::total;
use crate::graph::Node;

#[derive(Serialize)]
struct DiffJson<'c, 'a> {
    /// The files, as given.
    old: String,
    new: String,
    /// Whether both load the same bytes at the same addresses: exit status
    /// 0 when they do, and 1 when they don't.
    same_bytes: bool,
    /// Whether diff compared the builds' functions and data objects: not
    /// when the memory is the same, nor when either file has no symbol
    /// table, and then the keys below that count them are `null`.
    items_compared: bool,
    /// The bytes of all the functions and data objects in each, as the
    /// overview's `bytes`.
    old_bytes: Option<u64>,
    new_bytes: Option<u64>,
    /// How many paired items have the same size, in the same block.
    unchanged: Option<usize>,
    /// The sections whose address, size, load image or bytes differ, or
    /// that only one build has, in address order, `.tbss` among them: none
    /// when the memory is the same.
    sections_that_differ: Vec<&'a str>,
    /// The blocks of memory that the sections and load images of both
    /// occupy, in address order.
    blocks: Vec<BlockJson<'a>>,
    /// The items that changed, largest change first.
    items: ItemsJson<'c, 'a>,
}

#[derive(Serialize)]
struct BlockJson<'a> {
    #[serde(flatten)]
    range: json::Range,
    /// The bytes from each build's first section or load image in the block
    /// to its furthest end, or `null` when the build has none there.
    old_size: Option<u64>,
    new_size: Option<u64>,
    /// `new_size` less `old_size`, counting `null` as 0.
    change: i128,
    /// Whether both put the same bytes at the same addresses in the block.
    same_bytes: bool,
    /// How much of `change` the items' bytes, the bytes of sections and
    /// load images that hold no items, and the bytes between them make up:
    /// they add up to `change`, unless an item lies inside another, whose
    /// bytes `item_bytes` then counts twice.
    item_bytes: Option<i128>,
    section_bytes: Option<i128>,
    padding_bytes: Option<i128>,
    /// The sections and load images in the block, in address order.
    sections: Vec<SectionJson<'a>>,
}

#[derive(Serialize)]
struct SectionJson<'a> {
    name: &'a str,
    /// Whether this is the section's load image, rather than the section.
    load_image: bool,
    /// Whether items are in it, in either build.
    holds_items: Option<bool>,
    /// Where it is in each build, and its size, or `null` when that build
    /// doesn't have it in the block.
    old_address: Option<String>,
    old_size: Option<u64>,
    new_address: Option<String>,
    new_size: Option<u64>,
}

#[derive(Serialize)]
struct ItemJson<'a> {
    /// The new build's name, or for a gone item, the old build's.
    name: &'a str,
    /// The old build's name, when the new build names the item otherwise.
    was: Option<&'a str>,
    kind: String,
    /// `moved`, `new`, `gone`, `grew` or `shrank`.
    status: &'static str,
    /// The rule that paired the items: `name`, `bytes`, `uses` or `place`.
    paired_by: Option<PairedBy>,
    /// `new_size` less `old_size`, counting `null` as 0.
    change: i128,
    /// The item in the old build, or `null`s for a new item: where it is,
    /// its size, its section, and the start of its block; the item that
    /// keeps it, its parent in the dominator tree, or `null` when several
    /// of the places where the program starts keep it, when nothing reaches
    /// it, or when elfq can't tell; and its size plus the sizes of the items
    /// that only it keeps, as `top` gives it, or `null` when nothing reaches
    /// it or elfq can't tell.
    old_address: Option<String>,
    old_size: Option<u64>,
    old_section: Option<&'a str>,
    old_block: Option<String>,
    old_kept_by: Option<KeptBy<'a>>,
    old_retained: Option<u64>,
    /// The same for the new build, `null`s for a gone item.
    new_address: Option<String>,
    new_size: Option<u64>,
    new_section: Option<&'a str>,
    new_block: Option<String>,
    new_kept_by: Option<KeptBy<'a>>,
    new_retained: Option<u64>,
    /// Whether elfq estimated the size in either build.
    size_estimated: bool,
    /// For a gone function, the functions that the new build's DWARF says
    /// the compiler inlined it into.
    inlined_into: Option<Vec<&'a str>>,
}

/// An item in one build, for the keys of [`ItemJson`] that start with
/// `old_` or `new_`.
#[derive(Default)]
struct Side<'a> {
    address: Option<String>,
    size: Option<u64>,
    section: Option<&'a str>,
    block: Option<String>,
    kept_by: Option<KeptBy<'a>>,
    retained: Option<u64>,
}

#[derive(Serialize)]
struct KeptBy<'a> {
    name: &'a str,
    address: String,
}

pub(super) fn write(answer: &Answer, out: &mut dyn Write) -> Result<()> {
    let compared = answer.items.as_ref();
    let bytes = |build: &Build| total(build.binary.items.iter().map(|item| item.size));
    let holds_items: HashSet<&str> = compared.map_or_else(HashSet::new, |compared| {
        let items = &compared.items;
        [items.old, items.new]
            .into_iter()
            .flat_map(|build| {
                let binary = &build.binary;
                binary
                    .items
                    .iter()
                    .map(|item| binary.sections[item.section].name.as_str())
            })
            .collect()
    });
    let diff = DiffJson {
        old: answer.old_path.display().to_string(),
        new: answer.new_path.display().to_string(),
        same_bytes: answer.same_memory,
        items_compared: compared.is_some(),
        old_bytes: compared.map(|compared| bytes(compared.items.old)),
        new_bytes: compared.map(|compared| bytes(compared.items.new)),
        unchanged: compared.map(|compared| compared.items.unchanged),
        sections_that_differ: answer
            .sections_that_differ
            .iter()
            .map(|differs| differs.name)
            .collect(),
        blocks: answer
            .blocks
            .iter()
            .map(|block| block_json(answer, block, &holds_items))
            .collect(),
        items: ItemsJson::new(compared, &answer.blocks),
    };
    json::write(&diff, out)
}

fn block_json<'a>(
    answer: &Answer<'a>,
    block: &Block,
    holds_items: &HashSet<&str>,
) -> BlockJson<'a> {
    let place_address = |place: Option<Place>| place.map(|place| address(place.address));
    let place_size = |place: Option<Place>| place.map(|place| place.size);
    let parts = answer.items.as_ref().map(|compared| {
        let items = &compared.items;
        let old = block.parts(true, answer.old, &items.old.binary, holds_items);
        let new = block.parts(false, answer.new, &items.new.binary, holds_items);
        let change = |old: u64, new: u64| i128::from(new) - i128::from(old);
        (
            change(old.items, new.items),
            change(old.sections, new.sections),
            change(old.padding, new.padding),
        )
    });
    BlockJson {
        range: json::Range::new(block.range),
        old_size: block.old_size(),
        new_size: block.new_size(),
        change: block.change(),
        same_bytes: block.same_bytes,
        item_bytes: parts.map(|parts| parts.0),
        section_bytes: parts.map(|parts| parts.1),
        padding_bytes: parts.map(|parts| parts.2),
        sections: block
            .sections(answer.old, answer.new)
            .into_iter()
            .map(|section| SectionJson {
                name: section.name,
                load_image: section.load_image,
                holds_items: answer
                    .items
                    .as_ref()
                    .map(|_| !section.load_image && holds_items.contains(section.name)),
                old_address: place_address(section.old),
                old_size: place_size(section.old),
                new_address: place_address(section.new),
                new_size: place_size(section.new),
            })
            .collect(),
    }
}

/// A row for each item that changed, largest change first, and of the
/// same change, by name, each made as it is written, since a pair of large
/// programs has hundreds of thousands.
struct ItemsJson<'c, 'a> {
    compared: Option<&'c Compared<'a>>,
    blocks: &'c [Block],
    /// The changes, in the order of the rows.
    order: Vec<usize>,
    /// Where the compiler inlined each gone function (see
    /// [`Items::inlined_into`](super::rows::Items::inlined_into)).
    inlined_into: HashMap<usize, Vec<usize>>,
}

impl<'c, 'a> ItemsJson<'c, 'a> {
    fn new(compared: Option<&'c Compared<'a>>, blocks: &'c [Block]) -> ItemsJson<'c, 'a> {
        let (order, inlined_into) = compared.map_or_else(Default::default, |compared| {
            let items = &compared.items;
            let mut order: Vec<usize> = (0..items.changes.len()).collect();
            order.sort_by_cached_key(|&at| {
                let change = &items.changes[at];
                (
                    Reverse(items.change(change).unsigned_abs()),
                    items.item(change).name.as_str(),
                )
            });
            let gone = items
                .changes
                .iter()
                .filter(|change| change.status == Status::Gone)
                .filter_map(|change| change.old);
            (order, items.inlined_into(&compared.new_names, gone))
        });
        ItemsJson {
            compared,
            blocks,
            order,
            inlined_into,
        }
    }

    /// The row for a change.
    fn row(&self, compared: &Compared<'a>, change: &Change) -> ItemJson<'a> {
        let items = &compared.items;
        let item = items.item(change);
        let side = |build: &'a Build, index: Option<usize>, item_blocks: &[Option<usize>]| {
            index.map_or_else(Side::default, |index| {
                side(
                    build,
                    index,
                    item_blocks[index].map(|block| &self.blocks[block]),
                )
            })
        };
        let (old, new) = (
            side(items.old, change.old, &items.old_blocks),
            side(items.new, change.new, &items.new_blocks),
        );
        let estimated = |build: &Build, index: Option<usize>| {
            index.is_some_and(|index| build.binary.items[index].size_estimated)
        };
        let new_binary: &'a Binary = &items.new.binary;
        ItemJson {
            name: &item.name,
            was: items.was(change),
            kind: item.kind.to_string(),
            status: change.status.word(),
            paired_by: items.paired_by(change),
            change: items.change(change),
            old_address: old.address,
            old_size: old.size,
            old_section: old.section,
            old_block: old.block,
            old_kept_by: old.kept_by,
            old_retained: old.retained,
            new_address: new.address,
            new_size: new.size,
            new_section: new.section,
            new_block: new.block,
            new_kept_by: new.kept_by,
            new_retained: new.retained,
            size_estimated: estimated(items.old, change.old) || estimated(items.new, change.new),
            inlined_into: change
                .old
                .and_then(|index| self.inlined_into.get(&index))
                .map(|into| {
                    into.iter()
                        .map(|&index| new_binary.items[index].name.as_str())
                        .collect()
                }),
        }
    }
}

impl Serialize for ItemsJson<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut rows = serializer.serialize_seq(Some(self.order.len()))?;
        if let Some(compared) = self.compared {
            for &at in &self.order {
                rows.serialize_element(&self.row(compared, &compared.items.changes[at]))?;
            }
        }
        rows.end()
    }
}

/// The item at `index` in `build`, in `block`.
fn side<'a>(build: &'a Build, index: usize, block: Option<&Block>) -> Side<'a> {
    let binary = &build.binary;
    let item = &binary.items[index];
    let node = Node::Item(index);
    let tree = build.tree.as_ref();
    let kept_by = match tree.and_then(|tree| tree.parent(node)) {
        Some(Node::Item(parent)) => Some(KeptBy {
            name: &binary.items[parent].name,
            address: address(binary.items[parent].address),
        }),
        _ => None,
    };
    Side {
        address: Some(address(item.address)),
        size: (item.size > 0).then_some(item.size),
        section: Some(&binary.sections[item.section].name),
        block: block.map(|block| address(block.range.start)),
        kept_by,
        retained: tree
            .filter(|tree| tree.is_reachable(node))
            .map(|tree| tree.retained(node)),
    }
}
