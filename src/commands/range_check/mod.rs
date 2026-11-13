//! `elfq FILE callees RANGE...` and `elfq FILE callers RANGE...`: what
//! crosses the boundary of some ranges of memory, such as code in ITCM that
//! calls a function in flash. With `--to` or `--from`, only what is in
//! other ranges on the far side. `callees SYMBOL --to RANGE` and `callers
//! SYMBOL --from RANGE` come here too, and ask about the range that SYMBOL
//! occupies, and so do patterns, several SYMBOLs, and SYMBOLs with ranges,
//! which ask about the items they pick, wherever those lie.
//!
//! What a check asks about is [`Inside`] the boundary: the ranges, one
//! SYMBOL, or a set. [`run`] answers in three steps:
//!
//! 1. It works out the items inside once, an [`ItemsInRanges`], which the
//!    next two steps share.
//! 2. `validate.rs` checks the ranges asked about, and those of `--from` or
//!    `--to`, against those items.
//! 3. A [`Boundary`] drawn around them works out what crosses it, and how
//!    `--from` and `--to` narrow that, and `crossing.rs` holds the two
//!    answers: what leaves the inside, for `callees`, and what enters it,
//!    for `callers`.
//!
//! `rows.rs` holds the rows of their tables, which `reach` and `diff` print
//! too.

mod crossing;
mod rows;
mod validate;

pub(super) use crossing::complete;
pub(super) use rows::{
    CalleeJson, CrossingRow, callee_json, callee_rows, crossings_columns, group_unfollowed,
    json_far_side, row_cells, unfollowed_how,
};
pub(super) use validate::{check_holds_items, check_to_range};

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::symbol::{self, LeadName, Set};
use super::text::{self, Detail, Naming};
use super::{Outcome, json};
use crate::binary::{AddressRange, Binary};
use crate::cli::Format;
use crate::graph::{Boundary, Graph, ItemsInRanges, Node};
use crossing::{Entering, Leaving};

/// Which way the references cross the boundary: out of the ranges for
/// `callees`, into them for `callers`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Direction {
    /// Out of the ranges: what the items in them use outside them.
    Out,
    /// Into the ranges: what uses the items in them from outside.
    In,
}

impl Direction {
    /// The command that asks about crossings this way.
    pub(super) fn command(self) -> &'static str {
        match self {
            Direction::Out => "callees",
            Direction::In => "callers",
        }
    }

    /// The option that gives the ranges on the far side.
    pub(super) fn option(self) -> &'static str {
        match self {
            Direction::Out => "--to",
            Direction::In => "--from",
        }
    }

    /// The other way.
    pub(super) fn other(self) -> Direction {
        match self {
            Direction::Out => Direction::In,
            Direction::In => Direction::Out,
        }
    }
}

/// What a range check asks.
pub(super) struct RangeCheck<'a> {
    /// What it asks about: the ranges in place of SYMBOL, SYMBOL, or a set.
    pub(super) inside: Inside,
    /// The ranges of `--from` or `--to`: when there are any, only the uses
    /// whose far end is in them count.
    pub(super) far_side: &'a [AddressRange],
    pub(super) direction: Direction,
}

/// What a range check asks about: the inside of the boundary, whose
/// crossings the answer lists.
pub(super) enum Inside {
    /// Ranges of memory given in place of SYMBOL, which the answer names by
    /// their addresses.
    Ranges(Vec<AddressRange>),
    /// One SYMBOL, given with `--from` or `--to`: the range that its item
    /// occupies, and its name as the answer's first line gives it (see
    /// [`Symbol::lead_name`]).
    ///
    /// [`Symbol::lead_name`]: super::symbol::Symbol::lead_name
    Symbol {
        range: AddressRange,
        lead_name: String,
    },
    /// What patterns, several SYMBOLs, or SYMBOLs with ranges pick
    /// together: their items, wherever they lie, which the answer names as
    /// [`Set::lead_name`] does.
    Set(Set),
}

impl Inside {
    /// The ranges asked about: those given, or the one that SYMBOL's item
    /// occupies, as the answer's JSON lists them. Code in them that is
    /// outside every item, as in a chip's ROM, is inside too. A set has
    /// only the ranges given among its arguments, if any.
    fn ranges(&self) -> &[AddressRange] {
        match self {
            Inside::Ranges(ranges) => ranges,
            Inside::Symbol { range, .. } => std::slice::from_ref(range),
            Inside::Set(set) => set.ranges(),
        }
    }

    /// The items inside, which [`run`] works out once, for the checks and
    /// the boundary.
    fn items<'a>(&'a self, binary: &'a Binary) -> ItemsInRanges<'a> {
        match self {
            // Every item with a byte in the ranges: for SYMBOL, its item,
            // and any other that shares its bytes.
            Inside::Ranges(_) | Inside::Symbol { .. } => ItemsInRanges::new(binary, self.ranges()),
            // Each item picked, wherever it lies, and so every item in the
            // ranges given.
            Inside::Set(set) => {
                let picked = set.items().iter().map(|found| found.index);
                ItemsInRanges::with_items(binary, picked, set.ranges())
            }
        }
    }

    /// How the answer's sentences name what is asked about: SYMBOL, the
    /// ranges with the sections in them, or a set as its first line names
    /// it, every one with [`Detail::All`].
    fn lead_name(&self, binary: &Binary, detail: Detail) -> LeadName {
        match self {
            Inside::Ranges(ranges) => LeadName::name(text::ranges_text(binary, ranges, detail)),
            Inside::Symbol { lead_name, .. } => LeadName::name(lead_name.clone()),
            Inside::Set(set) => set.lead_name(binary, detail),
        }
    }

    /// Whether the sentences call what is asked about "it", and not "them",
    /// in `binary`: one range, one SYMBOL, or a set of one item, unless a
    /// range of code only, which the first line names beside the item, is
    /// among the set's arguments (see [`Set::has_range_of_code_only`]).
    fn one(&self, binary: &Binary) -> bool {
        match self {
            Inside::Ranges(ranges) => ranges.len() == 1,
            Inside::Symbol { .. } => true,
            Inside::Set(set) => set.items().len() == 1 && !set.has_range_of_code_only(binary),
        }
    }

    /// What the sentences call what is asked about after naming it, in
    /// `binary`: "it" or "them" (see [`Inside::one`]).
    fn it(&self, binary: &Binary) -> &'static str {
        if self.one(binary) { "it" } else { "them" }
    }

    /// The notes on how the patterns of a set matched, which come before
    /// the answer's others (see [`symbol::pattern_notes`]), naming every
    /// name with [`Detail::All`]; none without patterns.
    fn pattern_notes(&self, binary: &Binary, detail: Detail) -> Vec<String> {
        match self {
            Inside::Set(set) => match set.patterns() {
                Some(patterns) => symbol::pattern_notes(binary, patterns, &Naming::of(detail)),
                None => Vec::new(),
            },
            Inside::Ranges(_) | Inside::Symbol { .. } => Vec::new(),
        }
    }

    /// What a set picked, as the answer's JSON lists it after the ranges:
    /// its SYMBOLs and its patterns' matches. Neither for ranges, or one
    /// SYMBOL.
    fn picked_json<'a>(&'a self, binary: &'a Binary) -> PickedJson<'a> {
        let Inside::Set(set) = self else {
            return PickedJson {
                symbols: None,
                matches: None,
            };
        };
        let symbols = set.symbols();
        PickedJson {
            symbols: (!symbols.is_empty()).then(|| {
                (symbols.iter())
                    .map(|&index| json::Symbol::new(binary, Node::Item(index)))
                    .collect()
            }),
            matches: set
                .patterns()
                .map(|patterns| json::matches(binary, patterns)),
        }
    }
}

/// What a set given in place of SYMBOL picked, as the JSON of `callers` and
/// `callees` lists it after `ranges`, which has only the ranges given.
#[derive(Serialize)]
struct PickedJson<'a> {
    /// Only when SYMBOLs were given: the items they name, each once, in the
    /// order given.
    #[serde(skip_serializing_if = "Option::is_none")]
    symbols: Option<Vec<json::Symbol<'a>>>,
    /// Only when patterns were given: each item that each pattern matched,
    /// with how, as `retained` lists them.
    #[serde(skip_serializing_if = "Option::is_none")]
    matches: Option<Vec<json::Match<'a>>>,
}

/// Lists what crosses the boundary of what `check` asks about, every
/// crossing, and with [`Detail::All`], every name in its sentences. The
/// answer is [`Outcome::Crossed`] when it lists something, or says that
/// something may cross.
pub(super) fn run(
    binary: &Binary,
    graph: &Graph,
    check: &RangeCheck,
    detail: Detail,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    // The items inside, worked out once for the checks and the boundary.
    let items = check.inside.items(binary);
    validate::check(binary, graph, check, &items)?;
    let boundary = Boundary::around(binary, graph, items);
    let crosses = match check.direction {
        Direction::Out => {
            let answer = Leaving::find(binary, check, &boundary, detail);
            answer.write(format, out)?;
            answer.crosses()
        }
        Direction::In => {
            let answer = Entering::find(binary, graph, check, &boundary, detail);
            answer.write(format, out)?;
            answer.crosses()
        }
    };
    if crosses {
        Ok(Outcome::Crossed)
    } else {
        Ok(Outcome::Answered)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::graph::Unresolved;

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

    /// The text answer to the range check of `ranges`, with `far_side` for
    /// `--from` or `--to`.
    fn answer(
        binary: &Binary,
        graph: &Graph,
        ranges: &[&str],
        far_side: &[&str],
        direction: Direction,
    ) -> (Outcome, String) {
        let ranges: Vec<AddressRange> = ranges.iter().map(|r| r.parse().unwrap()).collect();
        let far_side: Vec<AddressRange> = far_side.iter().map(|r| r.parse().unwrap()).collect();
        let check = RangeCheck {
            inside: Inside::Ranges(ranges),
            far_side: &far_side,
            direction,
        };
        let mut out = Vec::new();
        let outcome = run(binary, graph, &check, Detail::Brief, Format::Text, &mut out).unwrap();
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
