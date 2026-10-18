//! `elfq FILE callees RANGE...` and `elfq FILE callers RANGE...`: what
//! crosses the boundary of some ranges of memory, such as code in ITCM that
//! calls a function in flash.

use std::collections::HashMap;
use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::{Outcome, json};
use crate::binary::{AddressRange, Binary};
use crate::cli::Format;
use crate::graph::{Boundary, Crossing, Graph, Node, Unresolved};
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
struct Leaving<'a> {
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
}

#[derive(Serialize)]
struct Entering<'a> {
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

pub fn run(
    binary: &Binary,
    graph: &Graph,
    ranges: &[AddressRange],
    direction: Direction,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    let boundary = Boundary::new(binary, graph, ranges);
    if let Some(range) = boundary.empty_range() {
        let place = super::empty_place(binary, Some(range))?;
        bail!("elfq found no functions or data in {place}");
    }
    match direction {
        Direction::Out => leaving(binary, &boundary, ranges, format, out),
        Direction::In => entering(binary, &boundary, ranges, format, out),
    }
}

fn leaving(
    binary: &Binary,
    boundary: &Boundary,
    ranges: &[AddressRange],
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    let crossings = boundary.leaving();
    let unfollowed = boundary.unfollowed();
    let unreadable = boundary.unreadable();
    let outcome = outcome(crossings.is_empty() && unfollowed.is_empty() && unreadable.is_empty());
    if format == Format::Json {
        let callees = groups(binary, &crossings, Direction::Out, |crossing| {
            json::how_field(crossing.kind)
        })
        .into_iter()
        .flat_map(|group| {
            group.rows.into_iter().map(move |row| CalleeJson {
                item: json::Item::new(binary, group.outside),
                caller: json::Item::new(binary, row.inside),
                how: row.how,
                veneer: row
                    .veneer
                    .map(|veneer| json::Item::new(binary, Node::Item(veneer))),
                sites: row.sites.iter().map(|&site| json::address(site)).collect(),
            })
        })
        .collect();
        let leaving = Leaving {
            ranges: ranges
                .iter()
                .map(|&range| json::Range::new(range))
                .collect(),
            callees,
            unresolved: unfollowed
                .iter()
                .map(|unresolved| UnresolvedJson {
                    caller: json::Item::new(binary, Node::Item(unresolved.from)),
                    site: json::address(unresolved.site),
                    how: super::call_or_jump(unresolved),
                    target: unresolved.target.map(json::address),
                })
                .collect(),
            unreadable: unreadable
                .iter()
                .map(|&index| json::Item::new(binary, Node::Item(index)))
                .collect(),
            complete: unfollowed.is_empty() && unreadable.is_empty(),
        };
        json::write(&leaving, out)?;
        return Ok(outcome);
    }

    let subject = ranges_text(binary, ranges);
    let it = if ranges.len() == 1 { "it" } else { "them" };
    let verb = |one: &str, many: &str| if ranges.len() == 1 { one } else { many }.to_string();
    let groups = groups(binary, &crossings, Direction::Out, |crossing| {
        super::how_column(binary, crossing.to, crossing.kind)
    });
    let used = super::count_text(binary, &outside_nodes(&groups));
    let unknown = super::calls_and_jumps(&unfollowed);
    let lead = match (groups.is_empty(), unfollowed.is_empty()) {
        (true, true) if unreadable.is_empty() => {
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

    let listed = !groups.is_empty() || !unfollowed.is_empty();
    if listed {
        writeln!(out)?;
        let mut table = Table::new(&[
            ("CALLEE", Align::Left),
            ("ADDRESS", Align::Left),
            ("SIZE", Align::Right),
            ("CALLER", Align::Left),
            ("HOW", Align::Left),
            ("AT", Align::Left),
        ]);
        // What elfq can't follow comes first, since it may hide more.
        for (unresolved, sites) in group_unfollowed(&unfollowed) {
            let (callee, address) = match unresolved.target {
                None => ("(through a pointer)", String::new()),
                Some(target) => ("(outside any function)", format!("{target:#x}")),
            };
            table.row(vec![
                callee.to_string(),
                address,
                String::new(),
                binary.items[unresolved.from].name.clone(),
                super::call_or_jump(&unresolved).to_string(),
                super::addresses(&sites),
            ]);
        }
        write_groups(binary, &groups, &mut table);
        table.write(out)?;
    }
    if !unreadable.is_empty() {
        let names: Vec<&str> = unreadable
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
            super::names_text(&names),
            if names.len() == 1 { "ends" } else { "end" },
            if names.len() == 1 { "its" } else { "their" }
        )?;
    }
    Ok(outcome)
}

fn entering(
    binary: &Binary,
    boundary: &Boundary,
    ranges: &[AddressRange],
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    let crossings = boundary.entering();
    let through_pointer = boundary.callable_through_pointer();
    let outcome = outcome(crossings.is_empty() && through_pointer.is_empty());
    if format == Format::Json {
        let callers = groups(binary, &crossings, Direction::In, |crossing| {
            json::how_field(crossing.kind)
        })
        .into_iter()
        .flat_map(|group| {
            group.rows.into_iter().map(move |row| CallerJson {
                item: json::Item::new(binary, group.outside),
                callee: json::Item::new(binary, row.inside),
                how: row.how,
                veneer: row
                    .veneer
                    .map(|veneer| json::Item::new(binary, Node::Item(veneer))),
                sites: row.sites.iter().map(|&site| json::address(site)).collect(),
            })
        })
        .collect();
        let entering = Entering {
            ranges: ranges
                .iter()
                .map(|&range| json::Range::new(range))
                .collect(),
            callers,
            through_pointer: through_pointer
                .iter()
                .map(|&index| json::Item::new(binary, Node::Item(index)))
                .collect(),
            complete: through_pointer.is_empty(),
        };
        json::write(&entering, out)?;
        return Ok(outcome);
    }

    let subject = ranges_text(binary, ranges);
    let it = if ranges.len() == 1 { "it" } else { "them" };
    let groups = groups(binary, &crossings, Direction::In, |crossing| {
        super::how_column(binary, crossing.to, crossing.kind)
    });
    if groups.is_empty() && through_pointer.is_empty() {
        writeln!(out, "Nothing outside {subject} uses {it}.")?;
    } else if groups.is_empty() {
        writeln!(out, "Nothing outside {subject} uses {it} directly.")?;
    } else {
        writeln!(
            out,
            "{subject} {} used by {} outside {it}:",
            if ranges.len() == 1 { "is" } else { "are" },
            super::count_text(binary, &outside_nodes(&groups))
        )?;
        writeln!(out)?;
        let mut table = Table::new(&[
            ("CALLER", Align::Left),
            ("ADDRESS", Align::Left),
            ("SIZE", Align::Right),
            ("CALLEE", Align::Left),
            ("HOW", Align::Left),
            ("AT", Align::Left),
        ]);
        write_groups(binary, &groups, &mut table);
        table.write(out)?;
    }
    if !through_pointer.is_empty() {
        let names: Vec<&str> = through_pointer
            .iter()
            .map(|&index| binary.items[index].name.as_str())
            .collect();
        if !groups.is_empty() {
            writeln!(out)?;
        }
        writeln!(
            out,
            "Calls through a pointer to {} aren't listed, since elfq can't see them.",
            super::names_text(&names)
        )?;
    }
    Ok(outcome)
}

/// [`Outcome::Crossed`] unless nothing crosses the boundary, or may.
fn outcome(nothing_crosses: bool) -> Outcome {
    if nothing_crosses {
        Outcome::Answered
    } else {
        Outcome::Crossed
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
    let size = |node: Node| match node {
        Node::Item(index) => binary.items[index].size,
        Node::Import(_) => 0,
    };
    groups.sort_by(|a, b| {
        size(b.outside)
            .cmp(&size(a.outside))
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

/// Adds a line to `table` for each row of `groups`, naming the item
/// outside the ranges only on the first line of its group, and saying "via
/// veneer" after a call through one.
fn write_groups(binary: &Binary, groups: &[Group], table: &mut Table) {
    for group in groups {
        for (index, row) in group.rows.iter().enumerate() {
            let (name, address, size) = match (index, group.outside) {
                (0, Node::Item(item)) => {
                    let item = &binary.items[item];
                    (
                        item.name.clone(),
                        format!("{:#x}", item.address),
                        super::size_text(item),
                    )
                }
                (0, node) => (node.name(binary).to_string(), String::new(), String::new()),
                _ => (String::new(), String::new(), String::new()),
            };
            let how = match (row.veneer, row.how) {
                (Some(_), "call") => "call via veneer",
                (Some(_), "tail call") => "tail call via veneer",
                (_, how) => how,
            };
            table.row(vec![
                name,
                address,
                size,
                row.inside.name(binary).to_string(),
                how.to_string(),
                super::addresses(&row.sites),
            ]);
        }
    }
}

/// The calls and jumps that elfq can't follow, one per function, kind of
/// call and target, with their sites.
fn group_unfollowed(unfollowed: &[Unresolved]) -> Vec<(Unresolved, Vec<u64>)> {
    let mut groups: Vec<(Unresolved, Vec<u64>)> = Vec::new();
    // Where each group is in `groups`.
    let mut index: HashMap<(usize, bool, Option<u64>), usize> = HashMap::new();
    for unresolved in unfollowed {
        let key = (unresolved.from, unresolved.is_call, unresolved.target);
        let group = *index.entry(key).or_insert_with(|| {
            groups.push((*unresolved, Vec::new()));
            groups.len() - 1
        });
        groups[group].1.push(unresolved.site);
    }
    groups
}

/// The ranges as `--range` takes them, with the sections in each, joined
/// into a phrase.
fn ranges_text(binary: &Binary, ranges: &[AddressRange]) -> String {
    let texts: Vec<String> = ranges
        .iter()
        .map(|&range| super::range_text(binary, range))
        .collect();
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    super::names_text(&texts)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

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
            Format::Text,
            &mut out,
        );
        assert_eq!(outcome.unwrap(), Outcome::Crossed);
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains("\n(outside any function)  0x1fff0100        crc32            call "),
            "{out}"
        );
    }
}
