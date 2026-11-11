//! The boundary of some ranges of memory, such as a microcontroller's
//! ITCM: the references that cross it, from the items in the ranges to the
//! rest of the file, or back. Help for `callers` and `callees` calls it the
//! ranges' edge, but in a graph an edge is a reference.
//!
//! An item is in the ranges when any of its bytes are, and an import never
//! is (see `items_in_ranges.rs`). The items of a set, which SYMBOLs and
//! patterns pick wherever they lie, count as in them too. A reference
//! crosses the boundary when one end is in the ranges and the other isn't,
//! such as a function in ITCM that calls one in flash. With `--to` or
//! `--from`, only the uses whose far end is in other ranges count:
//! [`Boundary::leaving_to`] and [`Boundary::entering_from`] narrow the
//! crossings to them.
//!
//! When a call is too far for the call instruction, the linker adds a
//! veneer, which jumps on to the target. A call through a veneer crosses
//! the boundary either at the call or at the veneer's jump, and either way
//! it is reported as a call to the target from the code someone wrote, at
//! the call to the veneer, with the veneer beside it. That holds for a
//! target outside every item too, such as a routine in a microcontroller's
//! ROM, which elfq doesn't read. Only when the caller and the target are on
//! one side and the veneer is on the other do both hops cross; then each is
//! reported as it is. [`Boundary::report`] holds these rules.
//!
//! A PLT stub that passes calls on to one of the file's own functions, as
//! `log_msg@plt` does in a shared library, counts as a veneer for that
//! function (see `veneers.rs`). When the stub also jumps to the header that
//! lazy binding uses, that jump is reported as the stub's own.

use std::cell::OnceCell;

use crate::binary::{AddressRange, Binary};

use super::{
    Graph, ItemsInRanges, Node, Reference, ReferenceKind, Unresolved, VeneerTarget, Veneers,
};

/// One use across the boundary: `from` uses `to`, directly or through a
/// veneer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Crossing {
    /// The item that makes the reference, such as the caller, as an index
    /// into [`Binary::items`].
    pub(crate) from: usize,
    /// What it uses: for a call through a veneer, the veneer's target.
    pub(crate) to: Node,
    /// How `from` uses `to`: for a call through a veneer, how it uses the
    /// veneer.
    pub(crate) kind: ReferenceKind,
    /// Where the reference is: for a call through a veneer, the call.
    pub(crate) site: u64,
    /// The veneer that a call goes through, as an index into
    /// [`Binary::items`].
    pub(crate) veneer: Option<usize>,
}

impl Crossing {
    /// `used`, a reference, as a use of `to`: through `veneer`, or
    /// directly when that is `None`.
    fn new(used: &Reference, veneer: Option<usize>, to: Node) -> Crossing {
        Crossing {
            from: used.from,
            to,
            kind: used.kind,
            site: used.site,
            veneer,
        }
    }
}

/// A call or jump that elfq can't follow across the boundary: through a
/// pointer, or to code outside every item, such as a routine in a
/// microcontroller's ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unfollowed {
    /// The call or jump. For a call through a veneer, the call to the
    /// veneer, with the veneer's target; a tail call is a jump.
    pub(crate) unresolved: Unresolved,
    /// The veneer that a call goes through, as an index into
    /// [`Binary::items`].
    pub(crate) veneer: Option<usize>,
}

impl Unfollowed {
    /// `call`, a call to `veneer`, as a call or jump that elfq can't
    /// follow to `target`, where the veneer jumps.
    pub(crate) fn through_veneer(
        call: &Reference,
        veneer: usize,
        target: Option<u64>,
    ) -> Unfollowed {
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
}

/// What crosses the boundary one way: the uses across it, and the calls
/// and jumps that elfq can't follow that may cross it.
#[derive(Debug)]
pub(crate) struct Crossings {
    /// The uses across it.
    pub(crate) uses: Vec<Crossing>,
    /// Sorted by the item they are in, then by site.
    pub(crate) unfollowed: Vec<Unfollowed>,
}

/// How a reference across the boundary is reported, once veneers are seen
/// through: the rules in the module doc, which [`Boundary::report`] picks
/// from.
enum Report<'r> {
    /// As it is.
    AsIs,
    /// It is a veneer's jump, and these calls to the veneer, from the
    /// veneer's own side, take its place.
    ByCallsToVeneer(Vec<&'r Reference>),
    /// It is a call to `veneer`, which jumps on to `target` without
    /// crossing back: a call to `target` through the veneer.
    ToTarget { veneer: usize, target: Node },
    /// It is a call to a veneer that jumps to code outside every item on
    /// the veneer's side, so it is reported with the calls that elfq can't
    /// follow (see [`Boundary::unfollowed_from`]).
    WithUnfollowed,
}

/// The boundary of some ranges of memory.
pub(crate) struct Boundary<'a> {
    binary: &'a Binary,
    /// `binary`'s graph.
    graph: &'a Graph,
    /// The items inside: those in the ranges, and a set's others.
    items: ItemsInRanges<'a>,
    /// The veneers, and where each jumps to.
    veneers: Veneers<'a>,
    /// The calls in the graph, by the item they call: worked out the first
    /// time a veneer's jump crosses the boundary, as the calls to the veneer
    /// then take its place.
    calls_to: OnceCell<CallsTo>,
}

impl<'a> Boundary<'a> {
    /// The boundary of `ranges` in `binary`, whose graph is `graph`.
    pub(crate) fn new(
        binary: &'a Binary,
        graph: &'a Graph,
        ranges: &'a [AddressRange],
    ) -> Boundary<'a> {
        Boundary::around(binary, graph, ItemsInRanges::new(binary, ranges))
    }

    /// The boundary around `items`, in `binary`, whose graph is `graph`:
    /// for a caller that has worked out the items already, as a range check
    /// does for its checks, or picked some one by one, as a set's are (see
    /// [`ItemsInRanges::with_items`]).
    pub(crate) fn around(
        binary: &'a Binary,
        graph: &'a Graph,
        items: ItemsInRanges<'a>,
    ) -> Boundary<'a> {
        Boundary {
            binary,
            graph,
            items,
            veneers: Veneers::new(binary, graph),
            calls_to: OnceCell::new(),
        }
    }

    /// The items inside: those in the ranges, and a set's others.
    pub(crate) fn items(&self) -> &ItemsInRanges<'a> {
        &self.items
    }

    /// The uses of the rest of the file by the items in the ranges.
    pub(crate) fn leaving(&self) -> Vec<Crossing> {
        self.crossings(true)
    }

    /// The uses of the items in the ranges by the rest of the file.
    fn entering(&self) -> Vec<Crossing> {
        self.crossings(false)
    }

    /// What the items in the ranges use in the rest of the file, with the
    /// calls and jumps in the ranges that elfq can't follow and that may
    /// leave them. When `to` has ranges, only what is in them, and what
    /// elfq can't follow that may go there: to an address in them, or
    /// through a pointer when a function there, outside the ranges, has its
    /// address taken or stored.
    pub(crate) fn leaving_to(&self, to: &[AddressRange]) -> Crossings {
        if to.is_empty() {
            return Crossings {
                uses: self.leaving(),
                unfollowed: self.unfollowed_leaving(),
            };
        }
        let in_to = ItemsInRanges::new(self.binary, to);
        // A veneer in `to` that a use goes through to something outside
        // `to`: the use goes into `to` at the veneer, so it is listed as a
        // use of the veneer.
        let veneer_in_to = |veneer: Option<usize>| {
            veneer
                .filter(|&veneer| in_to.contains_item(veneer) && !self.items.contains_item(veneer))
        };
        let mut uses = Vec::new();
        for crossing in self.leaving() {
            if in_to.contains(crossing.to) {
                uses.push(crossing);
            } else if let Some(veneer) = veneer_in_to(crossing.veneer) {
                uses.push(Crossing {
                    to: Node::Item(veneer),
                    veneer: None,
                    ..crossing
                });
            }
        }
        let through_pointer = in_to
            .callable_through_pointer(self.graph)
            .into_iter()
            .any(|function| !self.items.contains_item(function));
        let mut unfollowed = Vec::new();
        for row in self.unfollowed_leaving() {
            let unresolved = row.unresolved;
            if unresolved.may_go_to(to, through_pointer) {
                unfollowed.push(row);
            } else if let Some(veneer) = veneer_in_to(row.veneer) {
                uses.push(Crossing {
                    from: unresolved.from,
                    to: Node::Item(veneer),
                    kind: if unresolved.is_call {
                        ReferenceKind::Call
                    } else {
                        ReferenceKind::TailCall
                    },
                    site: unresolved.site,
                    veneer: None,
                });
            }
        }
        Crossings { uses, unfollowed }
    }

    /// What uses the items in the ranges from the rest of the file, with
    /// the calls and jumps from there into code in the ranges that is
    /// outside every item, as in a chip's ROM. When `from` has ranges, only
    /// what is in them.
    pub(crate) fn entering_from(&self, from: &[AddressRange]) -> Crossings {
        if from.is_empty() {
            return Crossings {
                uses: self.entering(),
                unfollowed: self.unfollowed_entering(),
            };
        }
        let in_from = ItemsInRanges::new(self.binary, from);
        let (mut uses, others): (Vec<Crossing>, Vec<Crossing>) = self
            .entering()
            .into_iter()
            .partition(|crossing| in_from.contains_item(crossing.from));
        let (mut unfollowed, other_rows): (Vec<Unfollowed>, Vec<Unfollowed>) = self
            .unfollowed_entering()
            .into_iter()
            .partition(|row| in_from.contains_item(row.unresolved.from));
        // A call through a veneer in `from` whose caller is outside `from`:
        // the veneer's jump is what uses the ranges from `from`, unless a
        // caller in `from` already has a row for it.
        let veneer_in_from = |veneer: usize, kept: &[Option<usize>]| {
            in_from.contains_item(veneer)
                && !self.items.contains_item(veneer)
                && !kept.contains(&Some(veneer))
        };
        let kept: Vec<Option<usize>> = uses.iter().map(|crossing| crossing.veneer).collect();
        let mut jumps: Vec<Crossing> = others
            .iter()
            .filter(|crossing| {
                crossing
                    .veneer
                    .is_some_and(|veneer| veneer_in_from(veneer, &kept))
            })
            .filter_map(|crossing| {
                let veneer = crossing.veneer?;
                // A PLT stub has a jump to its header too.
                let jump = self
                    .graph
                    .references_from(veneer)
                    .iter()
                    .find(|jump| jump.to == crossing.to)?;
                Some(Crossing::new(jump, None, jump.to))
            })
            .collect();
        jumps.sort_by_key(|jump| (jump.from, jump.site));
        jumps.dedup();
        uses.extend(jumps);
        let kept: Vec<Option<usize>> = unfollowed.iter().map(|row| row.veneer).collect();
        let mut jumps: Vec<Unfollowed> = other_rows
            .iter()
            .filter(|row| {
                row.veneer
                    .is_some_and(|veneer| veneer_in_from(veneer, &kept))
            })
            .filter_map(|row| {
                let veneer = row.veneer?;
                let jump = self
                    .graph
                    .unresolved_in(veneer)
                    .iter()
                    .find(|jump| jump.target == row.unresolved.target)?;
                Some(Unfollowed {
                    unresolved: *jump,
                    veneer: None,
                })
            })
            .collect();
        jumps.sort_by_key(|jump| (jump.unresolved.from, jump.unresolved.site));
        jumps.dedup();
        unfollowed.extend(jumps);
        unfollowed.sort_by_key(|row| (row.unresolved.from, row.unresolved.site));
        Crossings { uses, unfollowed }
    }

    /// The calls and jumps in the ranges that elfq can't follow, and that
    /// may leave them: through a pointer, or to code outside every item,
    /// directly or through a veneer. Code outside every item may leave the
    /// ranges even when it is in them, since elfq doesn't read it: it may be
    /// a veneer whose symbol was stripped.
    fn unfollowed_leaving(&self) -> Vec<Unfollowed> {
        self.unfollowed_from(true, |_| true)
    }

    /// The calls and jumps from the rest of the file into code in the
    /// ranges that is outside every item, such as a routine in a
    /// microcontroller's ROM, directly or through a veneer.
    fn unfollowed_entering(&self) -> Vec<Unfollowed> {
        self.unfollowed_from(false, |target| {
            target.is_some_and(|target| self.items.contains_address(target))
        })
    }

    /// The calls and jumps that elfq can't follow from the items on one
    /// side of the boundary, the ranges' side when `from_inside`, whose
    /// target passes `target_counts`. In place of a veneer's jump, the
    /// calls to the veneer from its own side.
    fn unfollowed_from(
        &self,
        from_inside: bool,
        target_counts: impl Fn(Option<u64>) -> bool,
    ) -> Vec<Unfollowed> {
        let mut unfollowed = Vec::new();
        for unresolved in &self.graph.unresolved {
            if self.items.contains_item(unresolved.from) != from_inside
                || !target_counts(unresolved.target)
            {
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
                unfollowed.push(Unfollowed::through_veneer(
                    call,
                    unresolved.from,
                    unresolved.target,
                ));
            }
        }
        // The calls to a veneer on the other side, which jumps to code on
        // its own side.
        for reference in &self.graph.references {
            if self.items.contains_item(reference.from) == from_inside
                && let Some((veneer, target)) = self.call_to_veneer_across(reference)
            {
                unfollowed.push(Unfollowed::through_veneer(reference, veneer, Some(target)));
            }
        }
        unfollowed
            .sort_by_key(|unfollowed| (unfollowed.unresolved.from, unfollowed.unresolved.site));
        unfollowed
    }

    /// The uses across the boundary by the items on one side of it: the items
    /// in the ranges when `from_inside`, or else the rest.
    fn crossings(&self, from_inside: bool) -> Vec<Crossing> {
        let mut crossings = Vec::new();
        for reference in &self.graph.references {
            if self.items.contains_item(reference.from) != from_inside
                || self.items.contains(reference.to) == from_inside
            {
                continue;
            }
            match self.report(reference, from_inside) {
                Report::AsIs => crossings.push(Crossing::new(reference, None, reference.to)),
                Report::ByCallsToVeneer(calls) => crossings.extend(
                    calls
                        .into_iter()
                        .map(|call| Crossing::new(call, Some(reference.from), reference.to)),
                ),
                Report::ToTarget { veneer, target } => {
                    crossings.push(Crossing::new(reference, Some(veneer), target));
                }
                Report::WithUnfollowed => {}
            }
        }
        crossings
    }

    /// How `reference`, which crosses the boundary from the ranges' side
    /// when `from_inside`, is reported once veneers are seen through.
    ///
    /// The rules are tried in this order, and the order decides the answer:
    /// a veneer's jump is a tail call, which counts as a call, so when one
    /// veneer jumps to another, the jump matches more than one rule. The
    /// test `a_veneer_that_jumps_to_a_veneer_is_reported_by_the_first_rule_that_matches`
    /// pins it.
    fn report(&self, reference: &'a Reference, from_inside: bool) -> Report<'a> {
        let calls = self.calls_from_its_side(reference.from, VeneerTarget::Node(reference.to));
        if !calls.is_empty() {
            return Report::ByCallsToVeneer(calls);
        }
        if reference.kind.is_call()
            && let Node::Item(veneer) = reference.to
            && let Some(VeneerTarget::Node(target)) = self.veneers.target(veneer)
            && self.items.contains(target) != from_inside
        {
            return Report::ToTarget { veneer, target };
        }
        if self.call_to_veneer_across(reference).is_some() {
            return Report::WithUnfollowed;
        }
        Report::AsIs
    }

    /// When `item` is a veneer that jumps to `target`, the calls to it from
    /// its own side of the boundary, which take the place of that jump.
    /// Otherwise, or when nothing on its side calls it, nothing.
    fn calls_from_its_side(&self, item: usize, target: VeneerTarget) -> Vec<&'a Reference> {
        if self.veneers.target(item) != Some(target) {
            return Vec::new();
        }
        let side = self.items.contains_item(item);
        let references = &self.graph.references;
        let calls_to = self
            .calls_to
            .get_or_init(|| CallsTo::new(references, self.binary.items.len()));
        calls_to
            .of(item)
            .iter()
            .map(|&index| &references[index])
            .filter(|reference| self.items.contains_item(reference.from) == side)
            .collect()
    }

    /// When `reference` is a call across the boundary to a veneer that
    /// jumps to code outside every item on the veneer's side, the veneer, as
    /// an index into [`Binary::items`], and that code's address.
    fn call_to_veneer_across(&self, reference: &Reference) -> Option<(usize, u64)> {
        let Node::Item(veneer) = reference.to else {
            return None;
        };
        let side = self.items.contains_item(veneer);
        if !reference.kind.is_call() || self.items.contains_item(reference.from) == side {
            return None;
        }
        match self.veneers.target(veneer)? {
            VeneerTarget::Outside(target) if self.items.contains_address(target) == side => {
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

    /// The unfollowed calls and jumps as (from, target, veneer), with the
    /// target in hex.
    fn unfollowed_names(binary: &Binary, rows: &[Unfollowed]) -> Vec<(String, String, String)> {
        rows.iter()
            .map(|row| {
                (
                    binary.items[row.unresolved.from].name.clone(),
                    row.unresolved
                        .target
                        .map_or(String::new(), |target| format!("{target:#x}")),
                    row.veneer
                        .map_or(String::new(), |veneer| binary.items[veneer].name.clone()),
                )
            })
            .collect()
    }

    #[test]
    fn a_veneer_that_jumps_to_a_veneer_is_reported_by_the_first_rule_that_matches() {
        // As if __app_tick_veneer jumped to __memcpy_veneer, which jumps to
        // memcpy. SysTick_Handler, at 0x0, tail-calls __app_tick_veneer, at
        // 0xd0, and __memcpy_veneer is at 0xd8. A veneer's jump is a tail
        // call, which counts as a call, so the jump to __memcpy_veneer is
        // both a veneer's jump and a call to a veneer: the order of the
        // rules decides how it is reported.
        let (binary, mut graph) = sample();
        let (first, second) = (
            index(&binary, "__app_tick_veneer"),
            index(&binary, "__memcpy_veneer"),
        );
        let jump = graph
            .references
            .iter_mut()
            .find(|reference| reference.from == first)
            .unwrap();
        jump.to = Node::Item(second);
        // With the caller: the call to the first veneer takes the place of
        // its jump, as a call through it to the second veneer.
        let ranges = [range("0x0..0x10"), range("0xd0..0xd8")];
        let boundary = Boundary::new(&binary, &graph, &ranges);
        assert_eq!(
            names(&binary, &boundary.leaving()),
            [
                crossing("SysTick_Handler", "__memcpy_veneer", "__app_tick_veneer"),
                crossing("SysTick_Handler", "systick_count", ""),
            ]
        );
        // Without the caller, nothing on the first veneer's side calls it,
        // so its jump is a call to the second veneer's target.
        let ranges = [range("0xd0..0xd8")];
        let boundary = Boundary::new(&binary, &graph, &ranges);
        assert_eq!(
            names(&binary, &boundary.leaving()),
            [crossing("__app_tick_veneer", "memcpy", "__memcpy_veneer")]
        );
        assert_eq!(
            names(&binary, &boundary.entering()),
            [crossing("SysTick_Handler", "__app_tick_veneer", "")]
        );

        // As if the second veneer jumped to a routine in ROM.
        let rom = 0x1fff_0000;
        let site = graph.references_from(second)[0].site;
        graph
            .references
            .retain(|reference| reference.from != second);
        graph.unresolved.push(Unresolved {
            from: second,
            site,
            is_call: false,
            target: Some(rom),
        });
        graph.unresolved.sort_by_key(|u| (u.from, u.site));
        // With the caller, the first rule still holds, and the jump is a
        // call to code outside every item too.
        let ranges = [range("0x0..0x10"), range("0xd0..0xd8")];
        let boundary = Boundary::new(&binary, &graph, &ranges);
        assert_eq!(
            names(&binary, &boundary.leaving()),
            [
                crossing("SysTick_Handler", "__memcpy_veneer", "__app_tick_veneer"),
                crossing("SysTick_Handler", "systick_count", ""),
            ]
        );
        assert_eq!(
            unfollowed_names(&binary, &boundary.unfollowed_leaving()),
            [(
                "__app_tick_veneer".to_string(),
                format!("{rom:#x}"),
                "__memcpy_veneer".to_string()
            )]
        );
        // Without the caller, the jump is only a call to code outside every
        // item.
        let ranges = [range("0xd0..0xd8")];
        let boundary = Boundary::new(&binary, &graph, &ranges);
        assert!(boundary.leaving().is_empty());
        assert_eq!(
            unfollowed_names(&binary, &boundary.unfollowed_leaving()),
            [(
                "__app_tick_veneer".to_string(),
                format!("{rom:#x}"),
                "__memcpy_veneer".to_string()
            )]
        );
    }

    #[test]
    fn both_ways_agree_on_a_veneer_with_callers_on_both_sides() {
        // on_timer, outside the flash range below, calls crc32 through the
        // veneer in it too, as process_frame does.
        let (binary, mut graph) = sample();
        let (on_timer, veneer) = (index(&binary, "on_timer"), index(&binary, "__crc32_veneer"));
        graph.references.push(Reference {
            from: on_timer,
            to: Node::Item(veneer),
            kind: ReferenceKind::Call,
            site: binary.items[on_timer].address + 4,
        });
        graph.references.sort_by_key(|r| (r.from, r.site));
        let flash = [range("0x30000320..0x300009c0")];
        let itcm = [range("0x0+256K")];
        let sites = |uses: &[Crossing]| {
            let mut sites: Vec<u64> = uses.iter().map(|crossing| crossing.site).collect();
            sites.sort_unstable();
            sites
        };
        // process_frame's calls, and not the veneer's jump, which a caller
        // in the flash range already stands for.
        let entering = Boundary::new(&binary, &graph, &itcm).entering_from(&flash);
        assert_eq!(
            sites(&entering.uses),
            [0x3000_034c, 0x3000_0386, 0x3000_0454]
        );
        assert!(entering.uses.iter().all(|crossing| crossing.from != veneer));
        // The same uses, asked the other way.
        let leaving = Boundary::new(&binary, &graph, &flash).leaving_to(&itcm);
        assert_eq!(sites(&leaving.uses), sites(&entering.uses));
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
            .unfollowed_leaving()
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
            .unfollowed_leaving()
            .iter()
            .map(|u| u.unresolved.target)
            .collect();
        assert_eq!(targets, [Some(0x1fff_0000)]);
    }
}
