//! diff's answer as JSON: whether the builds load the same memory, the
//! sections that differ, and a row for each block of memory, with each
//! section and load image in it.
//! The keys that count items are `null`, since diff doesn't compare items
//! yet, as `items_compared` says.

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::Answer;
use super::blocks::{Block, Place};
use crate::commands::json::{self, address};

#[derive(Serialize)]
struct DiffJson<'a> {
    /// The files, as given.
    old: String,
    new: String,
    /// Whether both load the same bytes at the same addresses: exit status
    /// 0 when they do, and 1 when they don't.
    same_bytes: bool,
    /// Whether diff compared the builds' functions and data objects: false,
    /// and the keys below that count them `null`.
    items_compared: bool,
    /// The bytes of all the functions and data objects in each, as the
    /// overview's `bytes`.
    old_bytes: Option<u64>,
    new_bytes: Option<u64>,
    /// How many items are the same.
    unchanged: Option<usize>,
    /// The sections whose address, size, load image or bytes differ, or
    /// that only one build has, in address order, `.tbss` among them: none
    /// when the memory is the same.
    sections_that_differ: &'a [&'a str],
    /// The blocks of memory that the sections and load images of both
    /// occupy, in address order.
    blocks: Vec<BlockJson<'a>>,
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
    /// load images that hold no items, and the bytes between them make up.
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
    /// Whether items are in it.
    holds_items: Option<bool>,
    /// Where it is in each build, and its size, or `null` when that build
    /// doesn't have it in the block.
    old_address: Option<String>,
    old_size: Option<u64>,
    new_address: Option<String>,
    new_size: Option<u64>,
}

pub(super) fn write(answer: &Answer, out: &mut dyn Write) -> Result<()> {
    let diff = DiffJson {
        old: answer.old_path.display().to_string(),
        new: answer.new_path.display().to_string(),
        same_bytes: answer.same_memory,
        items_compared: false,
        old_bytes: None,
        new_bytes: None,
        unchanged: None,
        sections_that_differ: &answer.sections_that_differ,
        blocks: answer
            .blocks
            .iter()
            .map(|block| block_json(answer, block))
            .collect(),
    };
    json::write(&diff, out)
}

fn block_json<'a>(answer: &Answer<'a>, block: &Block) -> BlockJson<'a> {
    let place_address = |place: Option<Place>| place.map(|place| address(place.address));
    let place_size = |place: Option<Place>| place.map(|place| place.size);
    BlockJson {
        range: json::Range::new(block.range),
        old_size: block.old_size(),
        new_size: block.new_size(),
        change: block.change(),
        same_bytes: block.same_bytes,
        item_bytes: None,
        section_bytes: None,
        padding_bytes: None,
        sections: block
            .sections(answer.old, answer.new)
            .into_iter()
            .map(|section| SectionJson {
                name: section.name,
                load_image: section.load_image,
                holds_items: None,
                old_address: place_address(section.old),
                old_size: place_size(section.old),
                new_address: place_address(section.new),
                new_size: place_size(section.new),
            })
            .collect(),
    }
}
