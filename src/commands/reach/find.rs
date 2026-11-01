//! The search from SYMBOL: [`Reach::find`], and what it uses to list a
//! call through a veneer as the call to the veneer.

use std::collections::{HashMap, HashSet};

use super::{Reach, Step, Use};
use crate::binary::{AddressRange, Binary, ItemKind, memory_blocks};
use crate::graph::{
    Graph, ItemsInRanges, Node, Reference, ReferenceKind, RootKind, ShortestPaths, Unfollowed,
    Unresolved, VeneerTarget, Veneers,
};

impl<'g, 'a> Reach<'g, 'a> {
    /// Finds everything that SYMBOL, the item at `index`, reaches in
    /// `graph`, the file's reference graph, keeping only the calls and
    /// jumps that elfq can't follow that may go to `to`, the ranges of
    /// `--to`, if there are any, which [`check_to`] has checked. SYMBOL
    /// must be an item whose code elfq can read (see [`readable_item`]): of
    /// a function whose end elfq doesn't know, the answer would be that it
    /// reaches nothing.
    ///
    /// [`check_to`]: super::check_to
    /// [`readable_item`]: crate::commands::checks::readable_item
    pub(super) fn find(
        binary: &'a Binary,
        graph: &'g Graph,
        index: usize,
        to: &[AddressRange],
    ) -> Reach<'g, 'a> {
        let start = Node::Item(index);
        let paths = ShortestPaths::from(binary, graph, start);
        let veneers = Veneers::new(binary, graph);
        let blocks = memory_blocks(&binary.sections);
        let to_itself: Vec<&Reference> = graph
            .references_from(index)
            .iter()
            .filter(|reference| reference.to == start)
            .collect();
        let self_use = match &to_itself[..] {
            [] => None,
            uses if uses.iter().all(|reference| reference.kind.is_call()) => Some("calls"),
            _ => Some("uses"),
        };
        let nodes = (0..binary.items.len())
            .map(Node::Item)
            .chain((0..binary.imports.len()).map(Node::Import))
            .filter(|&node| paths.is_reached(node));
        // Each item's children, in the order it first uses them.
        let mut children_of_item: HashMap<usize, Vec<Node>> = HashMap::new();
        for node in nodes {
            if let Some(via) = paths.via(node) {
                children_of_item.entry(via.from).or_default().push(node);
            }
        }
        for (&parent, nodes) in &mut children_of_item {
            let mut first_use: HashMap<Node, usize> = HashMap::new();
            for (position, reference) in graph.references_from(parent).iter().enumerate() {
                first_use.entry(reference.to).or_insert(position);
            }
            nodes.sort_by_key(|node| first_use[node]);
        }

        let mut steps: Vec<Step> = Vec::new();
        // The steps to add, each with its parent's step, last first.
        let mut next = vec![(start, None)];
        while let Some((node, parent)) = next.pop() {
            let via = paths.via(node);
            let sites = via.map_or_else(Vec::new, |via| {
                graph
                    .references_from(via.from)
                    .iter()
                    .filter(|r| r.to == node && r.kind == via.kind)
                    .map(|r| r.site)
                    .collect()
            });
            let used = via.map(|via| used(&paths, &veneers, node, via));
            let block = match node {
                Node::Item(index) => {
                    let address = binary.items[index].address;
                    blocks
                        .iter()
                        .position(|block| block.range.contains(address))
                }
                Node::Import(_) => None,
            };
            let step = steps.len();
            steps.push(Step {
                node,
                parent,
                via,
                sites,
                used,
                block,
                through_pointer: false,
            });
            if let Node::Item(index) = node
                && let Some(nodes) = children_of_item.get(&index)
            {
                next.extend(nodes.iter().rev().map(|&child| (child, Some(step))));
            }
        }
        let called = called(graph, &steps);
        for step in &mut steps[1..] {
            step.through_pointer = match step.node {
                Node::Item(index) => {
                    binary.items[index].kind == ItemKind::Function && !called.contains(&step.node)
                }
                Node::Import(_) => false,
            };
        }

        let mut unresolved = Vec::new();
        let mut unfollowed = Vec::new();
        let mut unreadable = Vec::new();
        for step in &steps {
            let Node::Item(index) = step.node else {
                continue;
            };
            let item = &binary.items[index];
            if item.end_unknown() {
                unreadable.push(index);
            }
            if graph.is_plt_header(step.node) {
                continue;
            }
            for &jump in graph.unresolved_in(index) {
                unresolved.push(jump);
                unfollowed.push(through_veneer(&paths, &veneers, jump));
            }
        }
        let plt_header = graph.is_plt_header(start);
        if !to.is_empty() {
            let callable_through_pointer = !ItemsInRanges::new(binary, to)
                .callable_through_pointer(graph)
                .is_empty();
            unresolved.retain(|jump| jump.may_go_to(to, callable_through_pointer));
            unfollowed.retain(|row| row.unresolved.may_go_to(to, callable_through_pointer));
        }
        Reach {
            binary,
            graph,
            symbol: index,
            self_use,
            plt_header,
            steps,
            unresolved,
            unfollowed,
            unreadable,
            blocks,
            to: to.to_vec(),
        }
    }
}

/// How the text lists the use that reaches `node`, from `via`: as the call
/// to a veneer when `via` is the veneer's jump to its target and the veneer
/// is reached by a call.
fn used(paths: &ShortestPaths, veneers: &Veneers, node: Node, via: Reference) -> Use {
    match call_to_veneer(paths, veneers, via.from, VeneerTarget::Node(node)) {
        Some(call) => Use {
            user: call.from,
            kind: call.kind,
            veneer: Some(via.from),
        },
        None => Use {
            user: via.from,
            kind: via.kind,
            veneer: None,
        },
    }
}

/// `jump` as the text lists it: when it is a veneer's jump to code outside
/// every item, and a call reaches the veneer, as that call, through the
/// veneer.
fn through_veneer(paths: &ShortestPaths, veneers: &Veneers, jump: Unresolved) -> Unfollowed {
    let call = jump.target.and_then(|target| {
        call_to_veneer(paths, veneers, jump.from, VeneerTarget::Outside(target))
    });
    match call {
        Some(call) => Unfollowed::through_veneer(&call, jump.from, jump.target),
        None => Unfollowed {
            unresolved: jump,
            veneer: None,
        },
    }
}

/// When `veneer` is a veneer that jumps to `target`, and the chain from
/// SYMBOL reaches it by a call, that call.
fn call_to_veneer(
    paths: &ShortestPaths,
    veneers: &Veneers,
    veneer: usize,
    target: VeneerTarget,
) -> Option<Reference> {
    if veneers.target(veneer) != Some(target) {
        return None;
    }
    paths
        .via(Node::Item(veneer))
        .filter(|call| call.kind.is_call())
}

/// The nodes that something in `steps` calls, or that a vector table among
/// them points to, since only the processor calls through it.
fn called(graph: &Graph, steps: &[Step]) -> HashSet<Node> {
    let mut called = HashSet::new();
    for step in steps {
        let Node::Item(index) = step.node else {
            continue;
        };
        let vector_table = graph
            .root(step.node)
            .is_some_and(|root| root.kind == RootKind::VectorTable);
        for reference in graph.references_from(index) {
            if reference.kind.is_call()
                || (vector_table && reference.kind == ReferenceKind::Pointer)
            {
                called.insert(reference.to);
            }
        }
    }
    called
}
