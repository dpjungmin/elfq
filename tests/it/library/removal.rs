//! What removing several items at once removes (`graph::Removal`), on every
//! sample.
//!
//! Removing one item must lose what is exclusive to it, and removing a few
//! must lose what two plain searches from the roots, before and after,
//! find, with the keepers of each top that `graph::KeptTogether` promises.
//! The tests on small graphs built by hand are in src/graph/removal.rs.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use elfq::binary::{AddressRange, Binary, Item};
use elfq::graph::{DominatorTree, Graph, Node, Removal};

use crate::support::{load_sample, node, samples};

/// A sample, with its graph and tree.
struct Sample {
    binary: Binary,
    graph: Graph,
    tree: DominatorTree,
}

impl Sample {
    fn load(path: &str) -> Sample {
        let binary = load_sample(path);
        let graph = Graph::build(&binary);
        let tree = DominatorTree::build(&binary, &graph, None);
        Sample {
            binary,
            graph,
            tree,
        }
    }

    fn remove(&self, removed: &[Node]) -> Removal {
        Removal::find(&self.binary, &self.graph, &self.tree, removed)
    }

    /// Every node: the items, then the imports.
    fn nodes(&self) -> impl Iterator<Item = Node> {
        let items = (0..self.binary.items.len()).map(Node::Item);
        items.chain((0..self.binary.imports.len()).map(Node::Import))
    }

    /// A node's own bytes: an item's size, and none for an import.
    fn own(&self, node: Node) -> u64 {
        match node {
            Node::Item(index) => self.binary.items[index].size,
            Node::Import(_) => 0,
        }
    }

    /// The nodes that `node` uses, each once, in the order of its
    /// references.
    fn uses(&self, node: Node) -> Vec<Node> {
        let Node::Item(item) = node else {
            return Vec::new();
        };
        let mut uses = Vec::new();
        for reference in self.graph.references_from(item) {
            if !uses.contains(&reference.to) {
                uses.push(reference.to);
            }
        }
        uses
    }

    /// A range over the middle of the file: from the start of the item a
    /// quarter of the way through it, by address, to the middle of the
    /// item three quarters of the way through, which it cuts in two.
    fn middle_range(&self) -> AddressRange {
        let mut items: Vec<&Item> = self.binary.items.iter().collect();
        items.sort_by_key(|item| item.address);
        let first = items[items.len() / 4];
        let last = items[items.len() * 3 / 4];
        let end = (last.address + last.size / 2).max(first.address + 1);
        let range = format!("{:#x}..{end:#x}", first.address);
        range.parse().unwrap()
    }

    /// The nodes that a search from the roots reaches, never entering
    /// `removed`, found without the tree.
    fn reachable_without(&self, removed: &HashSet<Node>) -> HashSet<Node> {
        let mut reached = HashSet::new();
        let mut next: Vec<Node> = Vec::new();
        for root in &self.graph.roots {
            if !removed.contains(&root.node) && reached.insert(root.node) {
                next.push(root.node);
            }
        }
        while let Some(node) = next.pop() {
            let Node::Item(item) = node else { continue };
            for reference in self.graph.references_from(item) {
                if !removed.contains(&reference.to) && reached.insert(reference.to) {
                    next.push(reference.to);
                }
            }
        }
        reached
    }

    /// What removing `removed` loses, found by two plain searches: what is
    /// reachable before, less what is reachable after. In the order of
    /// [`Sample::nodes`].
    fn lost(&self, before: &HashSet<Node>, removed: &HashSet<Node>) -> Vec<Node> {
        let after = self.reachable_without(removed);
        (self.nodes())
            .filter(|node| before.contains(node) && !after.contains(node))
            .collect()
    }

    /// Sets of up to 10 nodes to remove: items spread over the file, by
    /// their indexes; the items that use one node that several use, which
    /// may keep it together, with and without that node; a root, the nodes
    /// it uses and the nodes those use; and two roots with an item that
    /// nothing reaches.
    fn sets(&self, before: &HashSet<Node>) -> Vec<Vec<Node>> {
        let items = self.binary.items.len();
        let mut sets: Vec<Vec<Node>> = (0..12)
            .map(|set| {
                let size = 2 + set % 5;
                (0..size)
                    .map(|place| Node::Item((set * 7 + place * items / size) % items))
                    .collect()
            })
            .collect();
        let mut users: BTreeMap<Node, BTreeSet<usize>> = BTreeMap::new();
        for reference in &self.graph.references {
            users
                .entry(reference.to)
                .or_default()
                .insert(reference.from);
        }
        let shared: Vec<(&Node, &BTreeSet<usize>)> =
            users.iter().filter(|(_, users)| users.len() >= 2).collect();
        for &(&used, users) in shared.iter().step_by(shared.len().div_ceil(12).max(1)) {
            let users: Vec<Node> = users.iter().take(6).map(|&user| Node::Item(user)).collect();
            let mut with_used = vec![used];
            with_used.extend_from_slice(&users[..2]);
            sets.push(with_used);
            sets.push(users);
        }
        // A root, the nodes it uses and the nodes those use: here a removed
        // node that matters can lead to a top kept together only through
        // another that matters, and so isn't one of its keepers.
        for root in self.graph.roots.iter().take(12) {
            let mut set = vec![root.node];
            for used in self.uses(root.node).into_iter().take(3) {
                set.push(used);
                set.extend(self.uses(used).into_iter().take(2));
            }
            sets.push(set);
        }
        let roots = self.graph.roots.iter().map(|root| root.node).take(2);
        let unreachable = self.nodes().find(|node| !before.contains(node));
        sets.push(roots.chain(unreachable).collect());
        sets
    }
}

#[test]
fn removing_one_item_loses_what_is_exclusive_to_it() {
    for path in samples() {
        let sample = Sample::load(&path);
        for item in 0..sample.binary.items.len() {
            let node = Node::Item(item);
            let removal = sample.remove(&[node]);
            let name = &sample.binary.items[item].name;
            if !sample.tree.is_reachable(node) {
                assert_eq!(removal.lost().count(), 0, "{path}: {name}");
                let unreachable: Vec<Node> = removal.unreachable().collect();
                assert_eq!(unreachable, [node], "{path}: {name}");
                assert_eq!(removal.retained(), sample.own(node), "{path}: {name}");
                continue;
            }
            let mut expected = sample.tree.exclusive(node);
            expected.push(node);
            expected.sort();
            let lost: Vec<Node> = removal.lost().collect();
            assert_eq!(lost, expected, "{path}: {name}");
            assert_eq!(removal.tops(), [node], "{path}: {name}");
            assert_eq!(
                removal.retained(),
                sample.tree.retained(node),
                "{path}: {name}"
            );
            assert!(removal.matters(node), "{path}: {name}");
            assert_eq!(removal.kept_together(), [], "{path}: {name}");
            assert_eq!(removal.unreachable().count(), 0, "{path}: {name}");
        }
    }
}

#[test]
fn removing_several_items_loses_what_two_searches_find() {
    let mut kept_together = 0;
    for path in samples() {
        let sample = Sample::load(&path);
        let before = sample.reachable_without(&HashSet::new());
        // The plain search reaches what the tree does.
        for node in sample.nodes() {
            let reachable = sample.tree.is_reachable(node);
            assert_eq!(before.contains(&node), reachable, "{path}: {node:?}");
        }
        for set in sample.sets(&before) {
            let removal = sample.remove(&set);
            let removed: HashSet<Node> = set.iter().copied().collect();
            let context = format!("{path}: removing {set:?}");
            check_lost(&sample, &before, &removed, &removal, &context);
            check_matters(&sample, &before, &removed, &removal, &context);
            check_keepers(&sample, &removal, &context);
            kept_together += removal.kept_together().len();
        }
    }
    // Enough tops are kept together for the keepers' checks to count.
    assert!(kept_together > 1000, "{kept_together}");
}

#[test]
fn with_a_range_only_the_bytes_in_it_count() {
    let mut cut = 0;
    for path in samples() {
        let sample = Sample::load(&path);
        let range = sample.middle_range();
        let in_range = DominatorTree::build(&sample.binary, &sample.graph, Some(range));
        let own = own_bytes(&sample, &in_range);
        let before = sample.reachable_without(&HashSet::new());
        for set in sample.sets(&before) {
            let removal = sample.remove(&set);
            let removal_in_range = Removal::find(&sample.binary, &sample.graph, &in_range, &set);
            let context = format!("{path}: removing {set:?} in {range:?}");
            // The range changes only which bytes count.
            let lost: Vec<Node> = removal_in_range.lost().collect();
            assert_eq!(lost, removal.lost().collect::<Vec<_>>(), "{context}");
            let goes = lost.into_iter().chain(removal_in_range.unreachable());
            let bytes: u64 = goes.map(|node| own[&node]).sum();
            assert_eq!(removal_in_range.retained(), bytes, "{context}");
            if bytes < removal.retained() {
                cut += 1;
            }
        }
    }
    // Enough sets lose bytes outside the range for the check to count.
    assert!(cut > 100, "{cut}");
}

#[test]
fn the_example_in_the_module_doc_holds() {
    let sample = Sample::load("testdata/bin/c-x86_64.elf");
    let [process_frame, report, crc32, stats] =
        ["process_frame", "report", "crc32", "stats"].map(|name| node(&sample.binary, name));
    let removal = sample.remove(&[process_frame, report, crc32]);
    assert!(removal.is_lost(stats));
    assert_eq!(removal.tops().len(), 6);
    assert_eq!(removal.tops()[..3], [process_frame, report, stats]);
    assert!(removal.matters(process_frame) && removal.matters(report));
    assert!(removal.is_lost(crc32) && !removal.matters(crc32));
    // stats, frames_ok, frames_bad and __stack_chk_fail@plt.
    let kept = removal.kept_together();
    assert_eq!(kept.len(), 4);
    let mut keepers = [process_frame, report];
    keepers.sort();
    assert!(kept.iter().all(|kept| kept.keepers == keepers));
    // Removing either of the two alone keeps stats.
    assert!(!sample.remove(&[process_frame, crc32]).is_lost(stats));
    assert!(!sample.remove(&[report, crc32]).is_lost(stats));
}

/// Each node's own bytes as `tree` counts them: its retained size, less
/// those of the nodes directly under it.
fn own_bytes(sample: &Sample, tree: &DominatorTree) -> HashMap<Node, u64> {
    let mut own: HashMap<Node, u64> = (sample.nodes())
        .map(|node| (node, tree.retained(node)))
        .collect();
    for node in sample.nodes() {
        if let Some(parent) = tree.parent(node) {
            own.entry(parent)
                .and_modify(|bytes| *bytes -= tree.retained(node));
        }
    }
    own
}

/// What `removal` loses, its tops and what it saves, against the sets that
/// plain searches find.
fn check_lost(
    sample: &Sample,
    before: &HashSet<Node>,
    removed: &HashSet<Node>,
    removal: &Removal,
    context: &str,
) {
    let lost = sample.lost(before, removed);
    assert_eq!(removal.lost().collect::<Vec<_>>(), lost, "{context}");
    let is_lost: HashSet<Node> = lost.iter().copied().collect();
    for node in sample.nodes() {
        assert_eq!(removal.is_lost(node), is_lost.contains(&node), "{context}");
        assert_eq!(
            removal.is_removed(node),
            removed.contains(&node),
            "{context}"
        );
    }
    let unreachable: Vec<Node> = (sample.nodes())
        .filter(|node| removed.contains(node) && !before.contains(node))
        .collect();
    assert_eq!(
        removal.unreachable().collect::<Vec<_>>(),
        unreachable,
        "{context}"
    );
    // The tops are the lost nodes whose parent isn't lost, sorted as the
    // tree sorts nodes: largest first, and those of the same size in the
    // order of their indexes.
    let tree = &sample.tree;
    let mut tops: Vec<Node> = (lost.iter().copied())
        .filter(|&node| {
            !tree
                .parent(node)
                .is_some_and(|parent| is_lost.contains(&parent))
        })
        .collect();
    tops.sort_by_key(|&top| (Reverse(tree.retained(top)), top));
    assert_eq!(removal.tops(), tops, "{context}");
    // Their retained sizes and the own bytes of what nothing reaches add up
    // to the own bytes of everything that goes.
    let own = |nodes: &[Node]| nodes.iter().map(|&node| sample.own(node)).sum::<u64>();
    let goes = own(&lost) + own(&unreachable);
    let in_tops: u64 = tops.iter().map(|&top| tree.retained(top)).sum();
    assert_eq!(in_tops + own(&unreachable), goes, "{context}");
    assert_eq!(removal.retained(), goes, "{context}");
}

/// Which removed nodes matter, against what each adds: a removed node that
/// something reaches matters when it is a root or a node that the search
/// reaches uses it, which is when removing the others alone loses less.
fn check_matters(
    sample: &Sample,
    before: &HashSet<Node>,
    removed: &HashSet<Node>,
    removal: &Removal,
    context: &str,
) {
    let after = sample.reachable_without(removed);
    let lost = sample.lost(before, removed);
    for &node in removed {
        if !before.contains(&node) {
            assert!(!removal.matters(node), "{context}: {node:?}");
            continue;
        }
        let is_root = sample.graph.roots.iter().any(|root| root.node == node);
        let used = (sample.graph.references.iter())
            .any(|reference| reference.to == node && after.contains(&Node::Item(reference.from)));
        assert_eq!(
            removal.matters(node),
            is_root || used,
            "{context}: {node:?}"
        );
        let mut others = removed.clone();
        others.remove(&node);
        let adds = sample.lost(before, &others) != lost;
        assert_eq!(removal.matters(node), adds, "{context}: {node:?}");
    }
}

/// The keepers of each top that isn't removed: removing just them loses
/// it, there are at least two, and removing all of them but one keeps it.
fn check_keepers(sample: &Sample, removal: &Removal, context: &str) {
    let tops: Vec<Node> = (removal.tops().iter().copied())
        .filter(|&top| !removal.is_removed(top))
        .collect();
    let kept: Vec<Node> = removal
        .kept_together()
        .iter()
        .map(|kept| kept.top)
        .collect();
    assert_eq!(kept, tops, "{context}");
    for kept in removal.kept_together() {
        let context = format!("{context}: {:?} kept by {:?}", kept.top, kept.keepers);
        assert!(kept.keepers.len() >= 2, "{context}");
        assert!(
            kept.keepers.iter().all(|&keeper| removal.matters(keeper)),
            "{context}"
        );
        let keepers: HashSet<Node> = kept.keepers.iter().copied().collect();
        assert!(
            !sample.reachable_without(&keepers).contains(&kept.top),
            "{context}"
        );
        for keeper in &keepers {
            let mut all_but_one = keepers.clone();
            all_but_one.remove(keeper);
            let reachable = sample.reachable_without(&all_but_one);
            assert!(
                reachable.contains(&kept.top),
                "{context}: without {keeper:?}"
            );
        }
    }
}
