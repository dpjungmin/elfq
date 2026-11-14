//! A value for each node of the graph, for the analyses that keep one: the
//! dominator tree, the shortest paths and the removal.

use std::ops::{Index, IndexMut};

use super::Node;

/// A value for each node of a graph of some items and imports, in one
/// list: the items in the order of [`Binary::items`], then the imports in
/// the order of [`Binary::imports`]. A node's place in the list is its
/// [`Position`].
///
/// [`Binary::items`]: crate::binary::Binary::items
/// [`Binary::imports`]: crate::binary::Binary::imports
#[derive(Debug, Clone)]
pub(super) struct NodeMap<T> {
    /// How many items the graph has. The imports come after them.
    items: usize,
    /// Each node's value, at its position.
    values: Vec<T>,
}

/// A node's place in a [`NodeMap`]: an item's index into
/// [`Binary::items`], or for an import, the number of items plus its index
/// into [`Binary::imports`]. The maps of one graph give each node the same
/// position, so a search that reads several of them can work out a node's
/// position once. The dominator tree numbers petgraph's vertices from it.
///
/// [`Binary::items`]: crate::binary::Binary::items
/// [`Binary::imports`]: crate::binary::Binary::imports
#[derive(Debug, Clone, Copy)]
pub(super) struct Position(pub(super) usize);

impl<T> NodeMap<T> {
    /// A map of `items` items and `imports` imports, each with `value`.
    pub(super) fn new(items: usize, imports: usize, value: T) -> NodeMap<T>
    where
        T: Clone,
    {
        NodeMap {
            items,
            values: vec![value; items + imports],
        }
    }

    /// A map of `items` items and `imports` imports, each with what `f`
    /// gives for it.
    pub(super) fn from_fn(items: usize, imports: usize, f: impl FnMut(Node) -> T) -> NodeMap<T> {
        let nodes = ((0..items).map(Node::Item)).chain((0..imports).map(Node::Import));
        NodeMap {
            items,
            values: nodes.map(f).collect(),
        }
    }

    /// A map of the same nodes, each with `value`.
    pub(super) fn with_each<U: Clone>(&self, value: U) -> NodeMap<U> {
        NodeMap {
            items: self.items,
            values: vec![value; self.values.len()],
        }
    }

    /// How many nodes the map has: the items and the imports.
    pub(super) fn len(&self) -> usize {
        self.values.len()
    }

    /// Every node, in the order of their positions: the items, then the
    /// imports.
    pub(super) fn nodes(&self) -> impl Iterator<Item = Node> {
        let items = self.items;
        (0..self.values.len()).map(move |position| node_at(items, Position(position)))
    }

    /// Every node's value, in the order of their positions, to change.
    pub(super) fn values_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.values.iter_mut()
    }

    /// The position of `node`.
    pub(super) fn position(&self, node: Node) -> Position {
        match node {
            Node::Item(item) => Position(item),
            Node::Import(import) => Position(self.items + import),
        }
    }

    /// The node at `position`: the opposite of [`NodeMap::position`].
    pub(super) fn node(&self, position: Position) -> Node {
        node_at(self.items, position)
    }
}

impl<T> Index<Node> for NodeMap<T> {
    type Output = T;

    fn index(&self, node: Node) -> &T {
        &self.values[self.position(node).0]
    }
}

impl<T> IndexMut<Node> for NodeMap<T> {
    fn index_mut(&mut self, node: Node) -> &mut T {
        let position = self.position(node);
        &mut self.values[position.0]
    }
}

impl<T> Index<Position> for NodeMap<T> {
    type Output = T;

    fn index(&self, position: Position) -> &T {
        &self.values[position.0]
    }
}

impl<T> IndexMut<Position> for NodeMap<T> {
    fn index_mut(&mut self, position: Position) -> &mut T {
        &mut self.values[position.0]
    }
}

/// The node at `position` in a map of `items` items, then the imports.
fn node_at(items: usize, position: Position) -> Node {
    if position.0 < items {
        Node::Item(position.0)
    } else {
        Node::Import(position.0 - items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use Node::{Import, Item};

    #[test]
    fn the_imports_come_after_the_items() {
        let mut map = NodeMap::new(2, 2, 0);
        map[Import(1)] = 3;
        map[Item(1)] = 1;
        let nodes: Vec<Node> = map.nodes().collect();
        assert_eq!(nodes, [Item(0), Item(1), Import(0), Import(1)]);
        assert_eq!(map.position(Import(1)).0, 3);
        assert_eq!(map.node(Position(2)), Import(0));
        assert_eq!(map[Position(3)], 3);
        assert_eq!(map[Position(1)], 1);
    }

    #[test]
    fn a_map_from_a_function_is_in_the_same_order() {
        let map = NodeMap::from_fn(2, 1, |node| node);
        assert_eq!(map[Position(1)], Item(1));
        assert_eq!(map[Position(2)], Import(0));
        let same = map.with_each(false);
        assert_eq!(same.len(), 3);
        assert_eq!(same.position(Import(0)).0, 2);
    }
}
