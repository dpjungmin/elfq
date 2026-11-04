//! The parts of `retained`'s answer that depend only on what removing the
//! items loses, whichever items they are: the lost items under the
//! removal's tops, which the table shows, what the lost items share with
//! other code, and the calls through pointers in the items that go, with
//! the notes that name those two. The answer about SYMBOL, in `mod.rs`,
//! adds its lead line and picks its rows.

use crate::commands::retained_tree::RetainedTree;
use crate::commands::text::{self, Naming};
use crate::graph::{Node, Removal, Unresolved};

/// What every form of the answer about removing some items shares, worked
/// out once from what removing them loses: the bytes saved, the notes, and
/// the trees under the removal's tops, which the table shows.
pub(super) struct Parts<'t, 'g, 'a> {
    /// The tree of retained sizes that the answer is about.
    retained: &'t RetainedTree<'g, 'a>,
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
    /// The parts of the answer about removing the items in `removed` from
    /// the tree `retained`, from what removing them loses, which every part
    /// starts from.
    pub(super) fn new(retained: &'t RetainedTree<'g, 'a>, removed: &[Node]) -> Parts<'t, 'g, 'a> {
        let (graph, tree) = (retained.graph, &retained.tree);
        let removal = Removal::find(retained.binary, graph, tree, removed);
        // Imports are left out: their code isn't in the file.
        let item_index = |node: Node| match node {
            Node::Item(index) => Some(index),
            Node::Import(_) => None,
        };
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
            shared: shared(retained, &removal),
            indirect,
            under_tops,
            removal,
        }
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
