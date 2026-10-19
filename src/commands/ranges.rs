//! `elfq FILE callees RANGE...` and `elfq FILE callers RANGE...`: what
//! crosses the boundary of some ranges of memory, such as code in ITCM that
//! calls a function in flash.

use std::collections::HashMap;
use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::{Outcome, json, text};
use crate::binary::{AddressRange, Binary};
use crate::cli::Format;
use crate::graph::{Boundary, Crossing, Graph, Node, ReferenceKind, Unfollowed};
use crate::render::{Align, Table};

/// Which way the references cross the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Out of the ranges: what the items in them use outside them.
    Out,
    /// Into the ranges: what uses the items in them from outside.
    In,
}

#[derive(Serialize)]
struct LeavingJson<'a> {
    ranges: Vec<json::Range>,
    callees: Vec<CalleeJson<'a>>,
    /// The calls and jumps in the ranges that elfq can't follow, and that
    /// may leave them.
    unresolved: Vec<UnresolvedJson<'a>>,
    /// The functions in the ranges whose code elfq can't read, since it
    /// doesn't know where they end.
    unreadable: Vec<json::Item<'a>>,
    /// Whether `callees` has everything the ranges use outside them, as far
    /// as elfq can tell: `false` when `unresolved` or `unreadable` isn't
    /// empty.
    complete: bool,
}

#[derive(Serialize)]
struct CalleeJson<'a> {
    /// The item or import outside the ranges.
    item: json::Item<'a>,
    /// The item in the ranges that uses it.
    caller: json::Item<'a>,
    how: &'static str,
    /// The linker's veneer that the call goes through, or `null`.
    veneer: Option<json::Item<'a>>,
    sites: Vec<String>,
}

#[derive(Serialize)]
struct UnresolvedJson<'a> {
    /// The function it is in.
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
    callers: Vec<CallerJson<'a>>,
    /// The functions in the ranges whose address is taken or stored, so
    /// that code outside may call them through a pointer, which elfq can't
    /// see. A pointer in the vector table doesn't count, since only the
    /// processor calls through it.
    through_pointer: Vec<json::Item<'a>>,
    /// Whether `callers` has everything outside the ranges that uses them,
    /// as far as elfq can tell: `false` when `through_pointer` isn't empty.
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

/// Lists what crosses the boundary of `ranges` in `direction`, every
/// crossing, and with `all`, every name in its sentences. The answer is
/// [`Outcome::Crossed`] when something does, or may.
pub fn run(
    binary: &Binary,
    graph: &Graph,
    ranges: &[AddressRange],
    direction: Direction,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    let boundary = Boundary::new(binary, graph, ranges);
    if let Some(range) = boundary.empty_range() {
        let place = super::empty_place(binary, Some(range))?;
        bail!("elfq found no functions or data in {place}");
    }
    let nothing_crosses = match direction {
        Direction::Out => {
            let leaving = Leaving {
                binary,
                ranges,
                crossings: boundary.leaving(),
                unfollowed: boundary.unfollowed(),
                unreadable: boundary.unreadable(),
                all,
            };
            match format {
                Format::Json => leaving.write_json(out)?,
                // `--tree` is refused: there is no tree.
                Format::Text | Format::Tree => leaving.write_text(out)?,
            }
            leaving.crossings.is_empty()
                && leaving.unfollowed.is_empty()
                && leaving.unreadable.is_empty()
        }
        Direction::In => {
            let entering = Entering {
                binary,
                ranges,
                crossings: boundary.entering(),
                through_pointer: boundary.callable_through_pointer(),
                all,
            };
            match format {
                Format::Json => entering.write_json(out)?,
                Format::Text | Format::Tree => entering.write_text(out)?,
            }
            entering.crossings.is_empty() && entering.through_pointer.is_empty()
        }
    };
    if nothing_crosses {
        Ok(Outcome::Answered)
    } else {
        Ok(Outcome::Crossed)
    }
}

/// What the items in the ranges use outside them: `callees RANGE...`.
struct Leaving<'a> {
    binary: &'a Binary,
    ranges: &'a [AddressRange],
    crossings: Vec<Crossing>,
    /// The calls and jumps in the ranges that elfq can't follow, and that
    /// may leave them.
    unfollowed: Vec<Unfollowed>,
    /// The functions in the ranges whose code elfq can't read, since it
    /// doesn't know where they end.
    unreadable: Vec<usize>,
    /// Whether the sentences name every range, section and function, for
    /// `--all`.
    all: bool,
}

impl Leaving<'_> {
    fn write_json(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let item = |node: Node| json::Item::new(binary, node);
        let groups = groups(binary, &self.crossings, Direction::Out, |crossing| {
            json::how_field(crossing.kind)
        });
        let leaving = LeavingJson {
            ranges: json_ranges(self.ranges),
            callees: each_row(groups)
                .map(|(outside, row)| CalleeJson {
                    item: item(outside),
                    caller: item(row.inside),
                    how: row.how,
                    veneer: row.veneer.map(|veneer| item(Node::Item(veneer))),
                    sites: json::addresses(&row.sites),
                })
                .collect(),
            unresolved: self
                .unfollowed
                .iter()
                .map(|unfollowed| {
                    let unresolved = &unfollowed.unresolved;
                    UnresolvedJson {
                        caller: item(Node::Item(unresolved.from)),
                        site: json::address(unresolved.site),
                        how: text::call_or_jump(unresolved),
                        target: unresolved.target.map(json::address),
                        veneer: unfollowed.veneer.map(|veneer| item(Node::Item(veneer))),
                    }
                })
                .collect(),
            unreadable: self
                .unreadable
                .iter()
                .map(|&index| item(Node::Item(index)))
                .collect(),
            complete: self.unfollowed.is_empty() && self.unreadable.is_empty(),
        };
        json::write(&leaving, out)
    }

    fn write_text(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let one_range = self.ranges.len() == 1;
        let subject = ranges_text(binary, self.ranges, self.all);
        let it = if one_range { "it" } else { "them" };
        let verb = |one: &str, many: &str| if one_range { one } else { many }.to_string();
        let groups = groups(binary, &self.crossings, Direction::Out, |crossing| {
            how_column(binary, crossing)
        });
        let used = text::count_text(binary, &outside_nodes(&groups));
        let unknown = text::calls_and_jumps(
            self.unfollowed
                .iter()
                .map(|unfollowed| &unfollowed.unresolved),
        );
        let lead = match (groups.is_empty(), self.unfollowed.is_empty()) {
            (true, true) if self.unreadable.is_empty() => {
                format!("{subject} {} nothing outside {it}.", verb("uses", "use"))
            }
            (true, true) => format!(
                "{subject} {} nothing outside {it} in the code elfq can read.",
                verb("uses", "use")
            ),
            (false, true) => format!("{subject} {} {used} outside {it}:", verb("uses", "use")),
            (true, false) => format!(
                "{subject} {} {unknown} that elfq can't follow:",
                verb("makes", "make")
            ),
            (false, false) => format!(
                "{subject} {} {used} outside {it}, and {} {unknown} that elfq can't follow:",
                verb("uses", "use"),
                verb("makes", "make")
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
                let how = match (unfollowed.veneer, unresolved.is_call) {
                    (Some(_), true) => "call via veneer",
                    (Some(_), false) => "jump via veneer",
                    (None, _) => text::call_or_jump(unresolved),
                };
                table.row(vec![
                    address,
                    String::new(),
                    how.to_string(),
                    text::at_column(&sites),
                    names_cell(callee, Direction::Out, &binary.items[unresolved.from].name),
                ]);
            }
            write_groups(binary, &groups, Direction::Out, &mut table);
            table.write(out)?;
        }
        if !self.unreadable.is_empty() {
            let names: Vec<&str> = self
                .unreadable
                .iter()
                .map(|&index| binary.items[index].name.as_str())
                .collect();
            if listed {
                writeln!(out)?;
            }
            // As `callees SYMBOL` says it.
            writeln!(
                out,
                "elfq doesn't know where {} {}, so it can't read {} code.",
                text::names_text(&names, self.all),
                if names.len() == 1 { "ends" } else { "end" },
                if names.len() == 1 { "its" } else { "their" }
            )?;
        }
        Ok(())
    }
}

/// What uses the items in the ranges from outside them: `callers
/// RANGE...`.
struct Entering<'a> {
    binary: &'a Binary,
    ranges: &'a [AddressRange],
    crossings: Vec<Crossing>,
    /// The functions in the ranges that code outside may call through a
    /// pointer, which elfq can't see.
    through_pointer: Vec<usize>,
    /// Whether the sentences name every range, section and function, for
    /// `--all`.
    all: bool,
}

impl Entering<'_> {
    fn write_json(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let item = |node: Node| json::Item::new(binary, node);
        let groups = groups(binary, &self.crossings, Direction::In, |crossing| {
            json::how_field(crossing.kind)
        });
        let entering = EnteringJson {
            ranges: json_ranges(self.ranges),
            callers: each_row(groups)
                .map(|(outside, row)| CallerJson {
                    item: item(outside),
                    callee: item(row.inside),
                    how: row.how,
                    veneer: row.veneer.map(|veneer| item(Node::Item(veneer))),
                    sites: json::addresses(&row.sites),
                })
                .collect(),
            through_pointer: self
                .through_pointer
                .iter()
                .map(|&index| item(Node::Item(index)))
                .collect(),
            complete: self.through_pointer.is_empty(),
        };
        json::write(&entering, out)
    }

    fn write_text(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let subject = ranges_text(binary, self.ranges, self.all);
        let it = if self.ranges.len() == 1 { "it" } else { "them" };
        let groups = groups(binary, &self.crossings, Direction::In, |crossing| {
            how_column(binary, crossing)
        });
        if groups.is_empty() && self.through_pointer.is_empty() {
            writeln!(out, "Nothing outside {subject} uses {it}.")?;
        } else if groups.is_empty() {
            writeln!(out, "Nothing outside {subject} uses {it} directly.")?;
        } else {
            writeln!(
                out,
                "{subject} {} used by {} outside {it}:",
                if self.ranges.len() == 1 { "is" } else { "are" },
                text::count_text(binary, &outside_nodes(&groups))
            )?;
            writeln!(out)?;
            let mut table = crossings_table(Direction::In);
            write_groups(binary, &groups, Direction::In, &mut table);
            table.write(out)?;
        }
        if !self.through_pointer.is_empty() {
            let names: Vec<&str> = self
                .through_pointer
                .iter()
                .map(|&index| binary.items[index].name.as_str())
                .collect();
            if !groups.is_empty() {
                writeln!(out)?;
            }
            writeln!(
                out,
                "Calls through a pointer to {} aren't listed, since elfq can't see them.",
                text::names_text(&names, self.all)
            )?;
        }
        Ok(())
    }
}

/// The crossings with one item or import outside the ranges.
struct Group {
    outside: Node,
    rows: Vec<CrossingRow>,
}

/// The crossings between one item in the ranges and the one outside, with
/// the same words and veneer.
struct CrossingRow {
    inside: Node,
    how: &'static str,
    veneer: Option<usize>,
    sites: Vec<u64>,
}

/// Groups `crossings` by the item or import outside the ranges, largest
/// first, and each group's rows by the item inside, `how` it is used and
/// the veneer, in the order of their first site.
fn groups(
    binary: &Binary,
    crossings: &[Crossing],
    direction: Direction,
    how: impl Fn(&Crossing) -> &'static str,
) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    // Where each group is in `groups`, and each row in its group's rows,
    // since a large range has thousands.
    let mut group_index: HashMap<Node, usize> = HashMap::new();
    let mut row_index: HashMap<(Node, Node, &'static str, Option<usize>), usize> = HashMap::new();
    for crossing in crossings {
        let (outside, inside) = match direction {
            Direction::Out => (crossing.to, Node::Item(crossing.from)),
            Direction::In => (Node::Item(crossing.from), crossing.to),
        };
        let how = how(crossing);
        let group = *group_index.entry(outside).or_insert_with(|| {
            groups.push(Group {
                outside,
                rows: Vec::new(),
            });
            groups.len() - 1
        });
        let rows = &mut groups[group].rows;
        let row = *row_index
            .entry((outside, inside, how, crossing.veneer))
            .or_insert_with(|| {
                rows.push(CrossingRow {
                    inside,
                    how,
                    veneer: crossing.veneer,
                    sites: Vec::new(),
                });
                rows.len() - 1
            });
        rows[row].sites.push(crossing.site);
    }
    groups.sort_by(|a, b| {
        b.outside
            .size(binary)
            .cmp(&a.outside.size(binary))
            .then_with(|| a.outside.name(binary).cmp(b.outside.name(binary)))
    });
    for group in &mut groups {
        for row in &mut group.rows {
            row.sites.sort();
        }
        group.rows.sort_by_key(|row| row.sites[0]);
    }
    groups
}

fn outside_nodes(groups: &[Group]) -> Vec<Node> {
    groups.iter().map(|group| group.outside).collect()
}

/// Each row of `groups`, with the item or import outside the ranges that
/// its group is for, as JSON lists them.
fn each_row(groups: Vec<Group>) -> impl Iterator<Item = (Node, CrossingRow)> {
    groups
        .into_iter()
        .flat_map(|group| group.rows.into_iter().map(move |row| (group.outside, row)))
}

fn json_ranges(ranges: &[AddressRange]) -> Vec<json::Range> {
    ranges
        .iter()
        .map(|&range| json::Range::new(range))
        .collect()
}

/// The table for a range check's rows: the address and size of the item
/// outside the ranges, how one item uses the other and where, then both
/// names, as [`names_cell`] writes them. The names come last, so that a
/// long one that wraps moves no column.
fn crossings_table(direction: Direction) -> Table {
    let names = match direction {
        Direction::Out => "CALLEE",
        Direction::In => "CALLER",
    };
    Table::new(&[
        ("ADDRESS", Align::Left),
        ("SIZE", Align::Right),
        ("HOW", Align::Left),
        ("AT", Align::Left),
        (names, Align::Left),
    ])
}

/// The last cell of a range check's row: the item outside the ranges, then
/// the one in them, as "memcpy, used by apply_filter" for `callees
/// RANGE...` or "process_frame, uses crc32" for `callers RANGE...`.
fn names_cell(outside: &str, direction: Direction, inside: &str) -> String {
    let relation = match direction {
        Direction::Out => "used by",
        Direction::In => "uses",
    };
    format!("{outside}, {relation} {inside}")
}

/// Adds a line to `table` for each row of `groups`. Every line names both
/// items, so that each stands alone, as when `grep` picks it out.
fn write_groups(binary: &Binary, groups: &[Group], direction: Direction, table: &mut Table) {
    for group in groups {
        let (address, size) = match group.outside {
            Node::Item(item) => {
                let item = &binary.items[item];
                (format!("{:#x}", item.address), text::size_text(item))
            }
            Node::Import(_) => (String::new(), String::new()),
        };
        let outside = group.outside.name(binary);
        for row in &group.rows {
            table.row(vec![
                address.clone(),
                size.clone(),
                row.how.to_string(),
                text::at_column(&row.sites),
                names_cell(outside, direction, row.inside.name(binary)),
            ]);
        }
    }
}

/// How a crossing uses what is on the other side, for the HOW column, with
/// "via veneer" after a call through one.
fn how_column(binary: &Binary, crossing: &Crossing) -> &'static str {
    match (crossing.kind, crossing.veneer) {
        (ReferenceKind::Call, Some(_)) => "call via veneer",
        (ReferenceKind::TailCall, Some(_)) => "tail call via veneer",
        (kind, _) => text::how_column(binary, crossing.to, kind),
    }
}

/// The calls and jumps that elfq can't follow, one per function, kind of
/// call, target and veneer, with their sites.
fn group_unfollowed(unfollowed: &[Unfollowed]) -> Vec<(Unfollowed, Vec<u64>)> {
    let mut groups: Vec<(Unfollowed, Vec<u64>)> = Vec::new();
    // Where each group is in `groups`.
    let mut index: HashMap<(usize, bool, Option<u64>, Option<usize>), usize> = HashMap::new();
    for row in unfollowed {
        let unresolved = &row.unresolved;
        let key = (
            unresolved.from,
            unresolved.is_call,
            unresolved.target,
            row.veneer,
        );
        let group = *index.entry(key).or_insert_with(|| {
            groups.push((*row, Vec::new()));
            groups.len() - 1
        });
        groups[group].1.push(unresolved.site);
    }
    groups
}

/// The ranges as `--range` takes them, with the sections in each, joined
/// into a phrase, every range and section with `all`.
fn ranges_text(binary: &Binary, ranges: &[AddressRange], all: bool) -> String {
    let texts: Vec<String> = ranges
        .iter()
        .map(|&range| text::range_text(binary, range, all))
        .collect();
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    text::names_text(&texts, all)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::graph::Unresolved;

    #[test]
    fn a_call_to_code_outside_every_function_has_a_row() {
        // As when code in ITCM calls a routine in a microcontroller's ROM,
        // which the file has no symbol for.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-cortex-m7.elf");
        let binary = Binary::load(&path).unwrap();
        let mut graph = Graph::build(&binary);
        let crc32 = binary
            .items
            .iter()
            .position(|item| item.name == "crc32")
            .unwrap();
        graph.unresolved.push(Unresolved {
            from: crc32,
            site: 0x20,
            is_call: true,
            target: Some(0x1fff_0100),
        });
        let ranges = ["0x0+256K".parse().unwrap()];
        let mut out = Vec::new();
        let outcome = run(
            &binary,
            &graph,
            &ranges,
            Direction::Out,
            false,
            Format::Text,
            &mut out,
        );
        assert_eq!(outcome.unwrap(), Outcome::Crossed);
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("\n0x1fff0100        call  "), "{out}");
        assert!(
            out.contains("  0x20  (outside any function), used by crc32\n"),
            "{out}"
        );
    }
}
