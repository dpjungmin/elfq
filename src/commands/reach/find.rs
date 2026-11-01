//! The search from SYMBOL: [`Reach::find`], in named steps, and what it
//! uses to list a call through a veneer as the call to the veneer.

use std::collections::{HashMap, HashSet};

use super::{CantFollow, Reach, Step, StepId, Use};
use crate::binary::{AddressRange, Binary, ItemKind, MemoryBlock, memory_blocks};
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
        let symbol = Node::Item(index);
        let search = Search::new(binary, graph, symbol);
        let mut steps = search.steps(symbol);
        mark_through_pointer(binary, graph, &mut steps);
        let mut cant_follow = search.cant_follow(&steps);
        if !to.is_empty() {
            keep_those_that_may_go_to(binary, graph, to, &mut cant_follow);
        }
        Reach {
            binary,
            graph,
            symbol: index,
            self_use: self_use(graph, index),
            plt_header: graph.is_plt_header(symbol),
            unreadable: unreadable(binary, &steps),
            steps,
            cant_follow,
            blocks: search.blocks,
            to: to.to_vec(),
        }
    }
}

/// How SYMBOL, the item at `index`, uses itself, as a recursive function
/// calls itself: "calls" when every such use is a call, "uses" when one
/// isn't, or `None` when it doesn't use itself.
fn self_use(graph: &Graph, index: usize) -> Option<&'static str> {
    let symbol = Node::Item(index);
    let to_itself: Vec<&Reference> = graph
        .references_from(index)
        .iter()
        .filter(|reference| reference.to == symbol)
        .collect();
    match &to_itself[..] {
        [] => None,
        uses if uses.iter().all(|reference| reference.kind.is_call()) => Some("calls"),
        _ => Some("uses"),
    }
}

/// What the search from SYMBOL looks things up in.
struct Search<'s> {
    binary: &'s Binary,
    graph: &'s Graph,
    /// The shortest chain from SYMBOL to each node that it reaches.
    paths: ShortestPaths,
    veneers: Veneers<'s>,
    /// The file's blocks of memory, as the overview lists them.
    blocks: Vec<MemoryBlock>,
}

impl<'s> Search<'s> {
    fn new(binary: &'s Binary, graph: &'s Graph, symbol: Node) -> Search<'s> {
        Search {
            binary,
            graph,
            paths: ShortestPaths::from(binary, graph, symbol),
            veneers: Veneers::new(binary, graph),
            blocks: memory_blocks(&binary.sections),
        }
    }

    /// SYMBOL and every node that it reaches, as the steps of the tree:
    /// SYMBOL first, then depth first, each item's children in the order
    /// that it first uses them.
    fn steps(&self, symbol: Node) -> Vec<Step> {
        let children = self.children_in_use_order();
        let mut steps: Vec<Step> = Vec::new();
        // The steps to add, each with its parent's step, last first.
        let mut next = vec![(symbol, None)];
        while let Some((node, parent)) = next.pop() {
            let id = steps.len();
            steps.push(self.step(node, parent));
            if let Node::Item(index) = node
                && let Some(nodes) = children.get(&index)
            {
                next.extend(nodes.iter().rev().map(|&child| (child, Some(id))));
            }
        }
        steps
    }

    /// The nodes under each item in the tree, those whose chain from
    /// SYMBOL ends with a reference from it, in the order that it first
    /// uses them.
    fn children_in_use_order(&self) -> HashMap<usize, Vec<Node>> {
        let binary = self.binary;
        let nodes = (0..binary.items.len())
            .map(Node::Item)
            .chain((0..binary.imports.len()).map(Node::Import))
            .filter(|&node| self.paths.is_reached(node));
        let mut children: HashMap<usize, Vec<Node>> = HashMap::new();
        for node in nodes {
            if let Some(via) = self.paths.via(node) {
                children.entry(via.from).or_default().push(node);
            }
        }
        for (&parent, nodes) in &mut children {
            let mut first_use: HashMap<Node, usize> = HashMap::new();
            for (position, reference) in self.graph.references_from(parent).iter().enumerate() {
                first_use.entry(reference.to).or_insert(position);
            }
            nodes.sort_by_key(|node| first_use[node]);
        }
        children
    }

    /// The step of `node` under the step `parent`, which is `None` only
    /// for SYMBOL. Whether it is reached through pointers is left for
    /// [`mark_through_pointer`].
    fn step(&self, node: Node, parent: Option<StepId>) -> Step {
        let via = self.paths.via(node);
        let sites = via.map_or_else(Vec::new, |via| {
            self.graph
                .references_from(via.from)
                .iter()
                .filter(|r| r.to == node && r.kind == via.kind)
                .map(|r| r.site)
                .collect()
        });
        let block = match node {
            Node::Item(index) => {
                let address = self.binary.items[index].address;
                self.blocks
                    .iter()
                    .position(|block| block.range.contains(address))
            }
            Node::Import(_) => None,
        };
        Step {
            node,
            parent,
            via,
            sites,
            used: via.map(|via| self.used(node, via)),
            block,
            through_pointer: false,
        }
    }

    /// The calls and jumps in `steps` that elfq can't follow, in the order
    /// of the steps, then by site, but for the jump in a PLT's header,
    /// which goes to the dynamic loader.
    fn cant_follow(&self, steps: &[Step]) -> Vec<CantFollow> {
        let mut cant_follow = Vec::new();
        for step in steps {
            let Node::Item(index) = step.node else {
                continue;
            };
            if self.graph.is_plt_header(step.node) {
                continue;
            }
            cant_follow.extend(
                self.graph
                    .unresolved_in(index)
                    .iter()
                    .map(|&found| CantFollow {
                        found,
                        listed: self.through_veneer(found),
                    }),
            );
        }
        cant_follow
    }

    /// How the text lists the use that reaches `node`, from `via`: as the
    /// call to a veneer when `via` is the veneer's jump to its target and
    /// the veneer is reached by a call.
    fn used(&self, node: Node, via: Reference) -> Use {
        match self.call_to_veneer(via.from, VeneerTarget::Node(node)) {
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

    /// `jump` as the text lists it: when it is a veneer's jump to code
    /// outside every item, and a call reaches the veneer, as that call,
    /// through the veneer.
    fn through_veneer(&self, jump: Unresolved) -> Unfollowed {
        let call = jump
            .target
            .and_then(|target| self.call_to_veneer(jump.from, VeneerTarget::Outside(target)));
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
    fn call_to_veneer(&self, veneer: usize, target: VeneerTarget) -> Option<Reference> {
        if self.veneers.target(veneer) != Some(target) {
            return None;
        }
        self.paths
            .via(Node::Item(veneer))
            .filter(|call| call.kind.is_call())
    }
}

/// Marks the functions in `steps` but SYMBOL that nothing in them calls,
/// which SYMBOL reaches only through pointers (see
/// `ItemJson::through_pointer` in `json.rs`).
fn mark_through_pointer(binary: &Binary, graph: &Graph, steps: &mut [Step]) {
    let called = called(graph, steps);
    for step in &mut steps[1..] {
        step.through_pointer = match step.node {
            Node::Item(index) => {
                binary.items[index].kind == ItemKind::Function && !called.contains(&step.node)
            }
            Node::Import(_) => false,
        };
    }
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

/// The functions in `steps` whose end elfq doesn't know, in the order of
/// the steps.
fn unreadable(binary: &Binary, steps: &[Step]) -> Vec<usize> {
    steps
        .iter()
        .filter_map(|step| match step.node {
            Node::Item(index) if binary.items[index].end_unknown() => Some(index),
            _ => None,
        })
        .collect()
}

/// Keeps of `cant_follow` those that may go to `to`, the ranges of
/// `--to`, by the rule of `callees --to` (see [`Unresolved::may_go_to`]).
fn keep_those_that_may_go_to(
    binary: &Binary,
    graph: &Graph,
    to: &[AddressRange],
    cant_follow: &mut Vec<CantFollow>,
) {
    let callable_through_pointer = !ItemsInRanges::new(binary, to)
        .callable_through_pointer(graph)
        .is_empty();
    cant_follow.retain(|cant| cant.found.may_go_to(to, callable_through_pointer));
}
