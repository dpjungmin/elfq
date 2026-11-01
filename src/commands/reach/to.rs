//! The text answer of `reach SYMBOL --to RANGE`: what SYMBOL reaches in
//! the ranges, and the use from outside them that reaches each item.

use std::io::Write;

use anyhow::Result;

use super::tree::Tree;
use super::{Reach, Step};
use crate::commands::{range_check, text};
use crate::graph::{Node, ReferenceKind, Unfollowed, Unresolved};
use crate::render::{Align, Table};

impl<'n, 'a> Reach<'n, 'a> {
    /// Writes the answer as text with `--to`: what SYMBOL reaches in its
    /// ranges, every row, or with `tree`, the chains from SYMBOL to them.
    pub(super) fn write_to_text(&self, tree: bool, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let name = &self.lead_name;
        let counted = self.counted();
        let symbol_in_to = counted.first().is_some_and(|step| step.parent.is_none());
        let others: Vec<Node> = counted
            .iter()
            .filter(|step| step.parent.is_some())
            .map(|step| step.node)
            .collect();
        let bytes = text::code_and_data(self.bytes(counted.iter().copied()));
        let ranges = range_check::ranges_text(binary, &self.to, self.all);
        // Counted as the rows show them: a tree has the veneers that the
        // other rows step over.
        let unknown: Vec<&Unresolved> = if tree {
            self.unresolved.iter().collect()
        } else {
            self.unfollowed.iter().map(|u| &u.unresolved).collect()
        };
        let mut lead = match (symbol_in_to, &others[..]) {
            (true, []) => {
                format!("{name} takes {bytes} in {ranges}, and reaches nothing else there")
            }
            (true, [Node::Item(index)]) => format!(
                "{name} and the {} it reaches in {ranges} take {bytes}",
                text::kind_name(binary.items[*index].kind)
            ),
            (true, _) => format!(
                "{name} and the {} it reaches in {ranges} take {bytes}",
                text::count_text(binary, &others)
            ),
            (false, []) if !unknown.is_empty() => format!("{name} reaches no items in {ranges}"),
            (false, []) if self.unreadable.is_empty() => {
                format!("{name} reaches nothing in {ranges}.")
            }
            (false, []) => {
                format!("In the code elfq can read, {name} reaches nothing in {ranges}.")
            }
            (false, _) => format!(
                "{name} reaches {} in {ranges}, which {} {bytes}",
                text::count_text(binary, &others),
                if others.len() == 1 { "takes" } else { "take" }
            ),
        };
        if !unknown.is_empty() {
            // A call through a pointer may go anywhere, but one to an
            // address that the code names goes there.
            let go = if unknown.iter().any(|u| u.target.is_none()) {
                "may go"
            } else if unknown.len() == 1 {
                "goes"
            } else {
                "go"
            };
            let joint = if counted.is_empty() { ", but" } else { ";" };
            lead += &format!(
                "{joint} elfq can't follow {}, which {go} there",
                text::calls_and_jumps(unknown.iter().copied())
            );
        }
        let listed = !counted.is_empty() || !unknown.is_empty();
        if listed {
            lead += ":";
        }
        writeln!(out, "{lead}")?;
        if listed {
            writeln!(out)?;
            if tree {
                Tree::new(self).write(out)?;
            } else {
                self.write_to_rows(&counted, out)?;
            }
        }
        if !self.unreadable.is_empty() {
            if listed {
                writeln!(out)?;
            }
            writeln!(
                out,
                "{}",
                text::unreadable_text(binary, &self.unreadable, self.all)
            )?;
        }
        Ok(())
    }

    /// Writes the rows with `--to`: SYMBOL's, when it is among `counted`,
    /// the steps in its ranges; then the calls and jumps that elfq can't
    /// follow that may go there, first, as `callees --to` lists them, since
    /// they may hide more; then the rest of `counted`, in the order of
    /// [`Reach::rows`]. Every row is shown, with the sites of the use
    /// it names in the AT column.
    fn write_to_rows(&self, counted: &[&Step], out: &mut dyn Write) -> Result<()> {
        let mut table = Table::new(&[
            ("ADDRESS", Align::Left),
            ("SIZE", Align::Right),
            ("HOW", Align::Left),
            ("AT", Align::Left),
            ("ITEM", Align::Left),
        ]);
        let with_at = |mut cells: Vec<String>, sites: &[u64]| {
            cells.insert(3, text::at_column(sites));
            cells
        };
        let (symbol, reached) = match counted {
            [first, rest @ ..] if first.parent.is_none() => (true, rest),
            _ => (false, counted),
        };
        if symbol {
            table.row(with_at(self.symbol_row().into_cells(), &[]));
        }
        for (unfollowed, sites) in range_check::group_unfollowed(&self.unfollowed) {
            let sites = self.unfollowed_sites(&unfollowed, sites);
            table.row(with_at(
                self.unfollowed_row(&unfollowed).into_cells(),
                &sites,
            ));
        }
        for step in self.in_row_order(reached.to_vec()) {
            table.row(with_at(self.row(step).into_cells(), &self.use_sites(step)));
        }
        table.write(out)?;
        Ok(())
    }

    /// Where the item that uses `step` on its row uses it as the row's HOW
    /// says, for the AT column: every such site, as `callees` lists them,
    /// which may be more than the chain's, as when code both reads a
    /// variable and loads its address, which is "uses" either way.
    fn use_sites(&self, step: &Step) -> Vec<u64> {
        let binary = self.binary;
        let used = step.used.as_ref().expect("only SYMBOL has no user");
        // A call through a veneer is a call to the veneer.
        let to = used.veneer.map_or(step.node, Node::Item);
        let how = |kind| text::how_via(binary, step.node, kind, used.veneer);
        self.graph
            .references_from(used.by)
            .iter()
            .filter(|reference| reference.to == to && how(reference.kind) == how(used.kind))
            .map(|reference| reference.site)
            .collect()
    }

    /// Where the code that `unfollowed` names makes it, for the AT column:
    /// `sites`, or for a call through a veneer, every call of its kind from
    /// there to the veneer, as `callees` lists them, since the answer keeps
    /// only the one on the chain to the veneer.
    fn unfollowed_sites(&self, unfollowed: &Unfollowed, sites: Vec<u64>) -> Vec<u64> {
        let Some(veneer) = unfollowed.veneer else {
            return sites;
        };
        let unresolved = &unfollowed.unresolved;
        self.graph
            .references_from(unresolved.from)
            .iter()
            .filter(|reference| {
                reference.to == Node::Item(veneer)
                    && reference.kind.is_call()
                    && (reference.kind == ReferenceKind::Call) == unresolved.is_call
            })
            .map(|reference| reference.site)
            .collect()
    }
}
