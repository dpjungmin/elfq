//! The two answers of a range check: [`Leaving`], what the items in the
//! ranges use outside them, for `callees`, and [`Entering`], what uses them
//! from outside, for `callers`. They mirror each other, so each is kept
//! beside the other: a change to one usually needs the same change to the
//! other.

use std::collections::HashSet;
use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::rows::{
    CalleeJson, callee_json, callee_rows, crossings_table, each_row, group_unfollowed, groups,
    how_column, json_other, json_ranges, names_cell, outside_nodes, ranges_text, unfollowed_how,
    write_groups,
};
use super::{Direction, RangeCheck};
use crate::binary::{AddressRange, Binary};
use crate::commands::{json, text};
use crate::graph::{Crossing, Node, Unfollowed};

#[derive(Serialize)]
struct LeavingJson<'a> {
    ranges: Vec<json::Range>,
    /// The ranges of `--to`, or `null`.
    to: Option<Vec<json::Range>>,
    callees: Vec<CalleeJson<'a>>,
    /// The calls and jumps in the ranges that elfq can't follow, and that
    /// may leave them, or with `--to`, may go to its ranges.
    unresolved: Vec<UnresolvedJson<'a>>,
    /// The functions in the ranges whose code elfq can't read, since it
    /// doesn't know where they end.
    unreadable: Vec<json::Item<'a>>,
    /// Whether `callees` has everything the ranges use outside them, or with
    /// `--to`, in its ranges, as far as elfq can tell: `false` when
    /// `unreadable` isn't empty, or `unresolved` is not, other than with
    /// `--to`, for calls to known addresses in its ranges.
    complete: bool,
}

#[derive(Serialize)]
struct UnresolvedJson<'a> {
    /// The function it is in: for `callees`, in the ranges, and for
    /// `callers`, outside them.
    caller: json::Item<'a>,
    /// The instruction's address.
    site: String,
    /// `call` or `jump`.
    how: &'static str,
    /// Where it goes, when the code says so but no item is there, or
    /// `null` when only the running program knows.
    target: Option<String>,
    /// The linker's veneer that the call goes through, or `null`.
    veneer: Option<json::Item<'a>>,
}

#[derive(Serialize)]
struct EnteringJson<'a> {
    ranges: Vec<json::Range>,
    /// The ranges of `--from`, or `null`.
    from: Option<Vec<json::Range>>,
    callers: Vec<CallerJson<'a>>,
    /// The calls and jumps from outside into code in the ranges that is
    /// outside every item, as in a chip's ROM, which elfq doesn't read.
    unresolved: Vec<UnresolvedJson<'a>>,
    /// The functions in the ranges of `--from`, outside the ranges, whose
    /// code elfq can't read, since it doesn't know where they end.
    unreadable: Vec<json::Item<'a>>,
    /// The functions in the ranges whose address is taken or stored, so
    /// that code outside may call them through a pointer, which elfq can't
    /// see. A pointer in the vector table doesn't count, since only the
    /// processor calls through it. With `--from`, empty unless code in its
    /// ranges calls through a pointer, or elfq can't read it.
    through_pointer: Vec<json::Item<'a>>,
    /// Whether `callers` has everything outside the ranges that uses them,
    /// or with `--from`, in its ranges, as far as elfq can tell: `false`
    /// when `through_pointer` or `unreadable` isn't empty.
    complete: bool,
}

#[derive(Serialize)]
struct CallerJson<'a> {
    /// The item outside the ranges.
    item: json::Item<'a>,
    /// The item in the ranges that it uses.
    callee: json::Item<'a>,
    how: &'static str,
    /// The linker's veneer that the call goes through, or `null`.
    veneer: Option<json::Item<'a>>,
    sites: Vec<String>,
}

/// Where the far side of the crossings is, for a sentence: "outside it",
/// or with `--to` or `--from`, in their ranges, as "in 0x30000000..0x31000000
/// (.text)". `it` is "it" or "them".
fn far_side(binary: &Binary, other: &[AddressRange], it: &str, all: bool) -> String {
    if other.is_empty() {
        format!("outside {it}")
    } else {
        format!("in {}", ranges_text(binary, other, all))
    }
}

/// What the items in the ranges use outside them: `callees RANGE...`, or
/// `callees SYMBOL --to RANGE`.
pub(super) struct Leaving<'a> {
    pub(super) binary: &'a Binary,
    pub(super) check: &'a RangeCheck<'a>,
    pub(super) crossings: Vec<Crossing>,
    /// The calls and jumps in the ranges that elfq can't follow, and that
    /// may leave them, or with `--to`, may go to its ranges.
    pub(super) unfollowed: Vec<Unfollowed>,
    /// The functions in the ranges whose code elfq can't read, since it
    /// doesn't know where they end.
    pub(super) unreadable: Vec<usize>,
    /// Whether the sentences name every range, section and function, for
    /// `--all`.
    pub(super) all: bool,
}

impl Leaving<'_> {
    pub(super) fn write_json(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let item = |node: Node| json::Item::new(binary, node);
        let leaving = LeavingJson {
            ranges: json_ranges(self.check.ranges),
            to: json_other(self.check.other),
            callees: callee_rows(binary, &self.crossings)
                .iter()
                .map(|(outside, row)| callee_json(binary, *outside, row))
                .collect(),
            unresolved: unresolved_json(binary, &self.unfollowed),
            unreadable: self
                .unreadable
                .iter()
                .map(|&index| item(Node::Item(index)))
                .collect(),
            // With `--to`, a call to a known address in its ranges, as to a
            // chip's ROM, is a whole answer, as `callers --from` counts it:
            // only a call through a pointer leaves it open.
            complete: self.unreadable.is_empty()
                && self
                    .unfollowed
                    .iter()
                    .all(|row| !self.check.other.is_empty() && row.unresolved.target.is_some()),
        };
        json::write(&leaving, out)
    }

    pub(super) fn write_text(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let check = self.check;
        let subject = check.subject(binary, self.all);
        let verb = |one: &str, many: &str| if check.one() { one } else { many }.to_string();
        let groups = groups(binary, &self.crossings, Direction::Out, |crossing| {
            how_column(binary, crossing)
        });
        let used = text::count_text(binary, &outside_nodes(&groups));
        let unknown = text::calls_and_jumps(
            self.unfollowed
                .iter()
                .map(|unfollowed| &unfollowed.unresolved),
        );
        let far_side = far_side(binary, check.other, check.it(), self.all);
        let lead = match (groups.is_empty(), self.unfollowed.is_empty()) {
            (true, true) if self.unreadable.is_empty() => {
                format!("{subject} {} nothing {far_side}.", verb("uses", "use"))
            }
            (true, true) => format!(
                "In the code elfq can read, {subject} {} nothing {far_side}.",
                verb("uses", "use")
            ),
            (false, true) => format!("{subject} {} {used} {far_side}:", verb("uses", "use")),
            (true, false) if check.other.is_empty() => format!(
                "{subject} {} {unknown} that elfq can't follow:",
                verb("makes", "make")
            ),
            (true, false) => {
                // A call through a pointer may go anywhere, but one to an
                // address that the code names goes there.
                let go = if self
                    .unfollowed
                    .iter()
                    .any(|u| u.unresolved.target.is_none())
                {
                    "may go"
                } else if self.unfollowed.len() == 1 {
                    "goes"
                } else {
                    "go"
                };
                format!(
                    "{subject} {} {unknown} that elfq can't follow, which {go} to {}:",
                    verb("makes", "make"),
                    ranges_text(binary, check.other, self.all)
                )
            }
            (false, false) => format!(
                "{subject} {} {used} {far_side}; elfq can't follow {unknown}:",
                verb("uses", "use")
            ),
        };
        writeln!(out, "{lead}")?;

        let listed = !groups.is_empty() || !self.unfollowed.is_empty();
        if listed {
            writeln!(out)?;
            let mut table = crossings_table(Direction::Out);
            // What elfq can't follow comes first, since it may hide more.
            for (unfollowed, sites) in group_unfollowed(&self.unfollowed) {
                let unresolved = &unfollowed.unresolved;
                let (callee, address) = match unresolved.target {
                    None => ("(through a pointer)", String::new()),
                    Some(target) => ("(outside any function)", format!("{target:#x}")),
                };
                table.row(vec![
                    address,
                    String::new(),
                    unfollowed_how(&unfollowed).to_string(),
                    text::at_column(&sites),
                    names_cell(callee, Direction::Out, &binary.items[unresolved.from].name),
                ]);
            }
            write_groups(binary, &groups, Direction::Out, &mut table);
            table.write(out)?;
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
}

/// What uses the items in the ranges from outside them: `callers
/// RANGE...`, or `callers SYMBOL --from RANGE`.
pub(super) struct Entering<'a> {
    pub(super) binary: &'a Binary,
    pub(super) check: &'a RangeCheck<'a>,
    pub(super) crossings: Vec<Crossing>,
    /// The calls and jumps from outside into code in the ranges that is
    /// outside every item, as in a chip's ROM.
    pub(super) unfollowed: Vec<Unfollowed>,
    /// The functions in the ranges that code outside may call through a
    /// pointer, which elfq can't see. With `--from`, only when code in its
    /// ranges calls through a pointer, or elfq can't read it.
    pub(super) through_pointer: Vec<usize>,
    /// The functions in the ranges of `--from`, outside the ranges, whose
    /// code elfq can't read, since it doesn't know where they end.
    pub(super) unreadable: Vec<usize>,
    /// Whether the sentences name every range, section and function, for
    /// `--all`.
    pub(super) all: bool,
}

impl Entering<'_> {
    pub(super) fn write_json(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let item = |node: Node| json::Item::new(binary, node);
        let groups = groups(binary, &self.crossings, Direction::In, |crossing| {
            json::how_field(crossing.kind)
        });
        let entering = EnteringJson {
            ranges: json_ranges(self.check.ranges),
            from: json_other(self.check.other),
            callers: each_row(groups)
                .map(|(outside, row)| CallerJson {
                    item: item(outside),
                    callee: item(row.inside),
                    how: json::how_field(row.kind),
                    veneer: row.veneer.map(|veneer| item(Node::Item(veneer))),
                    sites: json::addresses(&row.sites),
                })
                .collect(),
            unresolved: unresolved_json(binary, &self.unfollowed),
            unreadable: self
                .unreadable
                .iter()
                .map(|&index| item(Node::Item(index)))
                .collect(),
            through_pointer: self
                .through_pointer
                .iter()
                .map(|&index| item(Node::Item(index)))
                .collect(),
            complete: self.through_pointer.is_empty() && self.unreadable.is_empty(),
        };
        json::write(&entering, out)
    }

    pub(super) fn write_text(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let check = self.check;
        let subject = check.subject(binary, self.all);
        let it = check.it();
        let groups = groups(binary, &self.crossings, Direction::In, |crossing| {
            how_column(binary, crossing)
        });
        let far_side = far_side(binary, check.other, it, self.all);
        // "Nothing outside X uses it", or "Nothing in Y uses X".
        let nothing = if check.other.is_empty() {
            format!("Nothing outside {subject} uses {it}")
        } else if self.unreadable.is_empty() {
            format!("Nothing {far_side} uses {subject}")
        } else {
            format!("In the code elfq can read, nothing {far_side} uses {subject}")
        };
        let listed = !groups.is_empty() || !self.unfollowed.is_empty();
        if !listed && self.through_pointer.is_empty() {
            writeln!(out, "{nothing}.")?;
        } else if !listed {
            writeln!(out, "{nothing} directly.")?;
        } else {
            let mut callers = outside_nodes(&groups);
            let mut seen: HashSet<Node> = callers.iter().copied().collect();
            for row in &self.unfollowed {
                let caller = Node::Item(row.unresolved.from);
                if seen.insert(caller) {
                    callers.push(caller);
                }
            }
            writeln!(
                out,
                "{subject} {} used by {} {far_side}:",
                if check.one() { "is" } else { "are" },
                text::count_text(binary, &callers),
            )?;
            writeln!(out)?;
            let mut table = crossings_table(Direction::In);
            // The calls into code that elfq doesn't read come first, as
            // `callees` lists them.
            for (unfollowed, sites) in group_unfollowed(&self.unfollowed) {
                let unresolved = &unfollowed.unresolved;
                let caller = &binary.items[unresolved.from];
                let target = unresolved
                    .target
                    .map_or(String::new(), |t| format!("{t:#x} "));
                table.row(vec![
                    format!("{:#x}", caller.address),
                    text::size_text(caller),
                    unfollowed_how(&unfollowed).to_string(),
                    text::at_column(&sites),
                    names_cell(
                        &caller.name,
                        Direction::In,
                        &format!("{target}(outside any function)"),
                    ),
                ]);
            }
            write_groups(binary, &groups, Direction::In, &mut table);
            table.write(out)?;
        }
        if !self.through_pointer.is_empty() {
            let names: Vec<&str> = self
                .through_pointer
                .iter()
                .map(|&index| binary.items[index].name.as_str())
                .collect();
            if listed {
                writeln!(out)?;
            }
            writeln!(
                out,
                "Calls through a pointer to {} aren't listed, since elfq can't see them.",
                text::names_text(&names, self.all)
            )?;
        }
        if !self.unreadable.is_empty() {
            if listed && self.through_pointer.is_empty() {
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
}

/// The calls and jumps that elfq can't follow, for JSON.
fn unresolved_json<'a>(binary: &'a Binary, unfollowed: &[Unfollowed]) -> Vec<UnresolvedJson<'a>> {
    let item = |index: usize| json::Item::new(binary, Node::Item(index));
    unfollowed
        .iter()
        .map(|unfollowed| {
            let unresolved = &unfollowed.unresolved;
            UnresolvedJson {
                caller: item(unresolved.from),
                site: json::address(unresolved.site),
                how: text::call_or_jump(unresolved),
                target: unresolved.target.map(json::address),
                veneer: unfollowed.veneer.map(item),
            }
        })
        .collect()
}
