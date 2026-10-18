//! The boundary of some ranges of memory, such as a microcontroller's
//! ITCM: the references that cross it, from the items in the ranges to the
//! rest of the file, or back. Help for `callers` and `callees` calls it the
//! ranges' edge, but in a graph an edge is a reference.
//!
//! An item is in the ranges when any of its bytes are, and an import never
//! is. A reference crosses the boundary when one end is in the ranges and
//! the other isn't, such as a function in ITCM that calls one in flash.
//!
//! When a call is too far for the call instruction, the linker adds a
//! veneer, which jumps on to the target. A call through a veneer crosses
//! the boundary either at the call or at the veneer's jump, and either way
//! it is reported as a call to the target from the code someone wrote, at
//! the call to the veneer, with the veneer beside it. Only when the caller
//! and the target are on one side and the veneer is on the other do both
//! hops cross; then each is reported as it is.

use crate::arch;
use crate::binary::{AddressRange, Binary, ItemKind};

use super::{Graph, Node, Reference, ReferenceKind, RootKind, Unresolved};

/// One use across the boundary: `from` uses `to`, directly or through a
/// veneer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crossing {
    /// The item that makes the reference, such as the caller, as an index
    /// into [`Binary::items`].
    pub from: usize,
    /// What it uses: for a call through a veneer, the veneer's target.
    pub to: Node,
    /// How `from` uses `to`: for a call through a veneer, how it uses the
    /// veneer.
    pub kind: ReferenceKind,
    /// Where the reference is: for a call through a veneer, the call.
    pub site: u64,
    /// The veneer that a call goes through, as an index into
    /// [`Binary::items`].
    pub veneer: Option<usize>,
}

/// The boundary of some ranges of memory.
pub struct Boundary<'a> {
    binary: &'a Binary,
    graph: &'a Graph,
    ranges: &'a [AddressRange],
    /// For each item, whether it is in the ranges.
    inside: Vec<bool>,
}

impl<'a> Boundary<'a> {
    pub fn new(binary: &'a Binary, graph: &'a Graph, ranges: &'a [AddressRange]) -> Boundary<'a> {
        let inside = binary
            .items
            .iter()
            .map(|item| {
                ranges
                    .iter()
                    .any(|range| range.overlap(item.address, item.size.max(1)) > 0)
            })
            .collect();
        Boundary {
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

    /// The uses of the rest of the file by the items in the ranges.
    pub fn leaving(&self) -> Vec<Crossing> {
        self.crossings(true)
    }

    /// The uses of the items in the ranges by the rest of the file.
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
        let vector_tables: Vec<Node> = self
            .graph
            .roots
            .iter()
            .filter(|root| root.kind == RootKind::VectorTable)
            .map(|root| root.node)
            .collect();
        let mut taken = vec![false; self.binary.items.len()];
        for reference in &self.graph.references {
            let counts = match reference.kind {
                ReferenceKind::Address => true,
                ReferenceKind::Pointer => !vector_tables.contains(&Node::Item(reference.from)),
                _ => false,
            };
            if counts && let Node::Item(index) = reference.to {
                taken[index] = true;
            }
        }
        self.functions_inside()
            .filter(|&index| taken[index])
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

    /// The uses across the boundary by the items on one side of it: the items
    /// in the ranges when `from_inside`, or else the rest.
    fn crossings(&self, from_inside: bool) -> Vec<Crossing> {
        let mut crossings = Vec::new();
        for reference in &self.graph.references {
            if self.inside[reference.from] != from_inside
                || self.contains(reference.to) == from_inside
            {
                continue;
            }
            let crossing = |used: &Reference, veneer: Option<usize>, to: Node| Crossing {
                from: used.from,
                to,
                kind: used.kind,
                site: used.site,
                veneer,
            };
            let calls = self.calls_from_its_side(reference.from);
            if !calls.is_empty() {
                // A veneer's jump across: the calls to the veneer take its
                // place.
                for call in calls {
                    crossings.push(crossing(call, Some(reference.from), reference.to));
                }
            } else if reference.kind.is_call()
                && let Node::Item(veneer) = reference.to
                && let Some(target) = self.veneer_target(veneer)
                && self.contains(target) != from_inside
            {
                // A call across to a veneer, which jumps on without
                // crossing back.
                crossings.push(crossing(reference, Some(veneer), target));
            } else {
                crossings.push(crossing(reference, None, reference.to));
            }
        }
        crossings
    }

    /// When `item` is a veneer, the calls to it from its own side of the
    /// boundary, which take the place of its jump to the other side. Otherwise,
    /// or when nothing on its side calls it, nothing.
    fn calls_from_its_side(&self, item: usize) -> Vec<&'a Reference> {
        if self.veneer_target(item).is_none() {
            return Vec::new();
        }
        let side = self.inside[item];
        self.graph
            .references_to(Node::Item(item))
            .filter(|reference| reference.kind.is_call() && self.inside[reference.from] == side)
            .collect()
    }

    /// Where `item` jumps to, when it is a linker's veneer: its code is all
    /// a veneer (see [`arch::is_veneer`]), and jumps to one function.
    fn veneer_target(&self, item: usize) -> Option<Node> {
        let function = &self.binary.items[item];
        let code = self.binary.bytes(function.address, function.size)?;
        if function.kind != ItemKind::Function || !arch::is_veneer(self.binary.architecture, code) {
            return None;
        }
        match self.graph.references_from(item) {
            [reference] if reference.kind == ReferenceKind::TailCall => Some(reference.to),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// The Cortex-M7 sample, with code in ITCM at 0x0 and flash at
    /// 0x30000000, and the veneers GNU ld added between them.
    fn sample() -> (Binary, Graph) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-cortex-m7.elf");
        let binary = Binary::load(&path).unwrap();
        let graph = Graph::build(&binary);
        (binary, graph)
    }

    fn range(text: &str) -> AddressRange {
        text.parse().unwrap()
    }

    fn index(binary: &Binary, name: &str) -> usize {
        binary
            .items
            .iter()
            .position(|item| item.name == name)
            .unwrap()
    }

    /// The crossings as (from, to, veneer) names.
    fn names(binary: &Binary, crossings: &[Crossing]) -> Vec<(String, String, String)> {
        let mut names: Vec<_> = crossings
            .iter()
            .map(|crossing| {
                (
                    binary.items[crossing.from].name.clone(),
                    crossing.to.name(binary).to_string(),
                    crossing
                        .veneer
                        .map_or(String::new(), |veneer| binary.items[veneer].name.clone()),
                )
            })
            .collect();
        names.sort();
        names
    }

    fn crossing(from: &str, to: &str, veneer: &str) -> (String, String, String) {
        (from.to_string(), to.to_string(), veneer.to_string())
    }

    #[test]
    fn a_call_through_a_veneer_is_a_call_to_its_target() {
        let (binary, graph) = sample();
        // The veneers are in ITCM, so the call crosses at their jumps.
        let ranges = [range("0x0+256K"), range("0x20000000+256K")];
        let boundary = Boundary::new(&binary, &graph, &ranges);
        assert_eq!(
            names(&binary, &boundary.leaving()),
            [
                crossing("SysTick_Handler", "app_tick", "__app_tick_veneer"),
                crossing("apply_filter", "memcpy", "__memcpy_veneer"),
                crossing("filter_tap", "fir_coeffs", ""),
            ]
        );
        // Without them, the call crosses at the call to the veneer.
        let ranges = [range("0x0..0xd0"), range("0x20000000+256K")];
        let boundary = Boundary::new(&binary, &graph, &ranges);
        assert_eq!(
            names(&binary, &boundary.leaving()),
            [
                crossing("SysTick_Handler", "app_tick", "__app_tick_veneer"),
                crossing("apply_filter", "memcpy", "__memcpy_veneer"),
                crossing("filter_tap", "fir_coeffs", ""),
            ]
        );
        // The veneers alone: the caller and the target are both outside,
        // so both hops cross.
        let ranges = [range("0xd0..0xe0")];
        let boundary = Boundary::new(&binary, &graph, &ranges);
        assert_eq!(
            names(&binary, &boundary.leaving()),
            [
                crossing("__app_tick_veneer", "app_tick", ""),
                crossing("__memcpy_veneer", "memcpy", ""),
            ]
        );
        assert_eq!(
            names(&binary, &boundary.entering()),
            [
                crossing("SysTick_Handler", "__app_tick_veneer", ""),
                crossing("apply_filter", "__memcpy_veneer", ""),
            ]
        );
    }

    #[test]
    fn a_veneer_only_jumps_to_one_function() {
        let (binary, mut graph) = sample();
        // Calling on is not what a veneer does.
        let veneer = index(&binary, "__memcpy_veneer");
        let start = graph.references.partition_point(|r| r.from < veneer);
        graph.references[start].kind = ReferenceKind::Call;
        let ranges = [range("0x0+256K"), range("0x20000000+256K")];
        let boundary = Boundary::new(&binary, &graph, &ranges);
        assert!(
            names(&binary, &boundary.leaving()).contains(&crossing(
                "__memcpy_veneer",
                "memcpy",
                ""
            )),
            "{:?}",
            boundary.leaving()
        );
        // Nor is a function a veneer when it only makes one tail call, but
        // its code isn't a veneer's.
        let handler = index(&binary, "SysTick_Handler");
        graph
            .references
            .retain(|r| r.from != handler || r.kind == ReferenceKind::TailCall);
        let boundary = Boundary::new(&binary, &graph, &ranges);
        assert_eq!(graph.references_from(handler).len(), 1);
        assert_eq!(boundary.veneer_target(handler), None);
    }

    #[test]
    fn only_calls_that_may_leave_the_ranges_are_unfollowed() {
        let (binary, mut graph) = sample();
        let from = index(&binary, "crc32");
        let unresolved = |target| Unresolved {
            from,
            site: 0x18,
            is_call: true,
            target,
        };
        graph.unresolved = vec![
            unresolved(None),
            unresolved(Some(0x100)),
            unresolved(Some(0x1fff_0000)),
        ];
        let ranges = [range("0x0+256K")];
        let boundary = Boundary::new(&binary, &graph, &ranges);
        let targets: Vec<Option<u64>> = boundary.unfollowed().iter().map(|u| u.target).collect();
        assert_eq!(targets, [None, Some(0x1fff_0000)]);
        // A call through a pointer from flash, outside the range, isn't
        // one.
        graph.unresolved[0].from = index(&binary, "process_frame");
        let boundary = Boundary::new(&binary, &graph, &ranges);
        let targets: Vec<Option<u64>> = boundary.unfollowed().iter().map(|u| u.target).collect();
        assert_eq!(targets, [Some(0x1fff_0000)]);
    }

    #[test]
    fn the_vector_table_is_not_a_pointer_that_code_calls_through() {
        let (binary, mut graph) = sample();
        // SysTick_Handler is at 0x0, and only the vector table holds its
        // address.
        let ranges = [range("0x0+0x10")];
        let boundary = Boundary::new(&binary, &graph, &ranges);
        assert!(boundary.callable_through_pointer().is_empty());
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
        let boundary = Boundary::new(&binary, &graph, &ranges);
        assert_eq!(boundary.callable_through_pointer(), [handler]);
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
        let boundary = Boundary::new(&binary, &graph, &ranges);
        assert_eq!(boundary.callable_through_pointer(), [handler]);
    }
}
