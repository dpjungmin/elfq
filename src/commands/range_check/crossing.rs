//! The two answers of a range check: [`Leaving`], what the items in the
//! ranges use outside them, for `callees`, and [`Entering`], what uses them
//! from outside, for `callers`. The items may be a set's, wherever they lie
//! (see [`Inside`](super::Inside)), and then the notes on how its patterns
//! matched come before the others. Both find the crossings of a `Boundary`
//! that [`run`](super::run) drew around the items inside. They mirror each
//! other, so each is kept beside the other: a change to one usually needs
//! the same change to the other.

use std::collections::HashSet;
use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::rows::{
    CalleeJson, callee_json, callee_rows, crossings_table, each_row, group_unfollowed, groups,
    how_column, json_far_side, json_ranges, names_cell, outside_nodes, unfollowed_how,
    write_groups,
};
use super::{Direction, PickedJson, RangeCheck};
use crate::binary::{AddressRange, Binary};
use crate::cli::Format;
use crate::commands::{json, text};
use crate::graph::{Boundary, Crossing, Graph, ItemsInRanges, Node, Unfollowed, Unresolved};

#[derive(Serialize)]
struct LeavingJson<'a> {
    /// The ranges given in place of SYMBOL, or the one SYMBOL occupies.
    ranges: Vec<json::Range>,
    /// For a set, its SYMBOLs and its patterns' matches.
    #[serde(flatten)]
    picked: PickedJson<'a>,
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
    #[serde(flatten)]
    call: json::Unresolved,
    /// The linker's veneer that the call goes through, or `null`.
    veneer: Option<json::Item<'a>>,
}

#[derive(Serialize)]
struct EnteringJson<'a> {
    /// The ranges given in place of SYMBOL, or the one SYMBOL occupies.
    ranges: Vec<json::Range>,
    /// For a set, its SYMBOLs and its patterns' matches.
    #[serde(flatten)]
    picked: PickedJson<'a>,
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
fn far_side_text(binary: &Binary, far_side: &[AddressRange], it: &str, all: bool) -> String {
    if far_side.is_empty() {
        format!("outside {it}")
    } else {
        format!("in {}", text::ranges_text(binary, far_side, all))
    }
}

/// What the items inside use outside them: `callees RANGE...`, `callees
/// SYMBOL --to RANGE`, or a set, as `callees 'on_*'`.
pub(super) struct Leaving<'a> {
    binary: &'a Binary,
    check: &'a RangeCheck<'a>,
    crossings: Vec<Crossing>,
    /// The calls and jumps in the ranges that elfq can't follow, and that
    /// may leave them, or with `--to`, may go to its ranges.
    unfollowed: Vec<Unfollowed>,
    /// The functions in the ranges whose code elfq can't read, since it
    /// doesn't know where they end.
    unreadable: Vec<usize>,
    /// Whether the sentences name every range, section and function, for
    /// `--all`.
    all: bool,
}

impl<'a> Leaving<'a> {
    /// What the items inside `boundary`, which `check` asks about, use
    /// outside it, in `binary`; `all` is for `--all`.
    pub(super) fn find(
        binary: &'a Binary,
        check: &'a RangeCheck<'a>,
        boundary: &Boundary,
        all: bool,
    ) -> Leaving<'a> {
        let crossings = boundary.leaving_to(check.far_side);
        Leaving {
            binary,
            check,
            crossings: crossings.uses,
            unfollowed: crossings.unfollowed,
            unreadable: boundary.items().unreadable(),
            all,
        }
    }

    /// Whether the answer lists something, or says that something may
    /// cross.
    pub(super) fn crosses(&self) -> bool {
        !self.crossings.is_empty() || !self.unfollowed.is_empty() || !self.unreadable.is_empty()
    }

    /// Whether the answer has everything that crosses, as far as elfq can
    /// tell (see [`complete`]).
    fn complete(&self) -> bool {
        let cant_follow = self.unfollowed.iter().map(|row| &row.unresolved);
        complete(
            cant_follow,
            &self.unreadable,
            !self.check.far_side.is_empty(),
        )
    }

    /// Writes the answer in `format`.
    pub(super) fn write(&self, format: Format, out: &mut dyn Write) -> Result<()> {
        match format {
            Format::Json => self.write_json(out),
            // `--tree` is refused: there is no tree.
            Format::Text | Format::Tree => self.write_text(out),
        }
    }

    fn write_json(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let leaving = LeavingJson {
            ranges: json_ranges(self.check.inside.ranges()),
            picked: self.check.inside.picked_json(binary),
            to: json_far_side(self.check.far_side),
            callees: callee_rows(binary, &self.crossings)
                .iter()
                .map(|(outside, row)| callee_json(binary, *outside, row))
                .collect(),
            unresolved: unresolved_json(binary, &self.unfollowed),
            unreadable: json::items(binary, &self.unreadable),
            complete: self.complete(),
        };
        json::write(&leaving, out)
    }

    fn write_text(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let check = self.check;
        let subject = check.inside.lead_name(binary, self.all);
        let subject_at_start = subject.at_start();
        let verb =
            |one: &str, many: &str| if check.inside.one(binary) { one } else { many }.to_string();
        let groups = groups(binary, &self.crossings, Direction::Out, |crossing| {
            how_column(binary, crossing)
        });
        let used = text::count_text(binary, &outside_nodes(&groups));
        let unknown = text::calls_and_jumps(
            self.unfollowed
                .iter()
                .map(|unfollowed| &unfollowed.unresolved),
        );
        let far_side = far_side_text(binary, check.far_side, check.inside.it(binary), self.all);
        let lead = match (groups.is_empty(), self.unfollowed.is_empty()) {
            (true, true) if self.unreadable.is_empty() => {
                format!(
                    "{subject_at_start} {} nothing {far_side}.",
                    verb("uses", "use")
                )
            }
            (true, true) => format!(
                "In the code elfq can read, {subject} {} nothing {far_side}.",
                verb("uses", "use")
            ),
            (false, true) => format!(
                "{subject_at_start} {} {used} {far_side}:",
                verb("uses", "use")
            ),
            (true, false) if check.far_side.is_empty() => format!(
                "{subject_at_start} {} {unknown} that elfq can't follow:",
                verb("makes", "make")
            ),
            (true, false) => {
                let cant_follow: Vec<&Unresolved> =
                    self.unfollowed.iter().map(|u| &u.unresolved).collect();
                let go = text::where_they_go(&cant_follow);
                format!(
                    "{subject_at_start} {} {unknown} that elfq can't follow, which {go} to {}:",
                    verb("makes", "make"),
                    text::ranges_text(binary, check.far_side, self.all)
                )
            }
            (false, false) => format!(
                "{subject_at_start} {} {used} {far_side}; elfq can't follow {unknown}:",
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
                let (address, callee) = text::cant_follow_target(unresolved);
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
        let mut notes = check.inside.pattern_notes(binary, self.all);
        if !self.unreadable.is_empty() {
            notes.push(text::unreadable_text(binary, &self.unreadable, self.all));
        }
        let under = if listed {
            NotesUnder::Table
        } else {
            NotesUnder::FirstLine
        };
        write_notes(&notes, under, out)
    }
}

/// Whether an answer that leaves out `cant_follow`, the calls and jumps
/// in it that elfq can't follow, and `unreadable`, the functions whose code
/// elfq can't read, has everything, as far as elfq can tell. When the
/// answer is `limited` to some ranges, as with `--to`, a call to a known
/// address in them, as to a chip's ROM, is a whole answer, as `callers
/// --from` counts it: only a call through a pointer leaves it open.
pub(in crate::commands) fn complete<'u>(
    cant_follow: impl IntoIterator<Item = &'u Unresolved>,
    unreadable: &[usize],
    limited: bool,
) -> bool {
    unreadable.is_empty()
        && (cant_follow.into_iter()).all(|unresolved| limited && unresolved.target.is_some())
}

/// What uses the items inside from outside them: `callers RANGE...`,
/// `callers SYMBOL --from RANGE`, or a set, as `callers 'on_*'`.
pub(super) struct Entering<'a> {
    binary: &'a Binary,
    check: &'a RangeCheck<'a>,
    crossings: Vec<Crossing>,
    /// The calls and jumps from outside into code in the ranges that is
    /// outside every item, as in a chip's ROM.
    unfollowed: Vec<Unfollowed>,
    /// The functions in the ranges that code outside may call through a
    /// pointer, which elfq can't see. With `--from`, only when code in its
    /// ranges calls through a pointer, or elfq can't read it.
    through_pointer: Vec<usize>,
    /// The functions in the ranges of `--from`, outside the ranges, whose
    /// code elfq can't read, since it doesn't know where they end.
    unreadable: Vec<usize>,
    /// Whether the sentences name every range, section and function, for
    /// `--all`.
    all: bool,
}

impl<'a> Entering<'a> {
    /// What uses the items inside `boundary`, which `check` asks about,
    /// from outside it, in `binary`, whose graph is `graph`; `all` is for
    /// `--all`.
    pub(super) fn find(
        binary: &'a Binary,
        graph: &Graph,
        check: &'a RangeCheck<'a>,
        boundary: &Boundary,
        all: bool,
    ) -> Entering<'a> {
        let from = ItemsInRanges::new(binary, check.far_side);
        let inside = |index: usize| boundary.items().contains_item(index);
        // Code in the `--from` ranges that elfq can't read may use the
        // ranges in any way.
        let unreadable: Vec<usize> = from
            .unreadable()
            .into_iter()
            .filter(|&index| !inside(index))
            .collect();
        // Code anywhere may call through a pointer passed to it, but with
        // `--from`, only code in its ranges counts.
        let calls_through_pointer = || {
            graph.unresolved.iter().any(|unresolved| {
                unresolved.is_indirect()
                    && from.contains_item(unresolved.from)
                    && !inside(unresolved.from)
            })
        };
        let through_pointer =
            if check.far_side.is_empty() || !unreadable.is_empty() || calls_through_pointer() {
                boundary.items().callable_through_pointer(graph)
            } else {
                Vec::new()
            };
        let crossings = boundary.entering_from(check.far_side);
        Entering {
            binary,
            check,
            crossings: crossings.uses,
            unfollowed: crossings.unfollowed,
            through_pointer,
            unreadable,
            all,
        }
    }

    /// Whether the answer lists something, or says that something may
    /// cross.
    pub(super) fn crosses(&self) -> bool {
        !self.crossings.is_empty()
            || !self.unfollowed.is_empty()
            || !self.through_pointer.is_empty()
            || !self.unreadable.is_empty()
    }

    /// Whether the answer has everything that crosses, as far as elfq can
    /// tell.
    fn complete(&self) -> bool {
        self.through_pointer.is_empty() && self.unreadable.is_empty()
    }

    /// Writes the answer in `format`.
    pub(super) fn write(&self, format: Format, out: &mut dyn Write) -> Result<()> {
        match format {
            Format::Json => self.write_json(out),
            Format::Text | Format::Tree => self.write_text(out),
        }
    }

    fn write_json(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let item = |node: Node| json::Item::new(binary, node);
        let groups = groups(binary, &self.crossings, Direction::In, |crossing| {
            json::how_field(crossing.kind)
        });
        let entering = EnteringJson {
            ranges: json_ranges(self.check.inside.ranges()),
            picked: self.check.inside.picked_json(binary),
            from: json_far_side(self.check.far_side),
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
            unreadable: json::items(binary, &self.unreadable),
            through_pointer: json::items(binary, &self.through_pointer),
            complete: self.complete(),
        };
        json::write(&entering, out)
    }

    fn write_text(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let check = self.check;
        let subject = check.inside.lead_name(binary, self.all);
        let it = check.inside.it(binary);
        let groups = groups(binary, &self.crossings, Direction::In, |crossing| {
            how_column(binary, crossing)
        });
        let far_side = far_side_text(binary, check.far_side, it, self.all);
        // "Nothing outside X uses it", or "Nothing in Y uses X".
        let nothing = if check.far_side.is_empty() {
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
            let verb = if check.inside.one(binary) {
                "is"
            } else {
                "are"
            };
            writeln!(
                out,
                "{} {verb} used by {} {far_side}:",
                subject.at_start(),
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
        let mut notes = check.inside.pattern_notes(binary, self.all);
        if !self.through_pointer.is_empty() {
            let names: Vec<&str> = self
                .through_pointer
                .iter()
                .map(|&index| binary.items[index].name.as_str())
                .collect();
            notes.push(format!(
                "Calls through a pointer to {} aren't listed, since elfq can't see them.",
                text::names_text(&names, self.all)
            ));
        }
        if !self.unreadable.is_empty() {
            notes.push(text::unreadable_text(binary, &self.unreadable, self.all));
        }
        let under = if listed {
            NotesUnder::Table
        } else {
            NotesUnder::FirstLine
        };
        write_notes(&notes, under, out)
    }
}

/// What an answer's notes come under.
#[derive(Clone, Copy)]
enum NotesUnder {
    /// The table of its rows, after an empty line.
    Table,
    /// Its first line, right under it, when it listed no row.
    FirstLine,
}

/// Writes `notes`, the lines under an answer: the notes on how a set's
/// patterns matched first, then what the rows can't list. They come
/// together `under` the answer's table, after an empty line, or right
/// under its first line when it listed no row.
fn write_notes(notes: &[String], under: NotesUnder, out: &mut dyn Write) -> Result<()> {
    // An empty line sets the notes apart from the table above them.
    match under {
        NotesUnder::Table if !notes.is_empty() => writeln!(out)?,
        NotesUnder::Table | NotesUnder::FirstLine => {}
    }
    for note in notes {
        writeln!(out, "{note}")?;
    }
    Ok(())
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
                call: json::Unresolved::new(unresolved),
                veneer: unfollowed.veneer.map(item),
            }
        })
        .collect()
}
