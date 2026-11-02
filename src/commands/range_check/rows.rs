//! The rows of a range check's tables: each groups the uses of one item
//! on the far side by one item in the ranges, with how and where. `reach`
//! and `diff` print these rows too.

use std::collections::HashMap;

use serde::Serialize;

use super::Direction;
use crate::binary::{AddressRange, Binary};
use crate::commands::{json, text};
use crate::graph::{Crossing, Node, ReferenceKind, Unfollowed};
use crate::render::{Align, Table};

/// A row of `callees RANGE --json`.
#[derive(Serialize)]
pub(crate) struct CalleeJson<'a> {
    /// The item or import outside the ranges.
    item: json::Item<'a>,
    /// The item in the ranges that uses it.
    caller: json::Item<'a>,
    how: &'static str,
    /// The linker's veneer that the call goes through, or `null`.
    veneer: Option<json::Item<'a>>,
    sites: Vec<String>,
}

/// The ranges of `--to` or `--from` for JSON, or `None` when there are none.
pub(crate) fn json_far_side(far_side: &[AddressRange]) -> Option<Vec<json::Range>> {
    (!far_side.is_empty()).then(|| json_ranges(far_side))
}

/// The crossings with one item or import outside the ranges.
pub(super) struct Group {
    outside: Node,
    rows: Vec<CrossingRow>,
}

/// The crossings between one item in the ranges and the one outside, said
/// in the same words, with the same veneer.
pub(crate) struct CrossingRow {
    pub(in crate::commands) inside: Node,
    /// How the first of them uses what it uses. Its words are those of all
    /// of them (see [`groups`]).
    pub(in crate::commands) kind: ReferenceKind,
    pub(in crate::commands) veneer: Option<usize>,
    pub(super) sites: Vec<u64>,
}

/// The rows of `callees RANGE --json` for `crossings`, which leave the
/// ranges: for each item or import outside them, largest first, a row for
/// each item in them that uses it in one way, through one veneer or none,
/// with the item outside.
pub(crate) fn callee_rows(binary: &Binary, crossings: &[Crossing]) -> Vec<(Node, CrossingRow)> {
    let groups = groups(binary, crossings, Direction::Out, |crossing| {
        json::how_field(crossing.kind)
    });
    each_row(groups).collect()
}

/// The JSON of a row of `callees RANGE`, in which an item in the ranges
/// uses `outside`.
pub(crate) fn callee_json<'a>(
    binary: &'a Binary,
    outside: Node,
    row: &CrossingRow,
) -> CalleeJson<'a> {
    let item = |node: Node| json::Item::new(binary, node);
    CalleeJson {
        item: item(outside),
        caller: item(row.inside),
        how: json::how_field(row.kind),
        veneer: row.veneer.map(|veneer| item(Node::Item(veneer))),
        sites: json::addresses(&row.sites),
    }
}

/// Groups `crossings` by the item or import outside the ranges, largest
/// first, and each group's rows by the item inside, how it uses the other
/// in the words that `how` gives, and the veneer, in the order of their
/// first site. Where the words are the same for two kinds of use, as the
/// HOW column says "uses" both for reading data and for taking its address,
/// so is the row.
pub(super) fn groups(
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
                    kind: crossing.kind,
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

pub(super) fn outside_nodes(groups: &[Group]) -> Vec<Node> {
    groups.iter().map(|group| group.outside).collect()
}

/// Each row of `groups`, with the item or import outside the ranges that
/// its group is for, as JSON lists them.
pub(super) fn each_row(groups: Vec<Group>) -> impl Iterator<Item = (Node, CrossingRow)> {
    groups
        .into_iter()
        .flat_map(|group| group.rows.into_iter().map(move |row| (group.outside, row)))
}

pub(super) fn json_ranges(ranges: &[AddressRange]) -> Vec<json::Range> {
    ranges
        .iter()
        .map(|&range| json::Range::new(range))
        .collect()
}

/// The table for a range check's rows (see [`crossings_columns`]).
pub(super) fn crossings_table(direction: Direction) -> Table {
    Table::new(&crossings_columns(direction))
}

/// The columns of a range check's rows: the address and size of the item
/// outside the ranges, how one item uses the other and where, then both
/// names, as [`names_cell`] writes them. The names come last, so that a
/// long one that wraps moves no column.
pub(crate) fn crossings_columns(direction: Direction) -> [(&'static str, Align); 5] {
    let names = match direction {
        Direction::Out => "CALLEE",
        Direction::In => "CALLER",
    };
    [
        ("ADDRESS", Align::Left),
        ("SIZE", Align::Right),
        ("HOW", Align::Left),
        ("AT", Align::Left),
        (names, Align::Left),
    ]
}

/// The last cell of a range check's row: the item outside the ranges, then
/// the one in them, as "memcpy, used by apply_filter" for `callees
/// RANGE...` or "process_frame, uses crc32" for `callers RANGE...`.
pub(super) fn names_cell(outside: &str, direction: Direction, inside: &str) -> String {
    let relation = match direction {
        Direction::Out => "used by",
        Direction::In => "uses",
    };
    format!("{outside}, {relation} {inside}")
}

/// Adds a line to `table` for each row of `groups`.
pub(super) fn write_groups(
    binary: &Binary,
    groups: &[Group],
    direction: Direction,
    table: &mut Table,
) {
    for group in groups {
        for row in &group.rows {
            table.row(row_cells(binary, group.outside, row, direction));
        }
    }
}

/// The cells of a range check's row, in [`crossings_columns`], where an
/// item in the ranges uses `outside`, or `outside` uses one in them. Every
/// line names both items, so that each stands alone, as when `grep` picks
/// it out.
pub(crate) fn row_cells(
    binary: &Binary,
    outside: Node,
    row: &CrossingRow,
    direction: Direction,
) -> Vec<String> {
    let [address, size] = text::address_and_size(binary, outside);
    let used = match direction {
        Direction::Out => outside,
        Direction::In => row.inside,
    };
    vec![
        address,
        size,
        text::how_via(binary, used, row.kind, row.veneer).to_string(),
        text::at_column(&row.sites),
        names_cell(outside.name(binary), direction, row.inside.name(binary)),
    ]
}

/// How a crossing uses what is on the other side, for the HOW column, with
/// "via veneer" after a call through one, or "via PLT" through a PLT stub.
pub(super) fn how_column(binary: &Binary, crossing: &Crossing) -> &'static str {
    text::how_via(binary, crossing.to, crossing.kind, crossing.veneer)
}

/// How a call or jump that elfq can't follow goes, for the HOW column, with
/// "via veneer" after one through a veneer.
pub(crate) fn unfollowed_how(unfollowed: &Unfollowed) -> &'static str {
    match (unfollowed.veneer, unfollowed.unresolved.is_call) {
        (Some(_), true) => "call via veneer",
        (Some(_), false) => "jump via veneer",
        (None, _) => text::call_or_jump(&unfollowed.unresolved),
    }
}

/// The calls and jumps that elfq can't follow, one per function, kind of
/// call, target and veneer, with their sites.
pub(crate) fn group_unfollowed(unfollowed: &[Unfollowed]) -> Vec<(Unfollowed, Vec<u64>)> {
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
/// into a phrase, every range and section with `all`. A load image in a
/// range isn't named, since range checks look only where the program uses
/// its code and data, and a load image is never used where it is stored.
pub(crate) fn ranges_text(binary: &Binary, ranges: &[AddressRange], all: bool) -> String {
    let texts: Vec<String> = ranges
        .iter()
        .map(|&range| text::range_sections_text(binary, range, all))
        .collect();
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    text::names_text(&texts, all)
}
