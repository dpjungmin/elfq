//! The items in some ranges of memory, such as a microcontroller's ITCM,
//! and two things elfq can't see about them: which functions it can't
//! read, and which ones code may call through a pointer.
//!
//! An item is in the ranges when any of its bytes are, and an import never
//! is. [`Boundary`](super::Boundary) builds on this to find the uses that
//! cross the ranges' edge; a command that only asks about the items needs
//! nothing more.

use crate::binary::{AddressRange, Binary, ItemKind};

use super::{Graph, Node, ReferenceKind, RootKind};

/// The items in some ranges of memory.
pub struct ItemsInRanges<'a> {
    binary: &'a Binary,
    /// The ranges as given: an item with any byte in one of them is inside.
    ranges: &'a [AddressRange],
    /// For each item, whether it is in the ranges.
    inside: Vec<bool>,
}

impl<'a> ItemsInRanges<'a> {
    /// Works out which items are in `ranges`: those with any byte in one of
    /// them, where an item of unknown size counts as its first byte.
    pub fn new(binary: &'a Binary, ranges: &'a [AddressRange]) -> ItemsInRanges<'a> {
        let inside = binary.items.iter().map(|item| item.is_in(ranges)).collect();
        ItemsInRanges {
            binary,
            ranges,
            inside,
        }
    }

    /// Whether the item at `index`, an index into [`Binary::items`], is in
    /// the ranges.
    pub fn contains_item(&self, index: usize) -> bool {
        self.inside[index]
    }

    /// Whether `node` is an item in the ranges. An import never is.
    pub fn contains(&self, node: Node) -> bool {
        match node {
            Node::Item(index) => self.inside[index],
            Node::Import(_) => false,
        }
    }

    /// Whether `address` is in the ranges.
    pub fn contains_address(&self, address: u64) -> bool {
        self.ranges.iter().any(|range| range.contains(address))
    }

    /// The functions in the ranges whose code elfq can't read, since it
    /// doesn't know where they end.
    pub fn unreadable(&self) -> Vec<usize> {
        self.functions()
            .filter(|&index| self.binary.items[index].size == 0)
            .collect()
    }

    /// The functions in the ranges whose address is taken or stored, so
    /// that code outside them may call them through a pointer, which elfq
    /// can't see. A pointer in the vector table doesn't count: only the
    /// processor calls through it. `graph` is the file's graph.
    pub fn callable_through_pointer(&self, graph: &Graph) -> Vec<usize> {
        let vector_tables: Vec<Node> = graph
            .roots
            .iter()
            .filter(|root| root.kind == RootKind::VectorTable)
            .map(|root| root.node)
            .collect();
        let mut taken = vec![false; self.binary.items.len()];
        for reference in &graph.references {
            let counts = match reference.kind {
                ReferenceKind::Address => true,
                ReferenceKind::Pointer => !vector_tables.contains(&Node::Item(reference.from)),
                _ => false,
            };
            if counts && let Node::Item(index) = reference.to {
                taken[index] = true;
            }
        }
        self.functions().filter(|&index| taken[index]).collect()
    }

    /// The functions in the ranges, as indices into [`Binary::items`].
    fn functions(&self) -> impl Iterator<Item = usize> {
        (0..self.binary.items.len()).filter(|&index| {
            self.inside[index] && self.binary.items[index].kind == ItemKind::Function
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::graph::Reference;

    /// The Cortex-M7 sample, with code in ITCM at 0x0 and flash at
    /// 0x30000000.
    fn sample() -> (Binary, Graph) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-cortex-m7.elf");
        let binary = Binary::load(&path).unwrap();
        let graph = Graph::build(&binary);
        (binary, graph)
    }

    fn index(binary: &Binary, name: &str) -> usize {
        binary
            .items
            .iter()
            .position(|item| item.name == name)
            .unwrap()
    }

    #[test]
    fn the_vector_table_is_not_a_pointer_that_code_calls_through() {
        let (binary, mut graph) = sample();
        // SysTick_Handler is at 0x0, and only the vector table holds its
        // address.
        let ranges = ["0x0+0x10".parse().unwrap()];
        let items = ItemsInRanges::new(&binary, &ranges);
        assert!(items.callable_through_pointer(&graph).is_empty());
        // Code that takes its address can pass it on.
        let handler = index(&binary, "SysTick_Handler");
        let start = graph
            .references
            .partition_point(|r| r.from < index(&binary, "main"));
        graph.references[start] = Reference {
            from: index(&binary, "main"),
            to: Node::Item(handler),
            kind: ReferenceKind::Address,
            site: graph.references[start].site,
        };
        assert_eq!(items.callable_through_pointer(&graph), [handler]);
        // So can data that holds it, other than the vector table.
        let (binary, mut graph) = sample();
        let handlers = index(&binary, "handlers");
        graph.references.push(Reference {
            from: handlers,
            to: Node::Item(handler),
            kind: ReferenceKind::Pointer,
            site: binary.items[handlers].address,
        });
        graph.references.sort_by_key(|r| (r.from, r.site));
        let items = ItemsInRanges::new(&binary, &ranges);
        assert_eq!(items.callable_through_pointer(&graph), [handler]);
    }
}
