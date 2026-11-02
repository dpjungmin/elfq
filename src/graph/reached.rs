//! What one item reaches through the graph, added up: one search that
//! keeps no paths and only counts bytes, which is quick enough to make for
//! every start of a large program, as `reach` without SYMBOL does.

use super::{Graph, Node};
use crate::binary::{Binary, CodeAndData, MemoryBlock};

/// What an item reaches, added up as `reach SYMBOL` counts it, but found
/// without the tree.
pub(crate) struct Reached {
    /// The bytes of the item and of what it reaches.
    pub(crate) bytes: CodeAndData,
    /// The same, in each of the blocks of memory that [`Reached::find`] was
    /// given.
    pub(crate) blocks: Vec<CodeAndData>,
    /// How many items other than itself it reaches.
    pub(crate) count: usize,
    /// Whether the item or what it reaches has a call or jump that elfq
    /// can't follow, but for the jump in a PLT's header to the dynamic
    /// loader.
    pub(crate) unfollowed: bool,
    /// Whether it is, or reaches, a function whose end elfq doesn't know,
    /// so that it can't read its code.
    pub(crate) unreadable: bool,
}

impl Reached {
    /// Adds up what the item at `index` reaches in `graph`, the file's
    /// reference graph, in all and in each of `blocks`.
    pub(crate) fn find(
        binary: &Binary,
        graph: &Graph,
        index: usize,
        blocks: &[MemoryBlock],
    ) -> Reached {
        let mut reached = Reached {
            bytes: CodeAndData::default(),
            blocks: vec![CodeAndData::default(); blocks.len()],
            count: 0,
            unfollowed: false,
            unreadable: false,
        };
        let mut seen = vec![false; binary.items.len()];
        seen[index] = true;
        let mut next = vec![index];
        while let Some(index) = next.pop() {
            let item = &binary.items[index];
            reached.bytes.add(item);
            if let Some(block) = blocks
                .iter()
                .position(|block| block.range.contains(item.address))
            {
                reached.blocks[block].add(item);
            }
            if item.end_unknown() {
                reached.unreadable = true;
            }
            if !graph.unresolved_in(index).is_empty() && !graph.is_plt_header(Node::Item(index)) {
                reached.unfollowed = true;
            }
            // Imports use nothing in the file, and take no bytes in it.
            for reference in graph.references_from(index) {
                if let Node::Item(to) = reference.to
                    && !seen[to]
                {
                    seen[to] = true;
                    reached.count += 1;
                    next.push(to);
                }
            }
        }
        reached
    }

    /// Whether elfq can follow every call and jump in the item and what it
    /// reaches, and knows where each function among them ends, as
    /// `complete` in `reach SYMBOL --json` says.
    pub(crate) fn complete(&self) -> bool {
        !self.unfollowed && !self.unreadable
    }
}
