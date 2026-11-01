//! The JSON answer of `reach SYMBOL`: what SYMBOL reaches, as a tree.

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::{CantFollow, Reach};
use crate::commands::{json, range_check};
use crate::graph::Node;

#[derive(Serialize)]
struct ReachJson<'a> {
    symbol: json::Symbol<'a>,
    /// The ranges of `--to`, or `null`.
    to: Option<Vec<json::Range>>,
    /// The bytes of the functions among SYMBOL and what it reaches, or with
    /// `--to`, among those in its ranges.
    code_bytes: u64,
    /// The bytes of the data objects, counted the same way.
    data_bytes: u64,
    /// Every block of memory that the overview's `memory` lists, with the
    /// bytes in it that SYMBOL and what it reaches take, counted the same
    /// way.
    blocks: Vec<BlockJson<'a>>,
    /// SYMBOL, then what it reaches, as the rows of a tree: each item comes
    /// after the item that uses it on its chain from SYMBOL, its `parent`.
    /// With `--to`, only the items that the tree draws (see
    /// [`Reach::in_answer`]).
    items: Vec<ItemJson<'a>>,
    /// The calls and jumps in what SYMBOL reaches that elfq can't follow,
    /// or with `--to`, those that may go to its ranges.
    unresolved: Vec<UnresolvedJson<'a>>,
    /// The functions that SYMBOL reaches whose code elfq can't read, since
    /// it doesn't know where they end.
    unreadable: Vec<json::Item<'a>>,
    /// Whether `items` has everything SYMBOL reaches, or with `--to`, in its
    /// ranges, as far as elfq can tell: `false` when `unreadable` isn't
    /// empty, or `unresolved` is not, other than with `--to`, for calls to
    /// known addresses in its ranges.
    complete: bool,
}

#[derive(Serialize)]
struct BlockJson<'a> {
    #[serde(flatten)]
    range: json::Range,
    code_bytes: u64,
    data_bytes: u64,
    /// The sections in the block that hold SYMBOL or what it reaches, in
    /// address order.
    sections: Vec<&'a str>,
}

#[derive(Serialize)]
struct ItemJson<'a> {
    /// Which row this is, counting from 0, which is SYMBOL's.
    id: usize,
    #[serde(flatten)]
    item: json::Item<'a>,
    /// The `start` of the block that holds it, or `null` for an import, or
    /// for an item in no block, as a damaged file can have.
    block: Option<String>,
    /// How its parent uses it, as in `callees`, or `null` for SYMBOL.
    how: Option<&'static str>,
    /// Where its parent uses it that way.
    sites: Vec<String>,
    /// Whether it is a function that nothing SYMBOL reaches calls, so that
    /// it is there because something takes or stores its address. A pointer
    /// in a vector table counts as a call.
    through_pointer: bool,
    /// Whether it has a byte in the ranges of `--to`, or `null` without
    /// `--to`. The rows where it is `false` lead to those where it isn't,
    /// or to the calls and jumps in `unresolved`.
    in_to: Option<bool>,
    /// The `id` of the row of the item that uses it on its chain from
    /// SYMBOL, or `null` for SYMBOL.
    parent: Option<usize>,
}

#[derive(Serialize)]
struct UnresolvedJson<'a> {
    /// The function it is in.
    caller: json::Item<'a>,
    #[serde(flatten)]
    call: json::Unresolved,
}

impl Reach<'_, '_> {
    pub(super) fn write_json(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let item = |node: Node| json::Item::new(binary, node);
        let counted = self.counted();
        let bytes = self.bytes(counted.iter().copied());
        let blocks = self
            .blocks
            .iter()
            .zip(self.blocks_reached(&counted))
            .map(|(block, (bytes, sections))| BlockJson {
                range: json::Range::new(block.range),
                code_bytes: bytes.code,
                data_bytes: bytes.data,
                sections,
            })
            .collect();
        let ids = self.json_ids();
        let items = self
            .steps
            .iter()
            .zip(&ids)
            .filter_map(|(step, &id)| {
                Some(ItemJson {
                    id: id?,
                    item: item(step.node),
                    block: step
                        .block
                        .map(|block| json::address(self.blocks[block].range.start)),
                    how: step.via.map(|via| json::how_field(via.kind)),
                    sites: json::addresses(&step.sites),
                    through_pointer: step.through_pointer,
                    in_to: (!self.to.is_empty()).then(|| self.in_to(step)),
                    parent: step
                        .parent
                        .map(|parent| ids[parent].expect("a row's parent has one")),
                })
            })
            .collect();
        // With `--to`, a call to a known address in its ranges, as to a
        // chip's ROM, is a whole answer, as `callees --to` counts it: only
        // a call through a pointer leaves it open.
        let known_target_in_to =
            |cant: &CantFollow| !self.to.is_empty() && cant.found.target.is_some();
        let answer = ReachJson {
            symbol: json::Symbol::new(binary, Node::Item(self.symbol)),
            to: range_check::json_far_side(&self.to),
            code_bytes: bytes.code,
            data_bytes: bytes.data,
            blocks,
            items,
            unresolved: (self.cant_follow.iter())
                .map(|cant| UnresolvedJson {
                    caller: item(Node::Item(cant.found.from)),
                    call: json::Unresolved::new(&cant.found),
                })
                .collect(),
            unreadable: json::items(binary, &self.unreadable),
            complete: self.cant_follow.iter().all(known_target_in_to) && self.unreadable.is_empty(),
        };
        json::write(&answer, out)
    }

    /// The `id` of each step's row in the JSON, for the steps the answer
    /// has (see [`Reach::in_answer`]).
    fn json_ids(&self) -> Vec<Option<usize>> {
        let mut ids = 0..;
        self.in_answer()
            .into_iter()
            .map(|in_answer| if in_answer { ids.next() } else { None })
            .collect()
    }
}
