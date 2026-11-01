//! The tree that `reach SYMBOL --tree` draws, with `--to` and without.

use std::collections::{HashMap, HashSet};

use super::rows::more_size;
use super::{StepId, Text};
use crate::commands::{text, tree_answer};
use crate::graph::{Node, Unresolved};
use crate::render::TreeRow;

/// What SYMBOL reaches, as `--tree` draws it: each item under the item that
/// uses it on its chain from SYMBOL, in the order that item first uses
/// them, with a veneer as a step of its own; and each call or jump to code
/// outside every item under the item it is in. With `--to`, only the chains
/// to the items in its ranges, and to the calls and jumps that may go
/// there, through pointers too.
pub(super) struct Tree<'r, 'a> {
    text: &'r Text<'r, 'a>,
    /// The steps under each step, in the order that it first uses them.
    children: Vec<Vec<StepId>>,
    /// The calls and jumps that elfq can't follow in each step that get
    /// rows, one per kind and target: those to code outside every item, and
    /// with [`TreeRows::Exactly`], those through pointers too.
    cant_follow_rows: Vec<Vec<Unresolved>>,
    /// The steps that get rows.
    has_row: HashSet<StepId>,
}

/// Which steps the tree has, and which of them get rows.
pub(super) enum TreeRows {
    /// Every step, with rows for the largest items and what holds them
    /// together (see [`Tree::largest_rows`]), and "N more" rows for the
    /// rest. The calls and jumps through pointers are left for a note below
    /// the tree.
    Largest,
    /// Only the steps marked `true`, each with a row, as with `--to`, where
    /// the tree has only the steps in the answer (see [`Reach::in_answer`]),
    /// with rows for the calls and jumps through pointers that may go to
    /// its ranges.
    ///
    /// [`Reach::in_answer`]: super::Reach::in_answer
    Exactly(Vec<bool>),
}

impl<'r, 'a> Tree<'r, 'a> {
    pub(super) fn new(text: &'r Text<'r, 'a>, rows: TreeRows) -> Tree<'r, 'a> {
        let reach = text.reach;
        let steps = &reach.steps;
        let in_tree = |id: StepId| match &rows {
            TreeRows::Largest => true,
            TreeRows::Exactly(marked) => marked[id],
        };
        let mut children = vec![Vec::new(); steps.len()];
        for (id, step) in steps.iter().enumerate() {
            if let Some(parent) = step.parent()
                && in_tree(id)
            {
                children[parent].push(id);
            }
        }
        let step_of_item: HashMap<usize, StepId> = steps
            .iter()
            .enumerate()
            .filter_map(|(id, step)| match step.node {
                Node::Item(index) => Some((index, id)),
                Node::Import(_) => None,
            })
            .collect();
        let mut cant_follow_rows: Vec<Vec<Unresolved>> = vec![Vec::new(); steps.len()];
        for cant in &reach.cant_follow {
            let unresolved = cant.found;
            // With `Largest`, a note below the tree names the calls and
            // jumps through pointers.
            if unresolved.target.is_none() && matches!(rows, TreeRows::Largest) {
                continue;
            }
            let step_rows = &mut cant_follow_rows[step_of_item[&unresolved.from]];
            let same = |row: &Unresolved| {
                (row.is_call, row.target) == (unresolved.is_call, unresolved.target)
            };
            if !step_rows.iter().any(same) {
                step_rows.push(unresolved);
            }
        }
        let mut tree = Tree {
            text,
            children,
            cant_follow_rows,
            has_row: HashSet::new(),
        };
        tree.has_row = match rows {
            TreeRows::Largest => tree.largest_rows(),
            TreeRows::Exactly(marked) => (0..steps.len()).filter(|&id| marked[id]).collect(),
        };
        tree
    }

    /// The steps that get rows with [`TreeRows::Largest`]: SYMBOL; the
    /// largest items, or every one (see [`Text::largest`]); the steps with
    /// rows for calls or jumps to code outside every item, since rows by
    /// block show every one; every step on their chains from SYMBOL, so
    /// that the tree holds together; and a step left out on its own (see
    /// [`tree_answer::add_lone`]).
    fn largest_rows(&self) -> HashSet<StepId> {
        let reach = self.text.reach;
        let steps = &reach.steps;
        let largest = self.text.largest(&steps[1..]);
        let wanted = (0..steps.len()).filter(|&id| {
            id == 0 || largest.contains(&steps[id].node) || !self.cant_follow_rows[id].is_empty()
        });
        let mut has_row = tree_answer::with_chains(wanted, |id| steps[id].parent());
        tree_answer::add_lone(&mut has_row, [0], |id| self.children[id].iter().copied());
        has_row
    }

    /// SYMBOL's row, with every row under it.
    pub(super) fn top(&self) -> TreeRow {
        self.row(0)
    }

    /// The row of the step `id`, with the rows under it: the steps under it
    /// that have rows, its calls and jumps that elfq can't follow, and a
    /// row that sums up the steps under it that don't, and what is under
    /// them.
    fn row(&self, id: StepId) -> TreeRow {
        let binary = self.text.reach.binary;
        let step = &self.text.reach.steps[id];
        let (node, through_pointer) = (step.node, step.through_pointer);
        let [address, size] = text::address_and_size(binary, node);
        let how =
            (step.link.as_ref()).map_or("", |link| text::how_column(binary, node, link.via.kind));
        let (with_rows, hidden): (Vec<StepId>, Vec<StepId>) = self.children[id]
            .iter()
            .partition(|child| self.has_row.contains(child));
        let mut children: Vec<TreeRow> = (with_rows.into_iter())
            .map(|child| self.row(child))
            .collect();
        for unresolved in &self.cant_follow_rows[id] {
            let (address, name) = text::cant_follow_target(unresolved);
            let cells = vec![
                address,
                String::new(),
                text::call_or_jump(unresolved).to_string(),
            ];
            children.push(TreeRow::new(cells, name, Vec::new()));
        }
        if !hidden.is_empty() {
            children.push(self.more(&hidden));
        }
        let mut row = TreeRow::new(
            vec![address, size, how.to_string()],
            node.name(binary),
            children,
        );
        row.mark = through_pointer.then(|| "(through a pointer)".to_string());
        row
    }

    /// The row that sums up the steps in `hidden`, and every step under
    /// them, which no row shows.
    fn more(&self, hidden: &[StepId]) -> TreeRow {
        let reach = self.text.reach;
        let steps = tree_answer::with_descendants(hidden, |id| self.children[id].iter().copied());
        let nodes = steps.iter().map(|&id| reach.steps[id].node);
        let cells = vec![String::new(), more_size(reach.binary, nodes), String::new()];
        TreeRow::new(cells, format!("{} more", steps.len()), Vec::new())
    }
}
