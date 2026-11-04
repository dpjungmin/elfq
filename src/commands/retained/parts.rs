//! The parts of `retained`'s answer that depend only on what removing the
//! items loses, whichever items they are: the lost items under the
//! removal's tops, which the table shows, what the lost items share with
//! other code, and the calls through pointers in the items that go, with
//! the notes that name those two; and for several items, the items that
//! each row kept together needs, and the bytes that go only when several
//! go. The answers in `mod.rs` and `several.rs` add their lead lines and
//! pick their rows.

use std::collections::HashMap;

use crate::binary::total;
use crate::commands::retained_tree::RetainedTree;
use crate::commands::text::{self, Naming};
use crate::graph::{Node, Removal, Unresolved};

/// What every form of the answer about removing some items shares, worked
/// out once from what removing them loses: the bytes saved and how many of
/// them go only together, the items that each row kept together needs, the
/// notes, and the trees under the removal's tops, which the table shows.
pub(super) struct Parts<'t, 'g, 'a> {
    /// The tree of retained sizes that the answer is about.
    retained: &'t RetainedTree<'g, 'a>,
    /// The removed items, as indexes into [`Binary::items`], in the order
    /// given.
    ///
    /// [`Binary::items`]: crate::binary::Binary::items
    removed: Vec<usize>,
    /// What removing the items loses: the branches of the tree under the
    /// removal's tops. For one SYMBOL that something reaches, the only top
    /// is SYMBOL itself, so what is lost is SYMBOL and everything it keeps.
    /// For one that nothing reaches, nothing is lost.
    pub(super) removal: Removal,
    /// The lost items under the tops that sizes count: all of them, or with
    /// a range, those with bytes of their own in it. For one SYMBOL, the
    /// items that it keeps. In no particular order.
    pub(super) under_tops: Vec<usize>,
    /// What the lost items use that other code uses too (see [`shared`]).
    pub(super) shared: Vec<Node>,
    /// The calls and jumps through pointers set at run time in the items
    /// that go: the lost ones, and the removed ones that nothing reaches.
    pub(super) indirect: Vec<Unresolved>,
}

impl<'t, 'g, 'a> Parts<'t, 'g, 'a> {
    /// The parts of the answer about removing the items at `removed`, as
    /// indexes into [`Binary::items`], from the tree `retained`, from what
    /// removing them loses, which every part starts from.
    ///
    /// [`Binary::items`]: crate::binary::Binary::items
    pub(super) fn new(retained: &'t RetainedTree<'g, 'a>, removed: &[usize]) -> Parts<'t, 'g, 'a> {
        let (graph, tree) = (retained.graph, &retained.tree);
        let nodes: Vec<Node> = removed.iter().map(|&index| Node::Item(index)).collect();
        let removal = Removal::find(retained.binary, graph, tree, &nodes);
        // A removed item that nothing reaches isn't lost, but it goes too.
        let indirect = (removal.lost().chain(removal.unreachable()))
            .filter_map(item_index)
            .flat_map(|item| graph.unresolved_in(item))
            .filter(|unresolved| unresolved.is_indirect())
            .copied()
            .collect();
        let under_tops = (removal.tops().iter())
            .flat_map(|&top| tree.exclusive(top))
            .filter_map(item_index)
            .filter(|&index| retained.has_bytes_in_range(index))
            .collect();
        Parts {
            retained,
            removed: removed.to_vec(),
            shared: shared(retained, &removal),
            indirect,
            under_tops,
            removal,
        }
    }

    /// The own bytes of the removed items that nothing reaches, which the
    /// bytes saved count, since removing them saves those too.
    pub(super) fn unreachable_own(&self) -> u64 {
        let retained = self.retained;
        total(
            (self.removal.unreachable())
                .filter_map(item_index)
                .map(|index| retained.own(index)),
        )
    }

    /// How many of the bytes saved no one removed item saves alone. That is
    /// all of them, less the own bytes of the removed items that nothing
    /// reaches, less the retained size of each removed item that no other
    /// removed item keeps, which is what removing it alone saves. Those
    /// retained sizes don't overlap, and a removed item that another keeps
    /// saves nothing alone that the other doesn't save already. For one
    /// item, none.
    pub(super) fn bytes_kept_together(&self) -> u64 {
        let tree = &self.retained.tree;
        let removal = &self.removal;
        // Whether another removed item is above `node` in the tree, and so
        // keeps it. Every item between the two would be lost too, so the
        // walk up stops at the first item that isn't.
        let kept_by_another = |node: Node| {
            let mut above = tree.parent(node);
            while let Some(node) = above.filter(|&node| removal.is_lost(node)) {
                if removal.is_removed(node) {
                    return true;
                }
                above = tree.parent(node);
            }
            false
        };
        let alone = total(
            (self.removed.iter())
                .map(|&index| Node::Item(index))
                .filter(|&node| tree.is_reachable(node) && !kept_by_another(node))
                .map(|node| tree.retained(node)),
        );
        (removal.retained())
            .saturating_sub(self.unreachable_own())
            .saturating_sub(alone)
    }

    /// The keepers of each top that isn't a removed item, which several
    /// removed items keep together and no one of them alone, by the top's
    /// index: the removed items that it needs (see [`KeptTogether`]), in
    /// the order given, as the answer names them. Removing just those loses
    /// the top, and removing all of them but one keeps it. A removed item
    /// at the top has none, even when the others keep it only together:
    /// the answer says instead that it adds nothing. An import kept
    /// together has no bytes in the file, and so no row to name them on.
    ///
    /// [`KeptTogether`]: crate::graph::KeptTogether
    pub(super) fn keepers(&self) -> HashMap<usize, Vec<usize>> {
        let given: HashMap<usize, usize> = (self.removed.iter().enumerate())
            .map(|(given, &index)| (index, given))
            .collect();
        (self.removal.kept_together().iter())
            .filter_map(|kept| {
                let top = item_index(kept.top)?;
                let mut keepers: Vec<usize> = (kept.keepers.iter().copied())
                    .filter_map(item_index)
                    .collect();
                keepers.sort_by_key(|keeper| given[keeper]);
                Some((top, keepers))
            })
            .collect()
    }

    /// The notes under the tree, naming the items they list as `naming`
    /// says: what the lost items share with other code, and the calls that
    /// elfq can't follow.
    pub(super) fn notes(&self, naming: &Naming) -> Vec<String> {
        [self.shared_note(naming), self.indirect_note(naming)]
            .into_iter()
            .flatten()
            .collect()
    }

    /// Names what the lost items use that other code uses too, and so
    /// isn't counted, in order of name, as `naming` names them. `None` when
    /// there is nothing.
    fn shared_note(&self, naming: &Naming) -> Option<String> {
        let binary = self.retained.binary;
        let mut shared: Vec<&str> = (self.shared.iter()).map(|node| node.name(binary)).collect();
        shared.dedup();
        if shared.is_empty() {
            return None;
        }
        Some(format!(
            "Shared with other code, so not counted: {}.",
            naming.list(&shared)
        ))
    }

    /// Says that what the code of the items that go calls through pointers
    /// may count for other items, if any of it calls anything that way,
    /// naming the functions that do, as `naming` names them: the removed
    /// ones first, then the others.
    fn indirect_note(&self, naming: &Naming) -> Option<String> {
        let indirect = &self.indirect;
        if indirect.is_empty() {
            return None;
        }
        let removal = &self.removal;
        let mut callers: Vec<usize> = indirect.iter().map(|u| u.from).collect();
        callers.sort_by_key(|&index| (!removal.is_removed(Node::Item(index)), index));
        callers.dedup();
        let binary = self.retained.binary;
        let names: Vec<&str> = callers
            .iter()
            .map(|&index| Node::Item(index).name(binary))
            .collect();
        let (one, several) = text::call_and_jump_verbs(indirect);
        let verb = if names.len() == 1 { one } else { several };
        let (through, reach) = if indirect.len() == 1 {
            ("a pointer", "that reaches")
        } else {
            ("pointers", "they reach")
        };
        Some(format!(
            "{} also {verb} through {through}, so what {reach} may count elsewhere.",
            naming.phrase(&names)
        ))
    }
}

/// The item that `node` is, as an index into [`Binary::items`], or `None`
/// for an import, whose code isn't in the file.
///
/// [`Binary::items`]: crate::binary::Binary::items
fn item_index(node: Node) -> Option<usize> {
    match node {
        Node::Item(index) => Some(index),
        Node::Import(_) => None,
    }
}

/// What the nodes that `removal` loses use that other code uses too, and
/// so isn't counted, sorted by name. With a range, only items with bytes in
/// it. Nothing is lost when nothing reaches the removed items, and so
/// nothing is shared either.
fn shared(retained: &RetainedTree, removal: &Removal) -> Vec<Node> {
    let binary = retained.binary;
    let mut shared: Vec<Node> = (retained.graph.references.iter())
        .filter(|r| removal.is_lost(Node::Item(r.from)) && !removal.is_lost(r.to))
        .map(|r| r.to)
        .filter(|&node| match node {
            Node::Item(index) => retained.has_bytes_in_range(index),
            // An import has no bytes in the file, so none in a range.
            Node::Import(_) => retained.range.is_none(),
        })
        .collect();
    shared.sort_by_key(|&node| (node.name(binary), node));
    shared.dedup();
    shared
}
