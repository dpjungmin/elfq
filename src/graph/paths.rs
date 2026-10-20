//! Shortest paths: the fewest references that lead from a root to each item.
//!
//! A breadth-first search from all the roots at once reaches each node first
//! through one of the shortest paths to it, and remembers the reference that
//! got it there. Following those references back gives the path. Where
//! several paths are equally short, it takes the one with the fewest
//! references that aren't calls, since a chain of calls is the easiest to
//! follow.
//!
//! `why` asks what keeps an item X in the file. Every path to X ends with an
//! item that uses X, so the search is made without passing through X: the
//! users it reaches are the ones that keep X, and a user it can't reach is
//! either unreachable or there only because of X.

use std::collections::VecDeque;

use super::{Graph, Node, Reference, index};
use crate::binary::Binary;

/// How a search from the roots first reached each node.
#[derive(Debug)]
pub struct ShortestPaths {
    /// The reference that first reached each node, indexed by
    /// `graph::index`. `None` for roots and for nodes the search didn't
    /// reach.
    via: Vec<Option<Reference>>,
    /// How many references lead to each node, or `None` if the search
    /// didn't reach it.
    distance: Vec<Option<usize>>,
    /// How many of those references aren't calls.
    not_calls: Vec<usize>,
    items: usize,
}

impl ShortestPaths {
    /// Searches from the roots without passing through `avoid`.
    pub fn avoiding(binary: &Binary, graph: &Graph, avoid: Node) -> ShortestPaths {
        let items = binary.items.len();
        ShortestPaths::search(items, items + binary.imports.len(), graph, avoid)
    }

    /// Searches a graph of `count` nodes, of which the first `items` are
    /// items and the rest imports.
    fn search(items: usize, count: usize, graph: &Graph, avoid: Node) -> ShortestPaths {
        let mut paths = ShortestPaths {
            via: vec![None; count],
            distance: vec![None; count],
            not_calls: vec![0; count],
            items,
        };
        let mut queue = VecDeque::new();
        for root in &graph.roots {
            if root.node != avoid && !paths.is_reached(root.node) {
                paths.distance[index(items, root.node)] = Some(0);
                queue.push_back(root.node);
            }
        }
        // Nodes leave the queue in order of distance, so every path of one
        // length is seen before any longer one.
        while let Some(node) = queue.pop_front() {
            // Imports use nothing in the file.
            let Node::Item(item) = node else { continue };
            let from = index(items, node);
            let distance = paths.distance[from].map(|d| d + 1);
            for reference in graph.references_from(item) {
                if reference.to == avoid {
                    continue;
                }
                let to = index(items, reference.to);
                let not_calls = paths.not_calls[from] + usize::from(!reference.kind.is_call());
                let first = paths.distance[to].is_none();
                // Paths of the same length are all seen before any node
                // they reach leaves the queue, so the choice can change
                // until then.
                let better = paths.distance[to] == distance && not_calls < paths.not_calls[to];
                if first || better {
                    paths.distance[to] = distance;
                    paths.not_calls[to] = not_calls;
                    paths.via[to] = Some(*reference);
                }
                if first {
                    queue.push_back(reference.to);
                }
            }
        }
        paths
    }

    /// Whether the search reached `node`: it is a root, or a reached item
    /// uses it. Never true for the node that the search avoids.
    pub fn is_reached(&self, node: Node) -> bool {
        self.distance(node).is_some()
    }

    /// How many references the shortest path to `node` has, or `None` when
    /// the search didn't reach it.
    pub fn distance(&self, node: Node) -> Option<usize> {
        self.distance[index(self.items, node)]
    }

    /// The shortest path to `node`: the references along it, from the one a
    /// root makes to the one that reaches `node`. Empty when `node` is a
    /// root, and `None` when the search didn't reach it.
    pub fn path_to(&self, node: Node) -> Option<Vec<Reference>> {
        if !self.is_reached(node) {
            return None;
        }
        let mut path = Vec::new();
        let mut next = node;
        while let Some(reference) = self.via[index(self.items, next)] {
            path.push(reference);
            next = Node::Item(reference.from);
        }
        path.reverse();
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::graph::{ReferenceKind, Root, RootKind};

    /// A graph of calls between items, where item 0 is the only root.
    fn graph(calls: &[(usize, usize)]) -> Graph {
        let mut graph = Graph {
            roots: vec![Root {
                node: Node::Item(0),
                kind: RootKind::Entry,
            }],
            ..Graph::default()
        };
        for &(from, to) in calls {
            graph.add(from, Node::Item(to), ReferenceKind::Call, 0);
        }
        graph.references.sort_by_key(|reference| reference.from);
        graph
    }

    fn path(paths: &ShortestPaths, node: usize) -> Option<Vec<usize>> {
        let path = paths.path_to(Node::Item(node))?;
        Some(path.iter().map(|reference| reference.from).collect())
    }

    #[test]
    fn takes_the_fewest_steps() {
        // 0 calls 1, which calls 2, which calls 3; 0 also calls 3.
        let graph = graph(&[(0, 1), (1, 2), (2, 3), (0, 3)]);
        // Item 4 doesn't exist, so the search avoids nothing.
        let paths = ShortestPaths::search(4, 4, &graph, Node::Item(4));
        assert_eq!(path(&paths, 3), Some(vec![0]));
        assert_eq!(path(&paths, 2), Some(vec![0, 1]));
        assert_eq!(path(&paths, 0), Some(vec![]));
    }

    #[test]
    fn prefers_calls_among_equally_short_paths() {
        // 0 takes the address of 1, then calls 2; both call 3.
        let mut graph = graph(&[(1, 3), (2, 3)]);
        graph.add(0, Node::Item(1), ReferenceKind::Address, 0);
        graph.add(0, Node::Item(2), ReferenceKind::Call, 1);
        graph
            .references
            .sort_by_key(|reference| (reference.from, reference.site));
        let paths = ShortestPaths::search(4, 4, &graph, Node::Item(4));
        assert_eq!(path(&paths, 3), Some(vec![0, 2]));
    }

    #[test]
    fn goes_around_the_item_to_avoid() {
        // 0 calls 1 and 2; 1 calls 3; 2 calls 1 and 4.
        let graph = graph(&[(0, 1), (0, 2), (1, 3), (2, 1), (2, 4)]);
        let paths = ShortestPaths::search(5, 5, &graph, Node::Item(1));
        assert!(!paths.is_reached(Node::Item(1)));
        // Only 1 leads to 3.
        assert_eq!(path(&paths, 3), None);
        assert_eq!(path(&paths, 4), Some(vec![0, 2]));
    }
}
