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

use petgraph::algo::dominators::simple_fast;
use petgraph::graph::{DiGraph, NodeIndex};

use super::{Graph, Node, index};
use crate::binary::Binary;

/// The dominator tree of a binary's graph.
#[derive(Debug)]
pub struct DominatorTree {
    /// Each node's closest dominator, indexed by `graph::index`.
    /// `None` for nodes that no root reaches, and for the roots, which hang
    /// directly under the tree's top.
    parent: Vec<Option<Node>>,
    /// The nodes each node dominates directly, largest retained size first.
    children: Vec<Vec<Node>>,
    /// The nodes directly under the tree's top: the roots, and anything
    /// that several roots reach.
    top: Vec<Node>,
    reachable: Vec<bool>,
    retained: Vec<u64>,
    items: usize,
}

impl DominatorTree {
    pub fn build(binary: &Binary, graph: &Graph) -> DominatorTree {
        let mut sizes: Vec<u64> = binary.items.iter().map(|item| item.size).collect();
        // Imports have no bytes in the file.
        sizes.resize(binary.items.len() + binary.imports.len(), 0);
        let roots: Vec<Node> = graph.roots.iter().map(|root| root.node).collect();
        let edges = graph
            .references
            .iter()
            .map(|reference| (Node::Item(reference.from), reference.to));
        DominatorTree::from_edges(binary.items.len(), &sizes, &roots, edges)
    }

    /// Builds the tree for a graph of `sizes.len()` nodes, of which the
    /// first `items` are items and the rest imports.
    fn from_edges(
        items: usize,
        sizes: &[u64],
        roots: &[Node],
        edges: impl Iterator<Item = (Node, Node)>,
    ) -> DominatorTree {
        let count = sizes.len();
        // Node 0 is a virtual top that points at every root; node i + 1 is
        // the graph node with index i.
        let mut petgraph = DiGraph::<(), ()>::with_capacity(count + 1, count);
        for _ in 0..=count {
            petgraph.add_node(());
        }
        let vertex = |node: Node| NodeIndex::new(index(items, node) + 1);
        let top = NodeIndex::new(0);
        for &root in roots {
            petgraph.add_edge(top, vertex(root), ());
        }
        for (from, to) in edges {
            petgraph.add_edge(vertex(from), vertex(to), ());
        }
        let dominators = simple_fast(&petgraph, top);

        let node = |vertex: NodeIndex| {
            let i = vertex.index() - 1;
            if i < items {
                Node::Item(i)
            } else {
                Node::Import(i - items)
            }
        };
        let mut tree = DominatorTree {
            parent: vec![None; count],
            children: vec![Vec::new(); count],
            top: Vec::new(),
            reachable: vec![false; count],
            retained: vec![0; count],
            items,
        };
        for i in 0..count {
            let Some(dominator) = dominators.immediate_dominator(NodeIndex::new(i + 1)) else {
                continue;
            };
            tree.reachable[i] = true;
            if dominator == top {
                tree.top.push(node(NodeIndex::new(i + 1)));
            } else {
                let parent = node(dominator);
                tree.parent[i] = Some(parent);
                tree.children[index(items, parent)].push(node(NodeIndex::new(i + 1)));
            }
        }
        tree.add_retained_sizes(sizes);
        tree
    }

    /// Whether any path from the roots reaches `node`.
    pub fn is_reachable(&self, node: Node) -> bool {
        self.reachable[self.index(node)]
    }

    /// The size of `node` plus the sizes of everything exclusive to it.
    pub fn retained(&self, node: Node) -> u64 {
        self.retained[self.index(node)]
    }

    /// The closest node that every path to `node` passes through. `None` for
    /// the roots, for nodes that several roots reach, and for unreachable
    /// nodes.
    pub fn parent(&self, node: Node) -> Option<Node> {
        self.parent[self.index(node)]
    }

    /// The nodes that `node` dominates directly, largest retained size
    /// first.
    pub fn children(&self, node: Node) -> &[Node] {
        &self.children[self.index(node)]
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
    fn add_retained_sizes(&mut self, sizes: &[u64]) {
        // Every node after all of its children: reverse depth-first order.
        let mut order = Vec::new();
        let mut stack = self.top.clone();
        while let Some(node) = stack.pop() {
            order.push(node);
            stack.extend_from_slice(self.children(node));
        }
        for &node in order.iter().rev() {
            let own = sizes[self.index(node)];
            let below: u64 = self
                .children(node)
                .iter()
                .map(|&child| self.retained(child))
                .sum();
            let i = self.index(node);
            self.retained[i] = own + below;
        }
        let retained = &self.retained;
        let items = self.items;
        let by_size = |a: &Node, b: &Node| {
            retained[index(items, *b)]
                .cmp(&retained[index(items, *a)])
                .then(a.cmp(b))
        };
        for children in &mut self.children {
            children.sort_by(by_size);
        }
        self.top.sort_by(by_size);
    }

    fn index(&self, node: Node) -> usize {
        index(self.items, node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use Node::Item;

    /// A tree for items with the given sizes, where item 0 is the only root.
    fn tree(sizes: &[u64], edges: &[(usize, usize)]) -> DominatorTree {
        let edges = edges.iter().map(|&(from, to)| (Item(from), Item(to)));
        DominatorTree::from_edges(sizes.len(), sizes, &[Item(0)], edges)
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
    }
}
