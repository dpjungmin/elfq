//! `elfq FILE callees RANGE...` and `elfq FILE callers RANGE...`: what
//! crosses the boundary of some ranges of memory, such as code in ITCM that
//! calls a function in flash. With `--to` or `--from`, only what is in
//! other ranges on the far side. `callees SYMBOL --to RANGE` and `callers
//! SYMBOL --from RANGE` come here too, and ask about the range that SYMBOL
//! occupies.

use std::collections::HashMap;
use std::io::Write;

use anyhow::{Result, bail};
use serde::Serialize;

use super::{Outcome, json, text};
use crate::binary::{AddressRange, Binary, Source};
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

/// What a range check asks.
pub struct RangeCheck<'a> {
    /// The ranges in place of SYMBOL, or the range that SYMBOL occupies.
    pub ranges: &'a [AddressRange],
    /// SYMBOL, as the answer names it, when `ranges` is its range.
    pub name: Option<String>,
    /// The ranges of `--from` or `--to`: when there are any, only the uses
    /// whose other end is in them count.
    pub other: &'a [AddressRange],
    pub direction: Direction,
}

impl RangeCheck<'_> {
    /// What the sentences call the ranges: SYMBOL, or the ranges with the
    /// sections in them.
    fn subject(&self, binary: &Binary, all: bool) -> String {
        match &self.name {
            Some(name) => name.clone(),
            None => ranges_text(binary, self.ranges, all),
        }
    }

    /// Whether the sentences call the ranges "it", and not "them".
    fn one(&self) -> bool {
        self.name.is_some() || self.ranges.len() == 1
    }

    /// What the sentences call the ranges after naming them.
    fn it(&self) -> &'static str {
        if self.one() { "it" } else { "them" }
    }

    /// The option that gives `other`.
    fn option(&self) -> &'static str {
        match self.direction {
            Direction::Out => "--to",
            Direction::In => "--from",
        }
    }
}

/// Lists what crosses the boundary of the ranges that `check` asks about,
/// every crossing, and with `all`, every name in its sentences. The answer
/// is [`Outcome::Crossed`] when it lists something, or says that something
/// may cross.
pub fn run(
    binary: &Binary,
    graph: &Graph,
    check: &RangeCheck,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    // Where calls and jumps in the file go to code outside every item, as
    // to a routine in a chip's ROM, which `callers` and `--to` can name.
    let code_outside: Vec<u64> = graph
        .unresolved
        .iter()
        .filter_map(|unresolved| unresolved.target)
        .collect();
    for &range in check.ranges {
        if check.direction == Direction::In && has_address(&code_outside, range) {
            continue;
        }
        check_holds_items(binary, range)?;
    }
    check_other_side(binary, check, &code_outside)?;
    let boundary = Boundary::new(binary, graph, check.ranges);
    let nothing_crosses = match check.direction {
        Direction::Out => {
            let (crossings, unfollowed) = leaving_to(binary, graph, &boundary, check);
            let leaving = Leaving {
                binary,
                check,
                crossings,
                unfollowed,
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
            let from = check.other;
            // Code in the `--from` ranges that elfq can't read may use the
            // ranges in any way.
            let unreadable = if from.is_empty() {
                Vec::new()
            } else {
                Boundary::new(binary, graph, from)
                    .unreadable()
                    .into_iter()
                    .filter(|&index| !binary.items[index].is_in(check.ranges))
                    .collect()
            };
            // Code anywhere may call through a pointer passed to it, but
            // with `--from`, only code in its ranges counts.
            let through_pointer = if from.is_empty()
                || !unreadable.is_empty()
                || calls_through_pointer(binary, graph, from, check.ranges)
            {
                boundary.callable_through_pointer()
            } else {
                Vec::new()
            };
            let (crossings, unfollowed) = entering_from(binary, graph, &boundary, check);
            let entering = Entering {
                binary,
                check,
                crossings,
                unfollowed,
                through_pointer,
                unreadable,
                all,
            };
            match format {
                Format::Json => entering.write_json(out)?,
                Format::Text | Format::Tree => entering.write_text(out)?,
            }
            entering.crossings.is_empty()
                && entering.unfollowed.is_empty()
                && entering.through_pointer.is_empty()
                && entering.unreadable.is_empty()
        }
    };
    if nothing_crosses {
        Ok(Outcome::Answered)
    } else {
        Ok(Outcome::Crossed)
    }
}

/// The range that an item occupies, for `--from` and `--to` with SYMBOL. An
/// item of unknown size counts as its first byte, as it does in a range
/// check.
pub(super) fn item_range(binary: &Binary, index: usize) -> AddressRange {
    let item = &binary.items[index];
    AddressRange {
        start: item.address,
        end: item.address + item.size.max(1),
    }
}

/// What the items in the ranges use outside them, with the calls and jumps
/// in them that elfq can't follow, and that may leave them. With `--to`,
/// only what is in its ranges, and what elfq can't follow that may go
/// there: to an address in them, or through a pointer when a function
/// there, outside the ranges, has its address taken or stored.
fn leaving_to(
    binary: &Binary,
    graph: &Graph,
    boundary: &Boundary,
    check: &RangeCheck,
) -> (Vec<Crossing>, Vec<Unfollowed>) {
    let to = check.other;
    if to.is_empty() {
        return (boundary.leaving(), boundary.unfollowed());
    }
    // A veneer in `to` that a use goes through to something outside `to`:
    // the use goes into `to` at the veneer, so it is listed as a use of the
    // veneer.
    let veneer_in_to = |veneer: Option<usize>| {
        veneer.filter(|&veneer| {
            let item = &binary.items[veneer];
            item.is_in(to) && !item.is_in(check.ranges)
        })
    };
    let mut crossings = Vec::new();
    for crossing in boundary.leaving() {
        if node_in(binary, crossing.to, to) {
            crossings.push(crossing);
        } else if let Some(veneer) = veneer_in_to(crossing.veneer) {
            crossings.push(Crossing {
                to: Node::Item(veneer),
                veneer: None,
                ..crossing
            });
        }
    }
    let through_pointer = Boundary::new(binary, graph, to)
        .callable_through_pointer()
        .into_iter()
        .any(|function| !binary.items[function].is_in(check.ranges));
    let mut unfollowed = Vec::new();
    for row in boundary.unfollowed() {
        let unresolved = row.unresolved;
        let may_go_to = match unresolved.target {
            Some(target) => contains(to, target),
            None => through_pointer,
        };
        if may_go_to {
            unfollowed.push(row);
        } else if let Some(veneer) = veneer_in_to(row.veneer) {
            crossings.push(Crossing {
                from: unresolved.from,
                to: Node::Item(veneer),
                kind: if unresolved.is_call {
                    ReferenceKind::Call
                } else {
                    ReferenceKind::TailCall
                },
                site: unresolved.site,
                veneer: None,
            });
        }
    }
    (crossings, unfollowed)
}

/// What uses the items in the ranges from outside them, with the calls and
/// jumps from outside into code in them that is outside every item, as in a
/// chip's ROM. With `--from`, only what is in its ranges.
fn entering_from(
    binary: &Binary,
    graph: &Graph,
    boundary: &Boundary,
    check: &RangeCheck,
) -> (Vec<Crossing>, Vec<Unfollowed>) {
    let from = check.other;
    if from.is_empty() {
        return (boundary.entering(), boundary.unfollowed_entering());
    }
    let in_from = |index: usize| binary.items[index].is_in(from);
    let (mut crossings, others): (Vec<Crossing>, Vec<Crossing>) = boundary
        .entering()
        .into_iter()
        .partition(|crossing| in_from(crossing.from));
    let (mut unfollowed, other_rows): (Vec<Unfollowed>, Vec<Unfollowed>) = boundary
        .unfollowed_entering()
        .into_iter()
        .partition(|row| in_from(row.unresolved.from));
    // A call through a veneer in `from` whose caller is outside `from`: the
    // veneer's jump is what uses the ranges from `from`, unless a caller in
    // `from` already has a row for it.
    let veneer_in_from = |veneer: usize, kept: &[Option<usize>]| {
        let item = &binary.items[veneer];
        item.is_in(from) && !item.is_in(check.ranges) && !kept.contains(&Some(veneer))
    };
    let kept: Vec<Option<usize>> = crossings.iter().map(|crossing| crossing.veneer).collect();
    let mut jumps: Vec<Crossing> = others
        .iter()
        .filter(|crossing| {
            crossing
                .veneer
                .is_some_and(|veneer| veneer_in_from(veneer, &kept))
        })
        .filter_map(|crossing| {
            let veneer = crossing.veneer?;
            // A PLT stub has a jump to its header too.
            let jump = graph
                .references_from(veneer)
                .iter()
                .find(|jump| jump.to == crossing.to)?;
            Some(Crossing {
                from: veneer,
                to: jump.to,
                kind: jump.kind,
                site: jump.site,
                veneer: None,
            })
        })
        .collect();
    jumps.sort_by_key(|jump| (jump.from, jump.site));
    jumps.dedup();
    crossings.extend(jumps);
    let kept: Vec<Option<usize>> = unfollowed.iter().map(|row| row.veneer).collect();
    let mut jumps: Vec<Unfollowed> = other_rows
        .iter()
        .filter(|row| {
            row.veneer
                .is_some_and(|veneer| veneer_in_from(veneer, &kept))
        })
        .filter_map(|row| {
            let veneer = row.veneer?;
            let jump = graph
                .unresolved_in(veneer)
                .iter()
                .find(|jump| jump.target == row.unresolved.target)?;
            Some(Unfollowed {
                unresolved: *jump,
                veneer: None,
            })
        })
        .collect();
    jumps.sort_by_key(|jump| (jump.unresolved.from, jump.unresolved.site));
    jumps.dedup();
    unfollowed.extend(jumps);
    unfollowed.sort_by_key(|row| (row.unresolved.from, row.unresolved.site));
    (crossings, unfollowed)
}

/// Whether code in `from`, outside `ranges`, calls or jumps through a
/// pointer.
fn calls_through_pointer(
    binary: &Binary,
    graph: &Graph,
    from: &[AddressRange],
    ranges: &[AddressRange],
) -> bool {
    graph.unresolved.iter().any(|unresolved| {
        let item = &binary.items[unresolved.from];
        unresolved.is_indirect() && item.is_in(from) && !item.is_in(ranges)
    })
}

/// Whether `node` is an item in `ranges`. An import is in none.
fn node_in(binary: &Binary, node: Node, ranges: &[AddressRange]) -> bool {
    match node {
        Node::Item(index) => binary.items[index].is_in(ranges),
        Node::Import(_) => false,
    }
}

/// Whether `address` is in one of `ranges`.
fn contains(ranges: &[AddressRange], address: u64) -> bool {
    ranges.iter().any(|range| range.contains(address))
}

/// Whether one of `addresses` is in `range`.
fn has_address(addresses: &[u64], range: AddressRange) -> bool {
    addresses.iter().any(|&address| range.contains(address))
}

/// Explains that no item is in `range`, as for a mistyped range, unless
/// one is.
fn check_holds_items(binary: &Binary, range: AddressRange) -> Result<()> {
    if binary.items.iter().any(|item| item.is_in(&[range])) {
        return Ok(());
    }
    let place = super::empty_place(binary, Some(range))?;
    // Where the program uses what is stored in the range.
    let used_at: Vec<String> = text::load_images_in(binary, range)
        .into_iter()
        .map(|section| {
            let used_at = AddressRange {
                start: section.address,
                end: section.address + section.size,
            };
            format!("{} at {}", section.name, text::range_argument(used_at))
        })
        .collect();
    if !used_at.is_empty() {
        let used_at: Vec<&str> = used_at.iter().map(String::as_str).collect();
        bail!(
            "elfq found no functions or data in {place}; the program uses {}",
            text::names_text(&used_at, false)
        );
    }
    bail!("elfq found no functions or data in {place}");
}

/// Checks the ranges of `--from` or `--to`. Each must hold an item outside
/// the ranges that `check` asks about, since the other end of each use is
/// outside them, or for `--to`, code outside every item and outside those
/// ranges that the file calls or jumps to, at one of `code_outside`, as in
/// a chip's ROM. Else it is most likely mistyped. And when the ranges hold
/// something outside them, nothing may be in both, since each end of a use
/// would then be on either side.
fn check_other_side(binary: &Binary, check: &RangeCheck, code_outside: &[u64]) -> Result<()> {
    let option = check.option();
    let it = check.it();
    let lists = match check.direction {
        Direction::Out => format!(
            "callees lists only what {} outside {it}",
            if check.one() { "it uses" } else { "they use" }
        ),
        Direction::In => format!("callers lists only what uses {it} from outside"),
    };
    let inside = || match &check.name {
        Some(name) => name.clone(),
        None => {
            let texts: Vec<String> = check
                .ranges
                .iter()
                .map(|&range| text::range_argument(range))
                .collect();
            let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
            text::names_text(&texts, true)
        }
    };
    for &range in check.other {
        // Code that the file calls in a `--to` range, as in a ROM, is all
        // that the range need hold.
        let called = check.direction == Direction::Out && has_address(code_outside, range);
        if !called {
            if check.direction == Direction::Out
                && text::sections_in(binary, range).is_empty()
                && text::load_images_in(binary, range).is_empty()
            {
                bail!(
                    "--to {} holds no functions or data, and no code in the file calls or \
                     jumps there",
                    text::range_argument(range)
                );
            }
            check_holds_items(binary, range)?;
        }
        let item_outside = binary
            .items
            .iter()
            .any(|item| item.is_in(&[range]) && !item.is_in(check.ranges));
        let code_outside_them = called
            && code_outside
                .iter()
                .any(|&address| range.contains(address) && !contains(check.ranges, address));
        if !item_outside && !code_outside_them {
            bail!(
                "{option} {} holds nothing outside {}, and {lists}",
                text::range_argument(range),
                inside()
            );
        }
    }
    // What the ranges hold, items and code outside every item that the file
    // calls, each with the range of the option that holds it too, if any.
    let items = binary
        .items
        .iter()
        .filter(|item| item.is_in(check.ranges))
        .map(|item| (Some(item), item.address));
    let code = code_outside
        .iter()
        .filter(|&&address| contains(check.ranges, address))
        .map(|&address| (None, address));
    let mut shared = None;
    let mut outside = false;
    for (item, address) in items.chain(code) {
        let other = check.other.iter().copied().find(|&range| match item {
            Some(item) => item.is_in(&[range]),
            None => range.contains(address),
        });
        match other {
            Some(range) => {
                shared.get_or_insert((item, address, range));
            }
            None => outside = true,
        }
    }
    if outside && let Some((item, address, other)) = shared {
        let (what, range) = match item {
            Some(item) => (
                item.name.clone(),
                check.ranges.iter().find(|&&range| item.is_in(&[range])),
            ),
            None => (
                format!("the code at {address:#x}"),
                check.ranges.iter().find(|range| range.contains(address)),
            ),
        };
        let range = range.map_or_else(inside, |&range| text::range_argument(range));
        bail!(
            "{option} {} and {range} both hold {what}; give ranges that don't share it",
            text::range_argument(other)
        );
    }
    Ok(())
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

/// The ranges of `--to` or `--from` for JSON, or `None` when there are none.
fn json_other(other: &[AddressRange]) -> Option<Vec<json::Range>> {
    (!other.is_empty()).then(|| json_ranges(other))
}

/// What the items in the ranges use outside them: `callees RANGE...`, or
/// `callees SYMBOL --to RANGE`.
struct Leaving<'a> {
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

impl Leaving<'_> {
    fn write_json(&self, out: &mut dyn Write) -> Result<()> {
        let binary = self.binary;
        let item = |node: Node| json::Item::new(binary, node);
        let groups = groups(binary, &self.crossings, Direction::Out, |crossing| {
            json::how_field(crossing.kind)
        });
        let leaving = LeavingJson {
            ranges: json_ranges(self.check.ranges),
            to: json_other(self.check.other),
            callees: each_row(groups)
                .map(|(outside, row)| CalleeJson {
                    item: item(outside),
                    caller: item(row.inside),
                    how: row.how,
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

    fn write_text(&self, out: &mut dyn Write) -> Result<()> {
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
                unreadable_text(binary, &self.unreadable, self.all)
            )?;
        }
        Ok(())
    }
}

/// What uses the items in the ranges from outside them: `callers
/// RANGE...`, or `callers SYMBOL --from RANGE`.
struct Entering<'a> {
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

impl Entering<'_> {
    fn write_json(&self, out: &mut dyn Write) -> Result<()> {
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
                    how: row.how,
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

    fn write_text(&self, out: &mut dyn Write) -> Result<()> {
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
            for row in &self.unfollowed {
                let caller = Node::Item(row.unresolved.from);
                if !callers.contains(&caller) {
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
                unreadable_text(binary, &self.unreadable, self.all)
            )?;
        }
        Ok(())
    }
}

/// The sentence that says elfq can't read `unreadable`, the functions whose
/// end it doesn't know, as `callees SYMBOL` says it.
fn unreadable_text(binary: &Binary, unreadable: &[usize], all: bool) -> String {
    let names: Vec<&str> = unreadable
        .iter()
        .map(|&index| binary.items[index].name.as_str())
        .collect();
    format!(
        "elfq doesn't know where {} {}, so it can't read {} code.",
        text::names_text(&names, all),
        if names.len() == 1 { "ends" } else { "end" },
        if names.len() == 1 { "its" } else { "their" }
    )
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
/// "via veneer" after a call through one, or "via PLT" through a PLT stub.
fn how_column(binary: &Binary, crossing: &Crossing) -> &'static str {
    let plt = crossing
        .veneer
        .is_some_and(|veneer| binary.items[veneer].source == Source::Plt);
    match (crossing.kind, crossing.veneer, plt) {
        (ReferenceKind::Call, Some(_), false) => "call via veneer",
        (ReferenceKind::Call, Some(_), true) => "call via PLT",
        (ReferenceKind::TailCall, Some(_), false) => "tail call via veneer",
        (ReferenceKind::TailCall, Some(_), true) => "tail call via PLT",
        (kind, _, _) => text::how_column(binary, crossing.to, kind),
    }
}

/// How a call or jump that elfq can't follow goes, for the HOW column, with
/// "via veneer" after one through a veneer.
fn unfollowed_how(unfollowed: &Unfollowed) -> &'static str {
    match (unfollowed.veneer, unfollowed.unresolved.is_call) {
        (Some(_), true) => "call via veneer",
        (Some(_), false) => "jump via veneer",
        (None, _) => text::call_or_jump(&unfollowed.unresolved),
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
/// into a phrase, every range and section with `all`. A load image in a
/// range isn't named, since range checks look only where the program uses
/// its code and data, and a load image is never used where it is stored.
fn ranges_text(binary: &Binary, ranges: &[AddressRange], all: bool) -> String {
    let texts: Vec<String> = ranges
        .iter()
        .map(|&range| text::range_sections_text(binary, range, all))
        .collect();
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    text::names_text(&texts, all)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::graph::{Reference, Unresolved};

    /// The Cortex-M7 sample, with code in ITCM at 0x0 and flash at
    /// 0x30000000, and the veneers GNU ld added between them.
    fn sample() -> (Binary, Graph) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-cortex-m7.elf");
        let binary = Binary::load(&path).unwrap();
        let graph = Graph::build(&binary);
        (binary, graph)
    }

    fn index(binary: &Binary, name: &str) -> usize {
        binary
            .items
            .iter()
            .position(|item| item.name == name)
            .unwrap()
    }

    /// The text answer to the range check of `ranges`, with `other` for
    /// `--from` or `--to`.
    fn answer(
        binary: &Binary,
        graph: &Graph,
        ranges: &[&str],
        other: &[&str],
        direction: Direction,
    ) -> (Outcome, String) {
        let ranges: Vec<AddressRange> = ranges.iter().map(|r| r.parse().unwrap()).collect();
        let other: Vec<AddressRange> = other.iter().map(|r| r.parse().unwrap()).collect();
        let check = RangeCheck {
            ranges: &ranges,
            name: None,
            other: &other,
            direction,
        };
        let mut out = Vec::new();
        let outcome = run(binary, graph, &check, false, Format::Text, &mut out).unwrap();
        (outcome, String::from_utf8(out).unwrap())
    }

    #[test]
    fn a_call_to_code_outside_any_function_has_a_row() {
        // As when code in ITCM calls a routine in a microcontroller's ROM,
        // which the file has no symbol for.
        let (binary, mut graph) = sample();
        graph.unresolved.push(Unresolved {
            from: index(&binary, "crc32"),
            site: 0x20,
            is_call: true,
            target: Some(0x1fff_0100),
        });
        let (outcome, out) = answer(&binary, &graph, &["0x0+256K"], &[], Direction::Out);
        assert_eq!(outcome, Outcome::Crossed);
        assert!(out.contains("\n0x1fff0100        call  "), "{out}");
        assert!(
            out.contains("  0x20  (outside any function), used by crc32\n"),
            "{out}"
        );
        // callers lists it from the other side.
        let (outcome, out) = answer(
            &binary,
            &graph,
            &["0x1fff0000+64K"],
            &["0x0+256K"],
            Direction::In,
        );
        assert_eq!(outcome, Outcome::Crossed);
        assert!(
            out.ends_with("  0x20  crc32, uses 0x1fff0100 (outside any function)\n"),
            "{out}"
        );
    }

    #[test]
    fn both_sides_agree_on_a_veneer_with_callers_on_both() {
        // on_timer, outside the flash range below, calls crc32 through the
        // veneer in it too, as process_frame does.
        let (binary, mut graph) = sample();
        let on_timer = index(&binary, "on_timer");
        graph.references.push(Reference {
            from: on_timer,
            to: Node::Item(index(&binary, "__crc32_veneer")),
            kind: ReferenceKind::Call,
            site: binary.items[on_timer].address + 4,
        });
        graph.references.sort_by_key(|r| (r.from, r.site));
        let flash = ["0x30000320..0x300009c0"];
        let itcm = ["0x0+256K"];
        let (outcome, callers) = answer(&binary, &graph, &itcm, &flash, Direction::In);
        assert_eq!(outcome, Outcome::Crossed);
        let (outcome, callees) = answer(&binary, &graph, &flash, &itcm, Direction::Out);
        assert_eq!(outcome, Outcome::Crossed);
        // process_frame's calls, and not the veneer's jump, which a caller
        // in the flash range already stands for: the AT column of each row.
        let sites = |out: &str| -> Vec<String> {
            out.lines()
                .skip(3)
                .map(|line| {
                    let cells: Vec<&str> =
                        line.split("  ").filter(|cell| !cell.is_empty()).collect();
                    cells[3].trim().to_string()
                })
                .collect()
        };
        assert_eq!(
            sites(&callers),
            ["0x3000034c", "0x30000386, 0x30000454"],
            "{callers}"
        );
        assert!(!callers.contains("__crc32_veneer"), "{callers}");
        let mut callees_sites = sites(&callees);
        callees_sites.sort();
        assert_eq!(callees_sites, sites(&callers), "{callees}");
    }

    #[test]
    fn only_a_call_through_a_pointer_may_call_through_a_pointer() {
        let (binary, mut graph) = sample();
        // crc32, in ITCM, calls the ROM, but through no pointer.
        graph.unresolved.push(Unresolved {
            from: index(&binary, "crc32"),
            site: 0x20,
            is_call: true,
            target: Some(0x1fff_0100),
        });
        graph.unresolved.sort_by_key(|u| (u.from, u.site));
        let (outcome, out) = answer(
            &binary,
            &graph,
            &["0x30000000+16M"],
            &["0x10+48"],
            Direction::In,
        );
        assert_eq!(outcome, Outcome::Answered, "{out}");
        // Code in flash calls through a pointer, but no pointer goes to the
        // ROM, which holds no function, and both sides say so.
        let (outcome, out) = answer(
            &binary,
            &graph,
            &["0x30000000+16M"],
            &["0x1fff0000+64K"],
            Direction::Out,
        );
        assert_eq!(outcome, Outcome::Answered, "{out}");
        let (outcome, out) = answer(
            &binary,
            &graph,
            &["0x1fff0000+64K"],
            &["0x30000000+16M"],
            Direction::In,
        );
        assert_eq!(outcome, Outcome::Answered, "{out}");
        // But not when that code is in the ranges asked about too: a call
        // there doesn't leave them.
        let crc32 = index(&binary, "crc32");
        graph.unresolved.push(Unresolved {
            from: crc32,
            site: 0x24,
            is_call: true,
            target: None,
        });
        graph.unresolved.sort_by_key(|u| (u.from, u.site));
        let (_, out) = answer(
            &binary,
            &graph,
            &["0x0..0x20000000"],
            &["0x0..0x20000010"],
            Direction::Out,
        );
        assert!(!out.contains("(through a pointer)"), "{out}");
    }
}
