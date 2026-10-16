//! `elfq FILE callees RANGE...` and `elfq FILE callers RANGE...`: what
//! crosses the edge of some ranges of memory, such as code in ITCM that
//! calls a function in flash.

use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::{Outcome, json};
use crate::binary::{AddressRange, Binary};
use crate::cli::Format;
use crate::graph::{Crossing, Edge, Graph, Node, Reference, Unresolved};
use crate::render::{Align, Table};

/// Which way the references cross the edge.
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
    /// The item outside the ranges.
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
    /// see.
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
    let edge = Edge::new(binary, graph, ranges);
    if let Some(range) = edge.empty_range() {
        if super::sections_in(binary, range).is_empty() {
            bail!(
                "nothing in the file is in {}; the overview lists the ranges that the file's \
                 sections occupy",
                super::range_argument(range)
            );
        }
        if !super::found_functions(binary) {
            bail!("the file has no symbols or unwind table, so elfq can't find its functions yet");
        }
        bail!(
            "elfq found no functions or data in {}",
            super::range_text(binary, range)
        );
    }
    match direction {
        Direction::Out => leaving(binary, &edge, ranges, format, out),
        Direction::In => entering(binary, &edge, ranges, format, out),
    }
}

fn leaving(
    binary: &Binary,
    edge: &Edge,
    ranges: &[AddressRange],
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    let crossings = edge.leaving();
    let unfollowed = edge.unfollowed();
    let unreadable = edge.unreadable();
    let outcome = outcome(crossings.is_empty() && unfollowed.is_empty() && unreadable.is_empty());
    if format == Format::Json {
        let callees = groups(binary, &crossings, |crossing| json::how(crossing.kind))
            .into_iter()
            .flat_map(|group| {
                group.rows.into_iter().map(move |row| CalleeJson {
                    item: json::Item::new(binary, group.outside),
                    caller: json::Item::new(binary, Node::Item(row.inside)),
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
                    how: call_or_jump(unresolved),
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
    let groups = groups(binary, &crossings, |crossing| {
        how(binary, crossing, Direction::Out)
    });
    let used = super::count_text(binary, &outside_nodes(&groups));
    let unknown = calls_and_jumps(&unfollowed);
    let lead = match (groups.is_empty(), unfollowed.is_empty()) {
        (true, true) => format!("{subject} {} nothing outside {it}.", verb("uses", "use")),
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

    if !groups.is_empty() || !unfollowed.is_empty() {
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
                Some(target) => ("(unknown code)", format!("{target:#x}")),
            };
            table.row(vec![
                callee.to_string(),
                address,
                String::new(),
                binary.items[unresolved.from].name.clone(),
                call_or_jump(&unresolved).to_string(),
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
        writeln!(out)?;
        writeln!(
            out,
            "elfq can't read the code of {}, since it doesn't know where {} {}.",
            super::names_text(&names),
            if names.len() == 1 { "it" } else { "they" },
            if names.len() == 1 { "ends" } else { "end" }
        )?;
    }
    Ok(outcome)
}

fn entering(
    binary: &Binary,
    edge: &Edge,
    ranges: &[AddressRange],
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    let crossings = edge.entering();
    let through_pointer = edge.callable_through_pointer();
    let outcome = outcome(crossings.is_empty() && through_pointer.is_empty());
    if format == Format::Json {
        let callers = groups(binary, &crossings, |crossing| json::how(crossing.kind))
            .into_iter()
            .flat_map(|group| {
                group.rows.into_iter().map(move |row| CallerJson {
                    item: json::Item::new(binary, group.outside),
                    callee: json::Item::new(binary, Node::Item(row.inside)),
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
    let groups = groups(binary, &crossings, |crossing| {
        how(binary, crossing, Direction::In)
    });
    if groups.is_empty() {
        writeln!(out, "Nothing outside {subject} uses {it}.")?;
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

/// [`Outcome::Crossed`] unless nothing crosses the edge, or may.
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
    rows: Vec<Row>,
}

/// The crossings between one pair of items that are worded the same way.
struct Row {
    inside: usize,
    how: &'static str,
    veneer: Option<usize>,
    sites: Vec<u64>,
}

/// Groups `crossings` by the item or import outside the ranges, largest
/// first, and each group's rows by the item inside and `how` it is used,
/// in the order of their first site.
fn groups(
    binary: &Binary,
    crossings: &[Crossing],
    how: impl Fn(&Crossing) -> &'static str,
) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for crossing in crossings {
        let how = how(crossing);
        let group = match groups
            .iter_mut()
            .position(|group| group.outside == crossing.outside)
        {
            Some(index) => &mut groups[index],
            None => {
                groups.push(Group {
                    outside: crossing.outside,
                    rows: Vec::new(),
                });
                groups.last_mut().unwrap()
            }
        };
        match group.rows.iter_mut().find(|row| {
            (row.inside, row.how, row.veneer) == (crossing.inside, how, crossing.veneer)
        }) {
            Some(row) => row.sites.push(crossing.site),
            None => group.rows.push(Row {
                inside: crossing.inside,
                how,
                veneer: crossing.veneer,
                sites: vec![crossing.site],
            }),
        }
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
/// outside the ranges only on the first line of its group.
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
            table.row(vec![
                name,
                address,
                size,
                binary.items[row.inside].name.clone(),
                row.how.to_string(),
                super::addresses(&row.sites),
            ]);
        }
    }
}

/// How a crossing in `direction` uses its target, for the HOW column, as
/// `callers` and `callees` word it, with "via veneer" after a call through
/// one.
fn how(binary: &Binary, crossing: &Crossing, direction: Direction) -> &'static str {
    let to = match direction {
        Direction::Out => crossing.outside,
        Direction::In => Node::Item(crossing.inside),
    };
    let reference = Reference {
        from: crossing.inside,
        to,
        kind: crossing.kind,
        site: crossing.site,
    };
    let how = super::how(binary, &reference);
    match (crossing.veneer, how) {
        (None, how) => how,
        (Some(_), "call") => "call via veneer",
        (Some(_), "tail call") => "tail call via veneer",
        (Some(_), how) => how,
    }
}

fn call_or_jump(unresolved: &Unresolved) -> &'static str {
    if unresolved.is_call { "call" } else { "jump" }
}

/// "1 call", "2 jumps" or "1 call and 2 jumps".
fn calls_and_jumps(unresolved: &[Unresolved]) -> String {
    let calls = unresolved.iter().filter(|u| u.is_call).count();
    let jumps = unresolved.len() - calls;
    match (calls, jumps) {
        (_, 0) => super::plural(calls, "call", "calls"),
        (0, _) => super::plural(jumps, "jump", "jumps"),
        _ => format!(
            "{} and {}",
            super::plural(calls, "call", "calls"),
            super::plural(jumps, "jump", "jumps")
        ),
    }
}

/// The calls and jumps that elfq can't follow, one per function, kind of
/// call and target, with their sites.
fn group_unfollowed(unfollowed: &[Unresolved]) -> Vec<(Unresolved, Vec<u64>)> {
    let mut groups: Vec<(Unresolved, Vec<u64>)> = Vec::new();
    for unresolved in unfollowed {
        let key = |u: &Unresolved| (u.from, u.is_call, u.target);
        match groups
            .iter_mut()
            .find(|(first, _)| key(first) == key(unresolved))
        {
            Some((_, sites)) => sites.push(unresolved.site),
            None => groups.push((*unresolved, vec![unresolved.site])),
        }
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
