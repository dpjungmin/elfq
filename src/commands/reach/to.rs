//! The text answer of `reach SYMBOL --to RANGE`: what SYMBOL reaches in
//! the ranges, and the use from outside them that reaches each item.

use super::tree::{Tree, TreeRows};
use super::{Step, Text, columns, reached_words};
use crate::commands::flat_answer::{FlatAnswer, FlatRow};
use crate::commands::{range_check, text};
use crate::graph::{Node, ReferenceKind, Unfollowed, Unresolved};

impl Text<'_, '_> {
    /// The answer as text with `--to`: what SYMBOL reaches in its ranges,
    /// with every row, or with `--tree`, the chains from SYMBOL to them;
    /// and the note on the code that elfq can't read, which may reach more.
    pub(super) fn to_answer(&self) -> FlatAnswer {
        let reach = self.reach;
        let counted = reach.counted();
        let cant_follow = self.counted_cant_follow();
        let mut lead = self.to_lead(&counted, &cant_follow);
        let mut answer = FlatAnswer::default();
        if !counted.is_empty() || !cant_follow.is_empty() {
            lead += ":";
            answer.columns = columns(!self.tree);
            if self.tree {
                let rows = TreeRows::Exactly(reach.in_answer());
                answer.tree = Some(Tree::new(self, rows).top());
            } else {
                answer.rows = self.to_rows(&counted);
            }
        }
        answer.lead = vec![lead];
        if !reach.unreadable.is_empty() {
            let unreadable = text::unreadable_text(reach.binary, &reach.unreadable, self.all);
            answer.notes.push(unreadable);
        }
        answer
    }

    /// The first line with `--to`, but for the colon before the rows: what
    /// SYMBOL and what it reaches in the ranges take, of `counted`, and
    /// what elfq can't follow of `cant_follow`, which may go there.
    fn to_lead(&self, counted: &[&Step], cant_follow: &[&Unresolved]) -> String {
        let reach = self.reach;
        let binary = reach.binary;
        let name = self.lead_name;
        let symbol_in_to = reach.in_to(&reach.steps[0]);
        let others: Vec<Node> = counted
            .iter()
            .filter(|step| step.parent.is_some())
            .map(|step| step.node)
            .collect();
        let bytes = text::code_and_data(reach.bytes(counted.iter().copied()));
        let ranges = range_check::ranges_text(binary, &reach.to, self.all);
        let mut lead = match (symbol_in_to, &others[..]) {
            (true, []) => {
                format!("{name} takes {bytes} in {ranges}, and reaches nothing else there")
            }
            (true, _) => format!(
                "{name} and the {} it reaches in {ranges} take {bytes}",
                reached_words(binary, &others)
            ),
            (false, []) if !cant_follow.is_empty() => {
                format!("{name} reaches no items in {ranges}")
            }
            (false, []) if reach.unreadable.is_empty() => {
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
        if !cant_follow.is_empty() {
            let joint = if counted.is_empty() { ", but" } else { ";" };
            lead += &format!(
                "{joint} elfq can't follow {}, which {} there",
                text::calls_and_jumps(cant_follow.iter().copied()),
                text::where_they_go(cant_follow)
            );
        }
        lead
    }

    /// The rows with `--to`: SYMBOL's, when it is in the ranges; then the
    /// calls and jumps that elfq can't follow that may go there, first, as
    /// `callees --to` lists them, since they may hide more; then the rest
    /// of `counted`, in the order of [`Text::rows`]. Every row is shown,
    /// with the sites of the use it names in the AT column.
    fn to_rows(&self, counted: &[&Step]) -> Vec<FlatRow> {
        let reach = self.reach;
        let with_at = |mut row: FlatRow, sites: &[u64]| {
            row.cells.push(text::at_column(sites));
            row
        };
        let mut rows = Vec::new();
        if reach.in_to(&reach.steps[0]) {
            rows.push(with_at(self.symbol_row(), &[]));
        }
        for (unfollowed, sites) in range_check::group_unfollowed(&reach.listed()) {
            let sites = self.unfollowed_sites(&unfollowed, sites);
            rows.push(with_at(self.unfollowed_row(&unfollowed), &sites));
        }
        let reached = (counted.iter().copied())
            .filter(|step| step.parent.is_some())
            .collect();
        for step in self.in_row_order(reached) {
            rows.push(with_at(self.row(step), &self.use_sites(step)));
        }
        rows
    }
    /// Where the item that uses `step` on its row uses it as the row's HOW
    /// says, for the AT column: every such site, as `callees` lists them,
    /// which may be more than the chain's, as when code both reads a
    /// variable and loads its address, which is "uses" either way.
    fn use_sites(&self, step: &Step) -> Vec<u64> {
        let reach = self.reach;
        let binary = reach.binary;
        let used = step.used.as_ref().expect("only SYMBOL has no user");
        // A call through a veneer is a call to the veneer.
        let to = used.veneer.map_or(step.node, Node::Item);
        let how = |kind| text::how_via(binary, step.node, kind, used.veneer);
        reach
            .graph
            .references_from(used.user)
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
        let reach = self.reach;
        let Some(veneer) = unfollowed.veneer else {
            return sites;
        };
        let unresolved = &unfollowed.unresolved;
        reach
            .graph
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
