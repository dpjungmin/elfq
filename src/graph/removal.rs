//! What removing several items at once would remove.
//!
//! Removing one item removes it and everything exclusive to it, which the
//! dominator tree holds under it. Removing several at once can remove more:
//! an item that two of them use, and nothing else does, goes only when both
//! go, and so hangs under neither in the tree. So [`Removal`] searches the
//! graph once more, from the roots, never entering a removed node. A node
//! is *lost* when something reaches it and this search doesn't.
//!
//! What is lost is a set of whole branches of the tree: every chain to a
//! node below a lost node passes through the lost one, so the search
//! reaches neither. The *tops* of those branches are the lost nodes whose
//! parent in the tree isn't lost, so their retained sizes add up to the
//! bytes of every lost node. A removed node that nothing reaches isn't
//! lost, since nothing reached it before either, but removing it saves its
//! own bytes too.
//!
//! A removed node *matters* when the search stopped at it: it is a root,
//! or a node that the search reaches uses it. Any other removed node that
//! something reaches adds nothing. Only lost nodes, and nodes that nothing
//! reaches, use it, so every chain from a root to it passes another
//! removed node.
//!
//! A top that isn't removed is *kept together* by its *keepers*: the
//! removed nodes that matter and lead to it through lost nodes that don't
//! matter. One walk back from the top finds them. Removing just the
//! keepers loses the top, and removing all of them but one keeps it, so
//! there are at least two. [`Removal::find_kept_together`] says why.
//!
//! For example, in testdata/bin/c-x86_64.elf, `main` calls
//! `process_frame` and `report`, and only `process_frame` calls `crc32`.
//! Only `process_frame` and `report` use `stats`, `frames_ok` and
//! `frames_bad`, and only they and what they keep call
//! `__stack_chk_fail@plt`. Removing `process_frame`, `report` and `crc32`
//! loses the three of them, what each keeps, and those four shared nodes.
//! The tops are `process_frame`, `report` and the four shared nodes.
//! `process_frame` and `report` matter, since `main` calls them. `crc32`
//! doesn't, since `process_frame` keeps it, so it adds nothing. Each
//! shared node is kept together by `process_frame` and `report`: removing
//! either of them alone keeps it.

use super::{DominatorTree, Graph, Node, index, node_at};
use crate::binary::{Binary, total};

/// What removing several nodes at once would remove: the nodes that are
/// lost, the tops of what is lost, and the keepers of each top that isn't
/// removed (see the module doc).
#[derive(Debug)]
pub struct Removal {
    /// What removing the nodes does to each node, indexed by
    /// `graph::index`.
    fates: Vec<Fate>,
    /// The lost nodes whose parent in the tree isn't lost, largest retained
    /// size first.
    tops: Vec<Node>,
    /// What keeps each of `tops` that isn't removed, in their order.
    kept_together: Vec<KeptTogether>,
    /// What removing the nodes saves: the retained sizes of `tops`, and the
    /// own bytes of the removed nodes that nothing reaches.
    retained: u64,
    /// How many items the binary has. `fates` is indexed by
    /// `graph::index`, which puts the imports after the items.
    items: usize,
}

/// A top that no one removed node keeps, and the removed nodes that keep it
/// together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptTogether {
    /// The top: a lost node that isn't removed.
    pub top: Node,
    /// The removed nodes that matter and lead to `top` through lost nodes
    /// that don't matter, sorted. A removed node that matters but leads to
    /// `top` only through one of these isn't one. Removing just them loses
    /// `top`, there are at least two, and removing all of them but one
    /// keeps it.
    pub keepers: Vec<Node>,
}

/// What removing the nodes does to one node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// It isn't removed and isn't lost: the search reaches it, or nothing
    /// does.
    Stays,
    /// It isn't removed, but it is lost: something reaches it, and the
    /// search doesn't.
    Freed,
    /// It is removed, and the search stopped at it: it is a root, or a
    /// node that the search reaches uses it.
    Matters,
    /// It is removed, and something reaches it, but the search didn't
    /// stop at it: it isn't a root, and only lost nodes, and nodes that
    /// nothing reaches, use it. Every chain from a root to it passes
    /// another removed node, so it adds nothing.
    KeptByOthers,
    /// It is removed, and nothing reaches it.
    Unreachable,
}

impl Removal {
    /// Works out what removing the nodes in `removed` at once would remove
    /// from `binary`, whose `graph` and `tree` are given. A node given twice
    /// counts once. Sizes are the tree's: with a range, only the bytes in
    /// it count.
    pub fn find(binary: &Binary, graph: &Graph, tree: &DominatorTree, removed: &[Node]) -> Removal {
        let items = binary.items.len();
        Removal::find_in(items, items + binary.imports.len(), graph, tree, removed)
    }

    /// Works out the removal in a graph of `count` nodes, of which the
    /// first `items` are items and the rest imports.
    fn find_in(
        items: usize,
        count: usize,
        graph: &Graph,
        tree: &DominatorTree,
        removed: &[Node],
    ) -> Removal {
        // Until the search says otherwise, every node that something
        // reaches is lost, and every removed one adds nothing.
        let mut fates: Vec<Fate> = (0..count)
            .map(|index| match tree.is_reachable(node_at(items, index)) {
                true => Fate::Freed,
                false => Fate::Stays,
            })
            .collect();
        for &node in removed {
            fates[index(items, node)] = match tree.is_reachable(node) {
                true => Fate::KeptByOthers,
                false => Fate::Unreachable,
            };
        }
        let mut removal = Removal {
            fates,
            tops: Vec::new(),
            kept_together: Vec::new(),
            retained: 0,
            items,
        };
        removal.search(graph);
        removal.tops = removal.find_tops(tree);
        removal.kept_together = removal.find_kept_together(graph);
        removal.retained = removal.find_retained(tree);
        removal
    }

    /// Whether removing the nodes loses `node`: something reaches it, and
    /// a search from the roots that never enters a removed node doesn't.
    pub fn is_lost(&self, node: Node) -> bool {
        match self.fate(node) {
            Fate::Freed | Fate::Matters | Fate::KeptByOthers => true,
            Fate::Stays | Fate::Unreachable => false,
        }
    }

    /// Whether `node` is one of the removed nodes.
    pub fn is_removed(&self, node: Node) -> bool {
        match self.fate(node) {
            Fate::Matters | Fate::KeptByOthers | Fate::Unreachable => true,
            Fate::Stays | Fate::Freed => false,
        }
    }

    /// Whether `node` is a removed node that matters: a root, or one that a
    /// node that the search reaches uses. A removed node that something
    /// reaches and that doesn't matter adds nothing: removing the others
    /// alone loses the same nodes.
    pub fn matters(&self, node: Node) -> bool {
        self.fate(node) == Fate::Matters
    }

    /// Whether `node` is a removed node that the other removed nodes keep,
    /// alone or together: something reaches it, but it doesn't matter, so
    /// it adds nothing.
    pub(crate) fn is_kept_by_others(&self, node: Node) -> bool {
        self.fate(node) == Fate::KeptByOthers
    }

    /// The lost nodes, in the order of `graph::index`: the items, then the
    /// imports.
    pub fn lost(&self) -> impl Iterator<Item = Node> + '_ {
        self.nodes().filter(|&node| self.is_lost(node))
    }

    /// The tops of what is lost, largest retained size first: the lost
    /// nodes whose parent in the tree isn't lost, much as
    /// [`DominatorTree::top`] lists the nodes with no parent at all. Each is
    /// a removed node, or a node that several removed nodes keep together.
    pub fn tops(&self) -> &[Node] {
        &self.tops
    }

    /// What keeps each top that isn't removed, in the order of
    /// [`Removal::tops`].
    pub fn kept_together(&self) -> &[KeptTogether] {
        &self.kept_together
    }

    /// The removed nodes that nothing reaches, in the order of
    /// `graph::index`.
    pub fn unreachable(&self) -> impl Iterator<Item = Node> + '_ {
        self.nodes()
            .filter(|&node| self.fate(node) == Fate::Unreachable)
    }

    /// What removing the nodes would save: the bytes of every lost node,
    /// which the tops' retained sizes add up to, and the own bytes of the
    /// removed nodes that nothing reaches. For one node that something
    /// reaches, its retained size.
    pub fn retained(&self) -> u64 {
        self.retained
    }

    /// Searches from the roots, never entering a removed node: each node
    /// that it reaches stays, and each removed node where it stops matters.
    /// It goes around several nodes where `why`'s search goes around one
    /// ([`ShortestPaths::avoiding`]), and keeps no paths.
    ///
    /// [`ShortestPaths::avoiding`]: super::ShortestPaths::avoiding
    fn search(&mut self, graph: &Graph) {
        let mut next = Vec::new();
        for root in &graph.roots {
            self.meet(root.node, &mut next);
        }
        while let Some(item) = next.pop() {
            for reference in graph.references_from(item) {
                self.meet(reference.to, &mut next);
            }
        }
    }

    /// Notes that the search came to `node`, from a root or from a node
    /// that it reached. If `node` isn't removed and is still counted as
    /// freed, which every node that something reaches is until the search
    /// comes to it, it stays, and the search goes on from it. If it is
    /// removed, it matters.
    fn meet(&mut self, node: Node, next: &mut Vec<usize>) {
        let at = self.index(node);
        match self.fates[at] {
            Fate::Freed => {
                self.fates[at] = Fate::Stays;
                // Imports use nothing in the file.
                if let Node::Item(item) = node {
                    next.push(item);
                }
            }
            Fate::KeptByOthers => self.fates[at] = Fate::Matters,
            Fate::Stays | Fate::Matters | Fate::Unreachable => {}
        }
    }

    /// The lost nodes whose parent in the tree isn't lost, largest retained
    /// size first, as the tree sorts the nodes under each node.
    fn find_tops(&self, tree: &DominatorTree) -> Vec<Node> {
        let mut tops: Vec<Node> = self
            .lost()
            .filter(|&node| !tree.parent(node).is_some_and(|parent| self.is_lost(parent)))
            .collect();
        tree.sort_largest_first(&mut tops);
        tops
    }

    /// For each top that isn't removed, its keepers: one walk back from it,
    /// against the references, through lost nodes, that stops at each node
    /// that matters and goes on through the others. So the keepers are the
    /// first nodes that matter that the walk meets.
    ///
    /// Removing just the keepers loses the top. Take any chain of uses from
    /// a root to the top. The node where it last comes into the lost nodes
    /// matters: it is a root, or a node that the search reaches uses it.
    /// From there to the top, the chain runs through lost nodes, so the
    /// walk, going back along it, meets a node that matters: that one, or
    /// one nearer the top. So every such chain passes a keeper.
    ///
    /// Removing all the keepers but one keeps the top. The search stopped
    /// at that one, so a chain from a root comes to it without passing a
    /// removed node. The walk found a way on from it to the top that passes
    /// no other node that matters, and so no other keeper.
    ///
    /// So there are at least two keepers. If one alone lost the top, every
    /// chain to the top would pass it, so the top would hang below it in
    /// the tree, under a lost node, and wouldn't be a top.
    ///
    /// Each walk may go over lost nodes that an earlier walk went over, so
    /// the walks take up to the number of these tops times the number of
    /// references between lost nodes. Real removals stay far below that:
    /// removing the 962 functions named `cargo::ops::` from cargo loses
    /// 36,101 nodes and keeps 4,038 tops together, and all of
    /// [`Removal::find`] takes 43 ms in a release build. Should that ever
    /// matter, a walk could go from a lost node straight to the top of its
    /// branch: a node that matters is always a top, and of the nodes that
    /// something reaches, only a branch's own nodes use those below its
    /// top.
    fn find_kept_together(&self, graph: &Graph) -> Vec<KeptTogether> {
        let tops: Vec<Node> = (self.tops.iter().copied())
            .filter(|&top| !self.is_removed(top))
            .collect();
        if tops.is_empty() {
            return Vec::new();
        }
        let users = self.lost_users(graph);
        // The top whose walk last met each node, as an index into `tops`,
        // so that each walk meets each node once.
        let mut met: Vec<Option<usize>> = vec![None; self.fates.len()];
        let mut kept_together = Vec::with_capacity(tops.len());
        for (walk, top) in tops.into_iter().enumerate() {
            met[self.index(top)] = Some(walk);
            let mut keepers = Vec::new();
            let mut next = vec![top];
            while let Some(node) = next.pop() {
                for &user in &users[self.index(node)] {
                    let at = self.index(user);
                    if met[at] == Some(walk) {
                        continue;
                    }
                    met[at] = Some(walk);
                    if self.matters(user) {
                        keepers.push(user);
                    } else {
                        next.push(user);
                    }
                }
            }
            keepers.sort_unstable();
            kept_together.push(KeptTogether { top, keepers });
        }
        kept_together
    }

    /// What removing the nodes saves: the retained sizes of the tops, and
    /// those of the removed nodes that nothing reaches, which are their own,
    /// since nothing is exclusive to them.
    fn find_retained(&self, tree: &DominatorTree) -> u64 {
        let saved = (self.tops.iter().copied()).chain(self.unreachable());
        total(saved.map(|node| tree.retained(node)))
    }

    /// For each lost node, indexed by `graph::index`, the lost items that
    /// use it: the references that a walk back from a top goes against.
    fn lost_users(&self, graph: &Graph) -> Vec<Vec<Node>> {
        let mut users = vec![Vec::new(); self.fates.len()];
        for reference in &graph.references {
            let user = Node::Item(reference.from);
            if self.is_lost(user) && self.is_lost(reference.to) {
                users[self.index(reference.to)].push(user);
            }
        }
        users
    }

    /// Every node of the graph, in the order of `graph::index`.
    fn nodes(&self) -> impl Iterator<Item = Node> + '_ {
        (0..self.fates.len()).map(|index| node_at(self.items, index))
    }

    fn fate(&self, node: Node) -> Fate {
        self.fates[self.index(node)]
    }

    fn index(&self, node: Node) -> usize {
        index(self.items, node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::graph::{ReferenceKind, Root, RootKind};

    use Node::{Import, Item};

    /// A graph built by hand, and its tree.
    struct Example {
        graph: Graph,
        tree: DominatorTree,
        /// How many items it has.
        items: usize,
        /// How many nodes it has, imports included.
        count: usize,
    }

    impl Example {
        /// Items of the given sizes and `imports` imports, where `roots`
        /// are the roots and each of `uses` is an item's call to a node.
        /// The tests give items sizes such as 1, 10 and 100, so that the
        /// digits of a total say which items it counts.
        fn new(sizes: &[u64], imports: usize, roots: &[usize], uses: &[(usize, Node)]) -> Example {
            let roots = roots.iter().map(|&root| Root {
                node: Item(root),
                kind: RootKind::Entry,
            });
            let mut graph = Graph {
                roots: roots.collect(),
                ..Graph::default()
            };
            for &(from, to) in uses {
                graph.add(from, to, ReferenceKind::Call, 0);
            }
            graph.references.sort_by_key(|reference| reference.from);
            let items = sizes.len();
            let mut sizes = sizes.to_vec();
            // Imports have no bytes in the file.
            sizes.resize(items + imports, 0);
            let tree = DominatorTree::from_graph(items, &sizes, &graph);
            let count = sizes.len();
            Example {
                graph,
                tree,
                items,
                count,
            }
        }

        /// Items of the given sizes, which call each other, where item 0
        /// is the only root.
        fn calls(sizes: &[u64], calls: &[(usize, usize)]) -> Example {
            let uses: Vec<(usize, Node)> =
                (calls.iter()).map(|&(from, to)| (from, Item(to))).collect();
            Example::new(sizes, 0, &[0], &uses)
        }

        fn remove(&self, removed: &[Node]) -> Removal {
            Removal::find_in(self.items, self.count, &self.graph, &self.tree, removed)
        }
    }

    fn lost(removal: &Removal) -> Vec<Node> {
        removal.lost().collect()
    }

    fn kept_together(top: Node, keepers: &[Node]) -> KeptTogether {
        KeptTogether {
            top,
            keepers: keepers.to_vec(),
        }
    }

    #[test]
    fn an_item_that_another_keeps_adds_nothing() {
        // 0 calls 1 and 3; 1 calls 2.
        let example = Example::calls(&[1, 10, 100, 1000], &[(0, 1), (1, 2), (0, 3)]);
        let removal = example.remove(&[Item(1), Item(2)]);
        assert_eq!(lost(&removal), [Item(1), Item(2)]);
        assert_eq!(removal.tops(), [Item(1)]);
        assert!(removal.matters(Item(1)) && !removal.is_kept_by_others(Item(1)));
        // Only 1 leads to 2.
        assert!(removal.is_removed(Item(2)) && !removal.matters(Item(2)));
        assert!(removal.is_kept_by_others(Item(2)));
        assert_eq!(removal.kept_together(), []);
        assert_eq!(removal.retained(), 110);
    }

    #[test]
    fn a_root_can_be_removed() {
        // Roots 0 and 3. 0 calls 1 and 2; 3 calls 2.
        let uses = [(0, Item(1)), (0, Item(2)), (3, Item(2))];
        let example = Example::new(&[1, 10, 100, 1000], 0, &[0, 3], &uses);
        let removal = example.remove(&[Item(0)]);
        assert_eq!(lost(&removal), [Item(0), Item(1)]);
        assert_eq!(removal.tops(), [Item(0)]);
        assert!(removal.matters(Item(0)));
        assert_eq!(removal.retained(), 11);
    }

    #[test]
    fn an_item_that_nothing_reaches_counts_its_own_bytes() {
        // 0 calls 1. Nothing reaches 2, which calls 1 too.
        let example = Example::calls(&[1, 10, 100], &[(0, 1), (2, 1)]);
        let removal = example.remove(&[Item(2)]);
        assert_eq!(lost(&removal), []);
        assert_eq!(removal.tops(), []);
        assert_eq!(removal.unreachable().collect::<Vec<_>>(), [Item(2)]);
        assert!(removal.is_removed(Item(2)) && !removal.matters(Item(2)));
        // Nothing keeps it, so it isn't kept by the others either.
        assert!(!removal.is_kept_by_others(Item(2)));
        assert_eq!(removal.retained(), 100);
        let removal = example.remove(&[Item(2), Item(1)]);
        assert_eq!(lost(&removal), [Item(1)]);
        assert_eq!(removal.retained(), 110);
    }

    #[test]
    fn a_cycle_is_lost_whole() {
        // 0 calls 1 and 2. 1 calls 3, 2 calls 4, and 3 and 4 call each
        // other.
        let calls = [(0, 1), (0, 2), (1, 3), (2, 4), (3, 4), (4, 3)];
        let example = Example::calls(&[1, 10, 20, 100, 200], &calls);
        // 2 still leads to 4, and through it to 3.
        assert_eq!(lost(&example.remove(&[Item(1)])), [Item(1)]);
        let removal = example.remove(&[Item(1), Item(2)]);
        assert_eq!(lost(&removal), [Item(1), Item(2), Item(3), Item(4)]);
        // Each of 3 and 4 hangs under 0, and is reached from 1 and 2.
        assert_eq!(removal.tops(), [Item(4), Item(3), Item(2), Item(1)]);
        let both = [Item(1), Item(2)];
        assert_eq!(
            removal.kept_together(),
            [kept_together(Item(4), &both), kept_together(Item(3), &both)]
        );
        assert_eq!(removal.retained(), 330);
    }

    #[test]
    fn two_items_keep_together_what_only_they_use() {
        // 0 calls 1 and 2, which both call 3, which calls 4.
        let calls = [(0, 1), (0, 2), (1, 3), (2, 3), (3, 4)];
        let example = Example::calls(&[1, 10, 20, 100, 1000], &calls);
        assert_eq!(lost(&example.remove(&[Item(1)])), [Item(1)]);
        let removal = example.remove(&[Item(1), Item(2)]);
        assert_eq!(removal.tops(), [Item(3), Item(2), Item(1)]);
        assert_eq!(
            removal.kept_together(),
            [kept_together(Item(3), &[Item(1), Item(2)])]
        );
        assert_eq!(removal.retained(), 1130);
    }

    #[test]
    fn three_items_can_keep_one_together() {
        // 0 calls 1, 2 and 3, which all call 4.
        let calls = [(0, 1), (0, 2), (0, 3), (1, 4), (2, 4), (3, 4)];
        let example = Example::calls(&[1, 10, 20, 30, 100], &calls);
        let removal = example.remove(&[Item(1), Item(2), Item(3)]);
        assert_eq!(
            removal.kept_together(),
            [kept_together(Item(4), &[Item(1), Item(2), Item(3)])]
        );
    }

    #[test]
    fn only_the_removed_items_that_matter_keep_a_top() {
        // Roots 0 and 1, like `main` and an interrupt handler. 0 calls 2 and
        // 4, 2 calls 4, 1 calls 3, and 3 calls 4.
        let uses = [
            (0, Item(2)),
            (0, Item(4)),
            (2, Item(4)),
            (1, Item(3)),
            (3, Item(4)),
        ];
        let example = Example::new(&[1, 2, 10, 20, 100], 0, &[0, 1], &uses);
        let removal = example.remove(&[Item(0), Item(2), Item(3)]);
        assert_eq!(lost(&removal), [Item(0), Item(2), Item(3), Item(4)]);
        // 0 is a root, and 1 stays and calls 3; only 0 calls 2.
        assert!(removal.matters(Item(0)) && removal.matters(Item(3)));
        assert!(!removal.matters(Item(2)));
        assert_eq!(removal.tops(), [Item(4), Item(3), Item(0)]);
        assert_eq!(
            removal.kept_together(),
            [kept_together(Item(4), &[Item(0), Item(3)])]
        );
    }

    #[test]
    fn a_removed_item_that_matters_behind_a_keeper_isnt_one() {
        // Roots 0 and 4. 0 calls 1 and 3, 4 calls 2, 1 calls 2, and 2 and 3
        // call 5.
        let uses = [
            (0, Item(1)),
            (0, Item(3)),
            (4, Item(2)),
            (1, Item(2)),
            (2, Item(5)),
            (3, Item(5)),
        ];
        let example = Example::new(&[1, 10, 20, 30, 2, 100], 0, &[0, 4], &uses);
        let removal = example.remove(&[Item(1), Item(2), Item(3)]);
        assert_eq!(lost(&removal), [Item(1), Item(2), Item(3), Item(5)]);
        // 0 calls 1 and 3, and 4 calls 2.
        let matters = [Item(1), Item(2), Item(3)];
        assert!(matters.iter().all(|&node| removal.matters(node)));
        // 1 leads to 5 only through 2: removing 1 and 3 keeps 5, which 4
        // reaches through 2, so 1 isn't one of 5's keepers.
        assert_eq!(
            removal.kept_together(),
            [kept_together(Item(5), &[Item(2), Item(3)])]
        );
        assert!(!lost(&example.remove(&[Item(1), Item(3)])).contains(&Item(5)));
    }

    #[test]
    fn a_keeper_can_lie_behind_a_removed_item_that_doesnt_matter() {
        // 0 calls 1 and 3, 1 calls 2, and 2 and 3 call 4.
        let calls = [(0, 1), (0, 3), (1, 2), (2, 4), (3, 4)];
        let example = Example::calls(&[1, 10, 20, 30, 100], &calls);
        let removal = example.remove(&[Item(1), Item(2), Item(3)]);
        // Only 1 calls 2, so the walk back from 4 goes on through 2 to 1.
        assert!(!removal.matters(Item(2)));
        assert_eq!(
            removal.kept_together(),
            [kept_together(Item(4), &[Item(1), Item(3)])]
        );
    }

    #[test]
    fn each_top_has_its_own_keepers() {
        // 0 calls 1, 2, 3 and 4. 1 and 2 call 5, and 3 and 4 call 6.
        let calls = [
            (0, 1),
            (0, 2),
            (0, 3),
            (0, 4),
            (1, 5),
            (2, 5),
            (3, 6),
            (4, 6),
        ];
        let example = Example::calls(&[1, 10, 20, 30, 40, 100, 100], &calls);
        let removal = example.remove(&[Item(1), Item(2), Item(3), Item(4)]);
        // 5 and 6 are as large, so they come in the order of their indexes,
        // as the tree sorts nodes of the same size.
        let tops = [Item(5), Item(6), Item(4), Item(3), Item(2), Item(1)];
        assert_eq!(removal.tops(), tops);
        assert_eq!(
            removal.kept_together(),
            [
                kept_together(Item(5), &[Item(1), Item(2)]),
                kept_together(Item(6), &[Item(3), Item(4)])
            ]
        );
    }

    #[test]
    fn an_item_that_the_others_keep_together_adds_nothing() {
        // 0 calls 1 and 2, which both call 3.
        let example = Example::calls(&[1, 10, 20, 100], &[(0, 1), (0, 2), (1, 3), (2, 3)]);
        let removal = example.remove(&[Item(1), Item(2), Item(3)]);
        assert_eq!(removal.tops(), [Item(3), Item(2), Item(1)]);
        assert!(!removal.matters(Item(3)) && removal.is_kept_by_others(Item(3)));
        // A top that is removed has no keepers to name.
        assert_eq!(removal.kept_together(), []);
        let without = example.remove(&[Item(1), Item(2)]);
        assert_eq!(lost(&removal), lost(&without));
        assert_eq!(removal.retained(), without.retained());
    }

    #[test]
    fn a_node_given_twice_counts_once() {
        // 0 calls 1, which calls 2. Nothing reaches 3.
        let example = Example::calls(&[1, 10, 100, 1000], &[(0, 1), (1, 2)]);
        let once = example.remove(&[Item(1), Item(3)]);
        let twice = example.remove(&[Item(1), Item(3), Item(1), Item(3)]);
        assert_eq!(lost(&twice), lost(&once));
        assert_eq!(twice.tops(), once.tops());
        assert_eq!(twice.unreachable().collect::<Vec<_>>(), [Item(3)]);
        assert_eq!(twice.retained(), 1110);
    }

    #[test]
    fn an_import_can_be_removed_or_kept_together() {
        // 0 calls 1 and 2, which both call import 0.
        let uses = [(0, Item(1)), (0, Item(2)), (1, Import(0)), (2, Import(0))];
        let example = Example::new(&[1, 10, 20], 1, &[0], &uses);
        let removal = example.remove(&[Import(0)]);
        assert_eq!(lost(&removal), [Import(0)]);
        assert!(removal.matters(Import(0)));
        assert_eq!(removal.retained(), 0);
        let removal = example.remove(&[Item(1), Item(2)]);
        assert_eq!(lost(&removal), [Item(1), Item(2), Import(0)]);
        assert_eq!(
            removal.kept_together(),
            [kept_together(Import(0), &[Item(1), Item(2)])]
        );
    }
}
