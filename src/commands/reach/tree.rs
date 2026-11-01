//! The tree that `reach SYMBOL --tree` draws, with `--to` and without.

use std::collections::{HashMap, HashSet};
use std::io::Write;

use anyhow::Result;

use super::rows::more_size;
use super::{COLUMNS, Step, StepId, Text};
use crate::commands::{text, tree_answer};
use crate::graph::{Node, Unresolved};
use crate::render::{Table, TreeRow, TreeShape};

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
    /// with `--to`, those through pointers too.
    unresolved_rows: Vec<Vec<Unresolved>>,
    /// The steps that get rows.
    has_row: HashSet<StepId>,
}

impl<'r, 'a> Tree<'r, 'a> {
    pub(super) fn new(text: &'r Text<'r, 'a>) -> Tree<'r, 'a> {
        let reach = text.reach;
        let steps = &reach.steps;
        let mut children = vec![Vec::new(); steps.len()];
        for (id, step) in steps.iter().enumerate().skip(1) {
            children[step.parent.expect("only SYMBOL has no parent")].push(id);
        }
        let step_of_item: HashMap<usize, StepId> = steps
            .iter()
            .enumerate()
            .filter_map(|(id, step)| match step.node {
                Node::Item(index) => Some((index, id)),
                Node::Import(_) => None,
            })
            .collect();
        let mut unresolved_rows: Vec<Vec<Unresolved>> = vec![Vec::new(); steps.len()];
        for &unresolved in &reach.unresolved {
            // Without `--to`, a note below the tree names the calls and
            // jumps through pointers.
            if unresolved.target.is_none() && reach.to.is_empty() {
                continue;
            }
            let rows = &mut unresolved_rows[step_of_item[&unresolved.from]];
            let same = |row: &Unresolved| {
                (row.is_call, row.target) == (unresolved.is_call, unresolved.target)
            };
            if !rows.iter().any(same) {
                rows.push(unresolved);
            }
        }
        let mut tree = Tree {
            text,
            children,
            unresolved_rows,
            has_row: HashSet::new(),
        };
        tree.has_row = tree.has_row();
        tree
    }

    /// Which steps get rows: with `--to`, the steps in the answer (see
    /// [`Reach::in_answer`]). Without it, SYMBOL; the largest items, or
    /// every one (see [`Text::largest`]); the steps with rows for calls or
    /// jumps to code outside every item, since rows by block show every
    /// one; every step on their chains from SYMBOL, so that the tree holds
    /// together; and a step left out on its own (see
    /// [`tree_answer::add_lone`]).
    ///
    /// [`Reach::in_answer`]: super::Reach::in_answer
    fn has_row(&self) -> HashSet<StepId> {
        let reach = self.text.reach;
        if !reach.to.is_empty() {
            let in_answer = reach.in_answer();
            return (0..in_answer.len()).filter(|&id| in_answer[id]).collect();
        }
        let steps = &reach.steps;
        let largest = self.text.largest(&steps[1..]);
        let wanted = (0..steps.len()).filter(|&id| {
            id == 0 || largest.contains(&steps[id].node) || !self.unresolved_rows[id].is_empty()
        });
        let mut has_row = tree_answer::with_chains(wanted, |id| steps[id].parent);
        tree_answer::add_lone(&mut has_row, [0], |id| self.children[id].iter().copied());
        has_row
    }

    /// Writes the tree under the columns of the rows by block, as `--to`
    /// writes it.
    pub(super) fn write(&self, out: &mut dyn Write) -> Result<()> {
        let mut table = Table::new(&COLUMNS);
        table.tree(&[self.top()], TreeShape::Drawn);
        table.write(out)?;
        Ok(())
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
        let Step {
            node,
            via,
            through_pointer,
            ..
        } = self.text.reach.steps[id];
        let (address, size) = match node {
            Node::Item(index) => {
                let item = &binary.items[index];
                (format!("{:#x}", item.address), text::size_text(item))
            }
            Node::Import(_) => (String::new(), String::new()),
        };
        let how = via.map_or("", |via| text::how_column(binary, node, via.kind));
        let (with_rows, hidden): (Vec<StepId>, Vec<StepId>) = self.children[id]
            .iter()
            .partition(|child| self.has_row.contains(child));
        let mut children: Vec<TreeRow> = (with_rows.into_iter())
            .map(|child| self.row(child))
            .collect();
        for unresolved in &self.unresolved_rows[id] {
            let (address, name) = match unresolved.target {
                Some(target) => (format!("{target:#x}"), "(outside any function)"),
                None => (String::new(), "(through a pointer)"),
            };
            let cells = vec![
                address,
                String::new(),
                text::call_or_jump(unresolved).to_string(),
            ];
            children.push(TreeRow::new(cells, name, Vec::new()));
        }
        // With `--to`, the steps left out aren't part of the answer.
        if !hidden.is_empty() && self.text.reach.to.is_empty() {
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
