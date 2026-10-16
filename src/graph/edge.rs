//! The edge of some ranges of memory, such as a microcontroller's ITCM:
//! the references that cross it, from the items in the ranges to the rest
//! of the file, or back.
//!
//! An item is in the ranges when any of its bytes are, and an import never
//! is. A reference crosses the edge when one end is in the ranges and the
//! other isn't, such as a function in ITCM that calls one in flash.
//!
//! When a call is too far for the call instruction, the linker adds a
//! veneer near it, which jumps on to the target. Such a call crosses the
//! edge in the veneer, but it is reported at the call to the veneer, which
//! is in the code someone wrote, with the veneer beside it.

use crate::arch;
use crate::binary::{AddressRange, Binary, ItemKind};

use super::{Graph, Node, Reference, ReferenceKind, RootKind, Unresolved};

/// One reference across the edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crossing {
    /// The item or import outside the ranges.
    pub outside: Node,
    /// The item in the ranges, as an index into [`Binary::items`].
    pub inside: usize,
    /// How the item that makes the reference uses the other one: for a
    /// call through a veneer, how it uses the veneer.
    pub kind: ReferenceKind,
    /// Where the reference is.
    pub site: u64,
    /// The veneer that a call goes through, as an index into
    /// [`Binary::items`].
    pub veneer: Option<usize>,
}

/// The edge of some ranges of memory.
pub struct Edge<'a> {
    binary: &'a Binary,
    graph: &'a Graph,
    ranges: &'a [AddressRange],
    /// For each item, whether it is in the ranges.
    inside: Vec<bool>,
}

impl<'a> Edge<'a> {
    pub fn new(binary: &'a Binary, graph: &'a Graph, ranges: &'a [AddressRange]) -> Edge<'a> {
        let inside = binary
            .items
            .iter()
            .map(|item| {
                ranges
                    .iter()
                    .any(|range| range.overlap(item.address, item.size.max(1)) > 0)
            })
            .collect();
        Edge {
            binary,
            graph,
            ranges,
            inside,
        }
    }

    /// A range that no item is in, if there is one: most likely a typo.
    pub fn empty_range(&self) -> Option<AddressRange> {
        self.ranges.iter().copied().find(|&range| {
            !self
                .binary
                .items
                .iter()
                .any(|item| range.overlap(item.address, item.size.max(1)) > 0)
        })
    }

    /// The references from the items in the ranges to the rest of the file,
    /// in the order of the items that make them.
    pub fn leaving(&self) -> Vec<Crossing> {
        self.crossings(true)
    }

    /// The references from the rest of the file to the items in the ranges,
    /// in the order of the items that make them.
    pub fn entering(&self) -> Vec<Crossing> {
        self.crossings(false)
    }

    /// The calls and jumps in the ranges that elfq can't follow, and that
    /// may leave them: through a pointer, or to code outside every item and
    /// outside the ranges.
    pub fn unfollowed(&self) -> Vec<Unresolved> {
        self.graph
            .unresolved
            .iter()
            .filter(|unresolved| self.inside[unresolved.from])
            .filter(|unresolved| {
                unresolved.target.is_none_or(|target| {
                    !self.ranges.iter().any(|range| range.overlap(target, 1) > 0)
                })
            })
            .copied()
            .collect()
    }

    /// The functions in the ranges whose code elfq can't read, since it
    /// doesn't know where they end.
    pub fn unreadable(&self) -> Vec<usize> {
        self.functions_inside()
            .filter(|&index| self.binary.items[index].size == 0)
            .collect()
    }

    /// The functions in the ranges whose address is taken or stored, so
    /// that code outside them may call them through a pointer, which elfq
    /// can't see. A pointer in the vector table doesn't count: only the
    /// processor calls through it.
    pub fn callable_through_pointer(&self) -> Vec<usize> {
        let vector_table: Vec<Node> = self
            .graph
            .roots
            .iter()
            .filter(|root| root.kind == RootKind::VectorTable)
            .map(|root| root.node)
            .collect();
        self.functions_inside()
            .filter(|&index| {
                self.graph
                    .references_to(Node::Item(index))
                    .any(|reference| match reference.kind {
                        ReferenceKind::Address => true,
                        ReferenceKind::Pointer => {
                            !vector_table.contains(&Node::Item(reference.from))
                        }
                        _ => false,
                    })
            })
            .collect()
    }

    fn functions_inside(&self) -> impl Iterator<Item = usize> {
        (0..self.binary.items.len()).filter(|&index| {
            self.inside[index] && self.binary.items[index].kind == ItemKind::Function
        })
    }

    fn contains(&self, node: Node) -> bool {
        match node {
            Node::Item(index) => self.inside[index],
            Node::Import(_) => false,
        }
    }

    /// The references across the edge that the items on one side make:
    /// the items in the ranges when `from_inside`, or else the rest.
    fn crossings(&self, from_inside: bool) -> Vec<Crossing> {
        let mut crossings = Vec::new();
        for reference in &self.graph.references {
            if self.contains(Node::Item(reference.from)) != from_inside
                || self.contains(reference.to) == from_inside
            {
                continue;
            }
            // The calls through the reference when it is a veneer's jump,
            // or else the reference itself.
            let through_veneer = self.calls_through_veneer(reference.from);
            let veneer = (!through_veneer.is_empty()).then_some(reference.from);
            let calls = if through_veneer.is_empty() {
                vec![reference]
            } else {
                through_veneer
            };
            for call in calls {
                let (inside, outside) = match (from_inside, reference.to) {
                    (true, _) => (call.from, reference.to),
                    (false, Node::Item(to)) => (to, Node::Item(call.from)),
                    // An import is never in the ranges.
                    (false, Node::Import(_)) => continue,
                };
                crossings.push(Crossing {
                    outside,
                    inside,
                    kind: call.kind,
                    site: call.site,
                    veneer,
                });
            }
        }
        crossings
    }

    /// When `item` is a veneer, the calls to it from its own side of the
    /// edge, so that the jump it makes across stands for them. Otherwise,
    /// or when nothing on its side calls it, nothing.
    fn calls_through_veneer(&self, item: usize) -> Vec<&'a Reference> {
        if !is_veneer(self.binary, self.graph, item) {
            return Vec::new();
        }
        let side = self.inside[item];
        self.graph
            .references_to(Node::Item(item))
            .filter(|reference| {
                matches!(
                    reference.kind,
                    ReferenceKind::Call | ReferenceKind::TailCall
                ) && self.inside[reference.from] == side
            })
            .collect()
    }
}

/// Whether `item` is a linker's veneer: its code is all a veneer (see
/// [`arch::is_veneer`]), and jumps to one function.
fn is_veneer(binary: &Binary, graph: &Graph, item: usize) -> bool {
    let function = &binary.items[item];
    function.kind == ItemKind::Function
        && binary
            .bytes(function.address, function.size)
            .is_some_and(|code| arch::is_veneer(binary.architecture, code))
        && matches!(
            graph.references_from(item),
            [reference] if reference.kind == ReferenceKind::TailCall
        )
}
