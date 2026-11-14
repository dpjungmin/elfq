//! Dominators: which items are reachable only through which.
//!
//! Item X *dominates* item Y when every path from the roots to Y passes
//! through X. Then Y is *exclusive* to X: it is in the file only because X
//! is. The dominators form a tree, rooted above the graph's roots, in which
//! each item hangs under its closest dominator. X's *retained size* is the
//! size of X plus the sizes of everything below it in that tree: the bytes
//! that would go away if X did.
//!
//! The tree is computed with the algorithm of Cooper, Harvey and Kennedy,
//! from `petgraph`.

use std::cmp::Ordering;

use petgraph::algo::dominators::simple_fast;
use petgraph::graph::{DiGraph, NodeIndex};

use super::{Graph, Node, NodeMap, Position};
use crate::binary::{AddressRange, Binary};

/// The dominator tree of a binary's graph.
#[derive(Debug)]
pub struct DominatorTree {
    /// Each node's closest dominator. `None` for nodes that no root
    /// reaches, and for the nodes directly under the tree's top: the roots,
    /// and nodes that several roots reach.
    parent: NodeMap<Option<Node>>,
    /// The nodes each node dominates directly, largest retained size first.
    children: NodeMap<Vec<Node>>,
    /// The nodes directly under the tree's top: the roots, and anything
    /// that several roots reach.
    top: Vec<Node>,
    /// Whether a path from the roots reaches each node.
    reachable: NodeMap<bool>,
    /// Each node's retained size: its own size, counting only its bytes in
    /// the range when the tree is built with one, plus its children's.
    retained: NodeMap<u64>,
}

impl DominatorTree {
    /// Builds the tree of `binary`'s graph. With a `range`, sizes count only
    /// the bytes inside it, load images too (see [`Binary::bytes_in`]), so
    /// an item outside it retains what it alone keeps there.
    pub fn build(binary: &Binary, graph: &Graph, range: Option<AddressRange>) -> DominatorTree {
        let (items, imports) = (binary.items.len(), binary.imports.len());
        let sizes = NodeMap::from_fn(items, imports, |node| match node {
            Node::Item(index) => binary.bytes_in(&binary.items[index], range),
            // Imports have no bytes in the file.
            Node::Import(_) => 0,
        });
        DominatorTree::from_graph(&sizes, graph)
    }

    /// Builds the tree of `graph`, whose nodes are those of `sizes`, each of
    /// the size that `sizes` gives.
    pub(super) fn from_graph(sizes: &NodeMap<u64>, graph: &Graph) -> DominatorTree {
        let roots: Vec<Node> = graph.roots.iter().map(|root| root.node).collect();
        let edges = graph
            .references
            .iter()
            .map(|reference| (Node::Item(reference.from), reference.to));
        DominatorTree::from_edges(sizes, &roots, edges)
    }

    /// Builds the tree for a graph whose nodes are those of `sizes`.
    fn from_edges(
        sizes: &NodeMap<u64>,
        roots: &[Node],
        edges: impl Iterator<Item = (Node, Node)>,
    ) -> DominatorTree {
        let count = sizes.len();
        // Vertex 0 is a virtual top that points at every root; vertex p + 1
        // is the graph node at position p.
        let mut petgraph = DiGraph::<(), ()>::with_capacity(count + 1, count);
        for _ in 0..=count {
            petgraph.add_node(());
        }
        let vertex_at = |position: Position| NodeIndex::new(position.0 + 1);
        let position_of = |vertex: NodeIndex| Position(vertex.index() - 1);
        let vertex = |node: Node| vertex_at(sizes.position(node));
        let top = NodeIndex::new(0);
        for &root in roots {
            petgraph.add_edge(top, vertex(root), ());
        }
        for (from, to) in edges {
            petgraph.add_edge(vertex(from), vertex(to), ());
        }
        let dominators = simple_fast(&petgraph, top);

        let mut tree = DominatorTree {
            parent: sizes.with_each(None),
            children: sizes.with_each(Vec::new()),
            top: Vec::new(),
            reachable: sizes.with_each(false),
            // Nothing is exclusive to a node that no root reaches, so its
            // retained size stays its own.
            retained: sizes.clone(),
        };
        for at in (0..count).map(Position) {
            let Some(dominator) = dominators.immediate_dominator(vertex_at(at)) else {
                continue;
            };
            let node = sizes.node(at);
            tree.reachable[at] = true;
            if dominator == top {
                tree.top.push(node);
            } else {
                let parent = position_of(dominator);
                tree.parent[at] = Some(sizes.node(parent));
                tree.children[parent].push(node);
            }
        }
        tree.add_retained_sizes(sizes);
        tree
    }

    /// Whether any path from the roots reaches `node`.
    pub fn is_reachable(&self, node: Node) -> bool {
        self.reachable[node]
    }

    /// The size of `node` plus the sizes of everything exclusive to it.
    pub fn retained(&self, node: Node) -> u64 {
        self.retained[node]
    }

    /// The closest node that every path to `node` passes through. `None` for
    /// the roots, for nodes that several roots reach, and for unreachable
    /// nodes.
    pub fn parent(&self, node: Node) -> Option<Node> {
        self.parent[node]
    }

    /// The nodes that `node` dominates directly, largest retained size
    /// first.
    pub(crate) fn children(&self, node: Node) -> &[Node] {
        &self.children[node]
    }

    /// The nodes directly under the tree's top: the roots, and the nodes
    /// that several roots reach, largest retained size first.
    pub(crate) fn top(&self) -> &[Node] {
        &self.top
    }

    /// Sorts `nodes` as the tree sorts the nodes under each node and under
    /// its top: largest retained size first.
    pub(super) fn sort_largest_first(&self, nodes: &mut [Node]) {
        nodes.sort_by(largest_first(&self.retained));
    }

    /// Everything exclusive to `node`: all the nodes below it in the tree.
    pub fn exclusive(&self, node: Node) -> Vec<Node> {
        let mut exclusive = Vec::new();
        let mut stack = self.children(node).to_vec();
        while let Some(next) = stack.pop() {
            exclusive.push(next);
            stack.extend_from_slice(self.children(next));
        }
        exclusive
    }

    /// Adds up retained sizes from the bottom of the tree, and sorts each
    /// node's children by them.
    fn add_retained_sizes(&mut self, sizes: &NodeMap<u64>) {
        // Every node after all of its children: reverse depth-first order.
        let mut order = Vec::new();
        let mut stack = self.top.clone();
        while let Some(node) = stack.pop() {
            order.push(node);
            stack.extend_from_slice(self.children(node));
        }
        for &node in order.iter().rev() {
            // Sizes stop at the largest `u64` rather than overflow: in a
            // damaged file, items can overlap, and their sizes add up to
            // more than a `u64` holds.
            let at = sizes.position(node);
            let below = self.children[at]
                .iter()
                .map(|&child| self.retained(child))
                .fold(0, u64::saturating_add);
            self.retained[at] = sizes[at].saturating_add(below);
        }
        let by_size = largest_first(&self.retained);
        for children in self.children.values_mut() {
            children.sort_by(&by_size);
        }
        self.top.sort_by(&by_size);
    }
}

/// The order of the nodes under each node of the tree, and under its top,
/// given each node's `retained` size: largest first, and nodes of the same
/// size in the order of their positions.
fn largest_first(retained: &NodeMap<u64>) -> impl Fn(&Node, &Node) -> Ordering + '_ {
    move |a, b| retained[*b].cmp(&retained[*a]).then(a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    use Node::Item;

    /// A tree for items with the given sizes, where item 0 is the only root.
    fn tree(sizes: &[u64], edges: &[(usize, usize)]) -> DominatorTree {
        let mut node_sizes = NodeMap::new(sizes.len(), 0, 0);
        for (item, &size) in sizes.iter().enumerate() {
            node_sizes[Item(item)] = size;
        }
        let edges = edges.iter().map(|&(from, to)| (Item(from), Item(to)));
        DominatorTree::from_edges(&node_sizes, &[Item(0)], edges)
    }

    #[test]
    fn a_diamond_shares_its_bottom() {
        // 0 calls 1 and 2, which both call 3. Only 0 dominates 3.
        let tree = tree(&[1, 10, 100, 1000], &[(0, 1), (0, 2), (1, 3), (2, 3)]);
        assert_eq!(tree.children(Item(0)), [Item(3), Item(2), Item(1)]);
        assert_eq!(tree.retained(Item(1)), 10);
        assert_eq!(tree.retained(Item(2)), 100);
        assert_eq!(tree.retained(Item(0)), 1111);
        assert_eq!(tree.parent(Item(3)), Some(Item(0)));
    }

    #[test]
    fn a_chain_keeps_everything_below_it() {
        // 0 calls 1, which calls 2, which calls itself and 1 back.
        let tree = tree(&[1, 10, 100], &[(0, 1), (1, 2), (2, 2), (2, 1)]);
        assert_eq!(tree.retained(Item(1)), 110);
        assert_eq!(tree.exclusive(Item(1)), [Item(2)]);
        assert_eq!(tree.exclusive(Item(0)).len(), 2);
    }

    #[test]
    fn unreachable_items_are_outside_the_tree() {
        // Nothing reaches 2, which calls 1.
        let tree = tree(&[1, 10, 100], &[(0, 1), (2, 1)]);
        assert!(!tree.is_reachable(Item(2)));
        assert_eq!(tree.parent(Item(1)), Some(Item(0)));
        assert_eq!(tree.retained(Item(0)), 11);
        assert_eq!(tree.retained(Item(2)), 100);
    }
}
