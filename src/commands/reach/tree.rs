//! The tree that `reach SYMBOL --tree` draws, with `--to` and without.

use std::collections::HashMap;
use std::io::Write;

use anyhow::Result;

use super::{Step, Text};
use crate::binary::total;
use crate::commands::text;
use crate::graph::{Node, Unresolved};
use crate::render::{Align, Table, TreeRow, TreeShape};

/// What SYMBOL reaches, as `--tree` draws it: each item under the item that
/// uses it on its chain from SYMBOL, in the order that item first uses
/// them, with a veneer as a step of its own; and each call or jump to code
/// outside every item under the item it is in. With `--to`, only the chains
/// to the items in its ranges, and to the calls and jumps that may go
/// there, through pointers too.
pub(super) struct Tree<'r, 'a> {
    text: &'r Text<'r, 'a>,
    /// The steps under each step, in the order that it first uses them.
    children: Vec<Vec<usize>>,
    /// The calls and jumps that elfq can't follow in each step that get
    /// rows, one per kind and target: those to code outside every item, and
    /// with `--to`, those through pointers too.
    outside: Vec<Vec<Unresolved>>,
    /// Whether each step gets a row.
    shown: Vec<bool>,
}

impl<'r, 'a> Tree<'r, 'a> {
    pub(super) fn new(text: &'r Text<'r, 'a>) -> Tree<'r, 'a> {
        let reach = text.reach;
        let steps = &reach.steps;
        let mut children = vec![Vec::new(); steps.len()];
        for (step, row) in steps.iter().enumerate().skip(1) {
            children[row.parent.expect("only SYMBOL has no parent")].push(step);
        }
        let step_of: HashMap<usize, usize> = steps
            .iter()
            .enumerate()
            .filter_map(|(step, row)| match row.node {
                Node::Item(index) => Some((index, step)),
                Node::Import(_) => None,
            })
            .collect();
        let mut outside: Vec<Vec<Unresolved>> = vec![Vec::new(); steps.len()];
        for &unresolved in &reach.unresolved {
            // Without `--to`, a note below the tree names the calls and
            // jumps through pointers.
            if unresolved.target.is_none() && reach.to.is_empty() {
                continue;
            }
            let rows = &mut outside[step_of[&unresolved.from]];
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
            outside,
            shown: Vec::new(),
        };
        tree.shown = tree.shown();
        tree
    }

    /// Which steps get rows: with `--to`, the steps the answer has (see
    /// [`Reach::kept`]). Without it, SYMBOL; the largest items, or every
    /// one (see [`Text::shown`]); the steps with rows for calls or jumps
    /// to code outside every item, since rows by block show every one; and
    /// every step on their chains from SYMBOL, so that the tree holds
    /// together. A step that would be the only one left out under a row,
    /// with nothing under it, is shown too, since its row takes no more
    /// room than "1 more" would.
    ///
    /// [`Reach::kept`]: super::Reach::kept
    fn shown(&self) -> Vec<bool> {
        let reach = self.text.reach;
        if !reach.to.is_empty() {
            return reach.kept();
        }
        let steps = &reach.steps;
        let largest = self.text.shown(&steps[1..]);
        let wanted = |step: usize| {
            step == 0 || largest.contains(&steps[step].node) || !self.outside[step].is_empty()
        };
        let mut shown = vec![false; steps.len()];
        for step in 0..steps.len() {
            if wanted(step) {
                let mut on_chain = Some(step);
                while let Some(step) = on_chain
                    && !shown[step]
                {
                    shown[step] = true;
                    on_chain = steps[step].parent;
                }
            }
        }
        let lone: Vec<usize> = (0..steps.len())
            .filter(|&step| shown[step])
            .filter_map(|step| {
                let mut hidden = self.children[step].iter().filter(|&&child| !shown[child]);
                match (hidden.next(), hidden.next()) {
                    (Some(&lone), None) if self.children[lone].is_empty() => Some(lone),
                    _ => None,
                }
            })
            .collect();
        for step in lone {
            shown[step] = true;
        }
        shown
    }

    pub(super) fn write(&self, out: &mut dyn Write) -> Result<()> {
        let mut table = Table::new(&[
            ("ADDRESS", Align::Left),
            ("SIZE", Align::Right),
            ("HOW", Align::Left),
            ("ITEM", Align::Left),
        ]);
        table.tree(&[self.row(0)], TreeShape::Drawn);
        table.write(out)?;
        Ok(())
    }

    /// The row of `step`, with the rows under it: the steps under it that
    /// are shown, its calls and jumps to code outside every item, and a row
    /// that sums up the steps under it that aren't, and what is under them.
    fn row(&self, step: usize) -> TreeRow {
        let binary = self.text.reach.binary;
        let Step {
            node,
            via,
            through_pointer,
            ..
        } = self.text.reach.steps[step];
        let (address, size) = match node {
            Node::Item(index) => {
                let item = &binary.items[index];
                (format!("{:#x}", item.address), text::size_text(item))
            }
            Node::Import(_) => (String::new(), String::new()),
        };
        let how = via.map_or("", |via| text::how_column(binary, node, via.kind));
        let (shown, hidden): (Vec<usize>, Vec<usize>) = self.children[step]
            .iter()
            .partition(|&&child| self.shown[child]);
        let mut children: Vec<TreeRow> = shown.into_iter().map(|child| self.row(child)).collect();
        for unresolved in &self.outside[step] {
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
    fn more(&self, hidden: &[usize]) -> TreeRow {
        let binary = self.text.reach.binary;
        let mut nodes: Vec<Node> = Vec::new();
        let mut next = hidden.to_vec();
        while let Some(step) = next.pop() {
            nodes.push(self.text.reach.steps[step].node);
            next.extend(&self.children[step]);
        }
        // Imports have no bytes in the file, as their own rows show.
        let size = if nodes.iter().all(|node| matches!(node, Node::Import(_))) {
            String::new()
        } else {
            total(nodes.iter().map(|node| node.size(binary))).to_string()
        };
        let cells = vec![String::new(), size, String::new()];
        TreeRow::new(cells, format!("{} more", nodes.len()), Vec::new())
    }
}
