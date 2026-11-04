//! The search from SYMBOL: [`Reach::find`], in named steps, and what it
//! uses to list a call through a veneer as the call to the veneer.

use std::collections::HashSet;

use super::{CantFollow, Link, Reach, Step, StepId, Use};
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
        let mut steps: Vec<Step> = Vec::new();
        // The steps to add, each with its parent's step, last first.
        let mut next = vec![(symbol, None)];
        while let Some((node, parent)) = next.pop() {
            let id = steps.len();
            steps.push(self.step(node, parent));
            if let Node::Item(index) = node {
                let children = self.children(index);
                next.extend(children.into_iter().rev().map(|child| (child, Some(id))));
            }
        }
        steps
    }

    /// The nodes under the item at `index` in the tree: those whose chain
    /// from SYMBOL ends with a reference from it, in the order that it
    /// first uses them. The tree's own children are by step (see
    /// `Tree::children` in `tree.rs`).
    fn children(&self, index: usize) -> Vec<Node> {
        // An item can use a node many times: the node goes at the first.
        let mut placed: HashSet<Node> = HashSet::new();
        (self.graph.references_from(index).iter())
            .map(|reference| reference.to)
            .filter(|&to| self.paths.via(to).is_some_and(|via| via.from == index))
            .filter(|&to| placed.insert(to))
            .collect()
    }

    /// The step of `node` under the step `parent`, which is `None` only
    /// for SYMBOL. Whether it is reached through pointers is left for
    /// [`mark_through_pointer`].
    fn step(&self, node: Node, parent: Option<StepId>) -> Step {
        let link = parent.zip(self.paths.via(node)).map(|(parent, via)| Link {
            parent,
            via,
            used: self.used(node, via),
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
            link,
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// The steps from every item that elfq can read in four samples, as
    /// SYMBOL, keep the rules that leave them one order: SYMBOL's step
    /// first, and one step for each node it reaches; each other step under
    /// the step of the item whose reference ends its chain; depth first;
    /// and the steps under each step in the order that its item first uses
    /// them, whichever reference ends their chains.
    #[test]
    fn steps_go_depth_first_each_under_its_item_in_the_order_it_uses_them() {
        let samples = [
            "c-x86_64.elf",
            "c-cortex-m7.elf",
            "far-gnu-m4-nodebug.elf",
            "rust-cortex-m4.elf",
        ];
        for sample in samples {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("testdata/bin")
                .join(sample);
            let binary = Binary::load(&path).unwrap();
            let graph = Graph::build(&binary);
            let nodes: Vec<Node> = (0..binary.items.len())
                .map(Node::Item)
                .chain((0..binary.imports.len()).map(Node::Import))
                .collect();
            for index in (0..binary.items.len()).filter(|&i| !binary.items[i].end_unknown()) {
                let context = format!("{sample} reach {}", binary.items[index].name);
                let steps = Reach::find(&binary, &graph, index, &[]).steps;
                let paths = ShortestPaths::from(&binary, &graph, Node::Item(index));
                let reached: HashSet<Node> = (nodes.iter().copied())
                    .filter(|&node| paths.is_reached(node))
                    .collect();
                let stepped: HashSet<Node> = steps.iter().map(|step| step.node).collect();
                assert_eq!(steps[0].node, Node::Item(index), "{context}");
                assert_eq!(steps.len(), reached.len(), "{context}: a node twice");
                assert_eq!(stepped, reached, "{context}");

                // The steps whose subtrees the next step can still be in,
                // SYMBOL's first.
                let mut open: Vec<StepId> = vec![0];
                // Where each step's item first uses the last step under it.
                let mut last_child: Vec<Option<usize>> = vec![None; steps.len()];
                for (id, step) in steps.iter().enumerate().skip(1) {
                    let parent = step.parent().unwrap();
                    let via = paths.via(step.node).unwrap();
                    assert_eq!(steps[parent].node, Node::Item(via.from), "{context}");
                    while open.last() != Some(&parent) {
                        assert!(
                            open.pop().is_some(),
                            "{context}: step {id} isn't depth first"
                        );
                    }
                    open.push(id);
                    let first_use = (graph.references_from(via.from).iter())
                        .position(|reference| reference.to == step.node);
                    assert!(last_child[parent] < first_use, "{context}: step {id}");
                    last_child[parent] = first_use;
                }
            }
        }
    }
}
