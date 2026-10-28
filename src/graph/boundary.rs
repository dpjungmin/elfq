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
//! the call to the veneer, with the veneer beside it. That holds for a
//! target outside every item too, such as a routine in a microcontroller's
//! ROM, which elfq doesn't read. Only when the caller and the target are on
//! one side and the veneer is on the other do both hops cross; then each is
//! reported as it is.
//!
//! A PLT stub that passes calls on to one of the file's own functions, as
//! `log_msg@plt` does in a shared library, counts as a veneer for that
//! function (see `veneers.rs`). When the stub also jumps to the header that
//! lazy binding uses, that jump is reported as the stub's own.

use std::cell::OnceCell;

use crate::binary::{AddressRange, Binary, ItemKind};

use super::{Graph, Node, Reference, ReferenceKind, RootKind, Unresolved, VeneerTarget, Veneers};

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

/// A call or jump that elfq can't follow across the boundary: through a
/// pointer, or to code outside every item, such as a routine in a
/// microcontroller's ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unfollowed {
    /// The call or jump. For a call through a veneer, the call to the
    /// veneer, with the veneer's target; a tail call is a jump.
    pub unresolved: Unresolved,
    /// The veneer that a call goes through, as an index into
    /// [`Binary::items`].
    pub veneer: Option<usize>,
}

/// The boundary of some ranges of memory.
pub struct Boundary<'a> {
    binary: &'a Binary,
    /// `binary`'s graph, whose roots say where the vector table is.
    graph: &'a Graph,
    /// The ranges as given: an item with any byte in one of them is inside.
    ranges: &'a [AddressRange],
    /// For each item, whether it is in the ranges.
    inside: Vec<bool>,
    /// The veneers, and where each jumps to.
    veneers: Veneers<'a>,
    /// The calls in the graph, by the item they call: worked out the first
    /// time a veneer's jump crosses the boundary, as the calls to the veneer
    /// then take its place.
    calls_to: OnceCell<CallsTo>,
}

impl<'a> Boundary<'a> {
    /// Works out which items are in `ranges`: those with any byte in one of
    /// them, where an item of unknown size counts as its first byte.
    pub fn new(binary: &'a Binary, graph: &'a Graph, ranges: &'a [AddressRange]) -> Boundary<'a> {
        let inside = binary.items.iter().map(|item| item.is_in(ranges)).collect();
        Boundary {
            binary,
            graph,
            ranges,
            inside,
            veneers: Veneers::new(binary, graph),
            calls_to: OnceCell::new(),
        }
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
    /// may leave them: through a pointer, or to code outside every item,
    /// directly or through a veneer. Code outside every item may leave the
    /// ranges even when it is in them, since elfq doesn't read it: it may be
    /// a veneer whose symbol was stripped.
    pub fn unfollowed(&self) -> Vec<Unfollowed> {
        self.unfollowed_from(true, |_| true)
    }

    /// The calls and jumps from the rest of the file into code in the
    /// ranges that is outside every item, such as a routine in a
    /// microcontroller's ROM, directly or through a veneer.
    pub fn unfollowed_entering(&self) -> Vec<Unfollowed> {
        self.unfollowed_from(false, |target| target.is_some_and(|t| self.in_ranges(t)))
    }

    /// The calls and jumps that elfq can't follow from the items on one
    /// side of the boundary, the ranges' side when `from_inside`, whose
    /// target passes `goes`. In place of a veneer's jump, the calls to the
    /// veneer from its own side.
    fn unfollowed_from(
        &self,
        from_inside: bool,
        goes: impl Fn(Option<u64>) -> bool,
    ) -> Vec<Unfollowed> {
        let mut unfollowed = Vec::new();
        for unresolved in &self.graph.unresolved {
            if self.inside[unresolved.from] != from_inside || !goes(unresolved.target) {
                continue;
            }
            let calls = match unresolved.target {
                Some(target) => {
                    self.calls_from_its_side(unresolved.from, VeneerTarget::Outside(target))
                }
                None => Vec::new(),
            };
            if calls.is_empty() {
                unfollowed.push(Unfollowed {
                    unresolved: *unresolved,
                    veneer: None,
                });
            }
            // A veneer's jump: the calls to the veneer take its place.
            for call in calls {
                unfollowed.push(through_veneer(call, unresolved.from, unresolved.target));
            }
        }
        // The calls to a veneer on the other side, which jumps to code on
        // its own side.
        for reference in &self.graph.references {
            if self.inside[reference.from] == from_inside
                && let Some((veneer, target)) = self.call_to_veneer_across(reference)
            {
                unfollowed.push(through_veneer(reference, veneer, Some(target)));
            }
        }
        unfollowed
            .sort_by_key(|unfollowed| (unfollowed.unresolved.from, unfollowed.unresolved.site));
        unfollowed
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

    /// Whether `address` is in the ranges.
    fn in_ranges(&self, address: u64) -> bool {
        self.ranges.iter().any(|range| range.contains(address))
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
            let calls = self.calls_from_its_side(reference.from, VeneerTarget::Node(reference.to));
            if !calls.is_empty() {
                // A veneer's jump across: the calls to the veneer take its
                // place.
                for call in calls {
                    crossings.push(crossing(call, Some(reference.from), reference.to));
                }
            } else if reference.kind.is_call()
                && let Node::Item(veneer) = reference.to
                && let Some(VeneerTarget::Node(target)) = self.veneers.target(veneer)
                && self.contains(target) != from_inside
            {
                // A call across to a veneer, which jumps on without
                // crossing back.
                crossings.push(crossing(reference, Some(veneer), target));
            } else if self.call_to_veneer_across(reference).is_some() {
                // A call to code outside every item: see `unfollowed_from`.
            } else {
                crossings.push(crossing(reference, None, reference.to));
            }
        }
        crossings
    }

    /// When `item` is a veneer that jumps to `target`, the calls to it from
    /// its own side of the boundary, which take the place of that jump.
    /// Otherwise, or when nothing on its side calls it, nothing.
    fn calls_from_its_side(&self, item: usize, target: VeneerTarget) -> Vec<&'a Reference> {
        if self.veneers.target(item) != Some(target) {
            return Vec::new();
        }
        let side = self.inside[item];
        let references = &self.graph.references;
        let calls_to = self
            .calls_to
            .get_or_init(|| CallsTo::new(references, self.binary.items.len()));
        calls_to
            .of(item)
            .iter()
            .map(|&index| &references[index])
            .filter(|reference| self.inside[reference.from] == side)
            .collect()
    }

    /// When `reference` is a call across the boundary to a veneer that
    /// jumps to code outside every item on the veneer's side, the veneer, as
    /// an index into [`Binary::items`], and that code's address.
    fn call_to_veneer_across(&self, reference: &Reference) -> Option<(usize, u64)> {
        let Node::Item(veneer) = reference.to else {
            return None;
        };
        if !reference.kind.is_call() || self.inside[reference.from] == self.inside[veneer] {
            return None;
        }
        match self.veneers.target(veneer)? {
            VeneerTarget::Outside(target) if self.in_ranges(target) == self.inside[veneer] => {
                Some((veneer, target))
            }
            _ => None,
        }
    }
}

/// The calls among some references, by the item they call, so that finding
/// the calls to a veneer doesn't read every reference again for each one.
struct CallsTo {
    /// For each item, where its calls start in `calls`, and after the last
    /// item, where they end.
    starts: Vec<usize>,
    /// The calls, as indices into the references: those to the first item,
    /// then those to the next, each item's in the order of the references.
    calls: Vec<usize>,
}

impl CallsTo {
    /// Sorts the calls in `references` to `items` items by the item they
    /// call, counting the calls to each first, so that it takes one pass
    /// over the references for each step.
    fn new(references: &[Reference], items: usize) -> CallsTo {
        let callee = |reference: &Reference| match reference.to {
            Node::Item(index) if reference.kind.is_call() => Some(index),
            _ => None,
        };
        let mut starts = vec![0; items + 1];
        for index in references.iter().filter_map(callee) {
            starts[index + 1] += 1;
        }
        for item in 0..items {
            starts[item + 1] += starts[item];
        }
        let mut next = starts.clone();
        let mut calls = vec![0; starts[items]];
        for (index, reference) in references.iter().enumerate() {
            if let Some(callee) = callee(reference) {
                calls[next[callee]] = index;
                next[callee] += 1;
            }
        }
        CallsTo { starts, calls }
    }

    /// The calls to `item`, as indices into the references, in their order.
    fn of(&self, item: usize) -> &[usize] {
        &self.calls[self.starts[item]..self.starts[item + 1]]
    }
}

/// `call`, a call to `veneer`, as a call or jump that elfq can't follow to
/// `target`.
fn through_veneer(call: &Reference, veneer: usize, target: Option<u64>) -> Unfollowed {
    Unfollowed {
        unresolved: Unresolved {
            from: call.from,
            site: call.site,
            is_call: call.kind == ReferenceKind::Call,
            target,
        },
        veneer: Some(veneer),
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
    fn calls_that_may_leave_the_ranges_are_unfollowed() {
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
        // Each may leave: code outside every item, even in the range, may
        // be a veneer whose symbol was stripped.
        let ranges = [range("0x0+256K")];
        let boundary = Boundary::new(&binary, &graph, &ranges);
        let targets: Vec<Option<u64>> = boundary
            .unfollowed()
            .iter()
            .map(|u| u.unresolved.target)
            .collect();
        assert_eq!(targets, [None, Some(0x100), Some(0x1fff_0000)]);
        // A call from flash, outside the range, isn't one.
        for unresolved in &mut graph.unresolved[..2] {
            unresolved.from = index(&binary, "process_frame");
        }
        let boundary = Boundary::new(&binary, &graph, &ranges);
        let targets: Vec<Option<u64>> = boundary
            .unfollowed()
            .iter()
            .map(|u| u.unresolved.target)
            .collect();
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
