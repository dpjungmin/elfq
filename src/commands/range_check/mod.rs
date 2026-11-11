//! `elfq FILE callees RANGE...` and `elfq FILE callers RANGE...`: what
//! crosses the boundary of some ranges of memory, such as code in ITCM that
//! calls a function in flash. With `--to` or `--from`, only what is in
//! other ranges on the far side. `callees SYMBOL --to RANGE` and `callers
//! SYMBOL --from RANGE` come here too, and ask about the range that SYMBOL
//! occupies.
//!
//! What a check asks about is [`Inside`] the boundary: the ranges, or one
//! SYMBOL. [`run`] answers in three steps:
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

use super::{Outcome, text};
use crate::binary::{AddressRange, Binary};
use crate::cli::Format;
use crate::graph::{Boundary, Graph, ItemsInRanges};
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
    /// What it asks about: the ranges in place of SYMBOL, or SYMBOL.
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
}

impl Inside {
    /// The ranges asked about: those given, or the one that SYMBOL's item
    /// occupies, as the answer's JSON lists them. Code in them that is
    /// outside every item, as in a chip's ROM, is inside too.
    fn ranges(&self) -> &[AddressRange] {
        match self {
            Inside::Ranges(ranges) => ranges,
            Inside::Symbol { range, .. } => std::slice::from_ref(range),
        }
    }

    /// The items inside, which [`run`] works out once, for the checks and
    /// the boundary.
    fn items<'a>(&'a self, binary: &'a Binary) -> ItemsInRanges<'a> {
        match self {
            // Every item with a byte in the ranges: for SYMBOL, its item,
            // and any other that shares its bytes.
            Inside::Ranges(_) | Inside::Symbol { .. } => ItemsInRanges::new(binary, self.ranges()),
        }
    }

    /// How the answer's sentences name what is asked about: SYMBOL, or the
    /// ranges with the sections in them, every one with `all`.
    fn lead_name(&self, binary: &Binary, all: bool) -> String {
        match self {
            Inside::Ranges(ranges) => text::ranges_text(binary, ranges, all),
            Inside::Symbol { lead_name, .. } => lead_name.clone(),
        }
    }

    /// Whether the sentences call what is asked about "it", and not "them".
    fn one(&self) -> bool {
        match self {
            Inside::Ranges(ranges) => ranges.len() == 1,
            Inside::Symbol { .. } => true,
        }
    }

    /// What the sentences call what is asked about after naming it.
    fn it(&self) -> &'static str {
        if self.one() { "it" } else { "them" }
    }
}

/// Lists what crosses the boundary of what `check` asks about, every
/// crossing, and with `all`, every name in its sentences. The answer is
/// [`Outcome::Crossed`] when it lists something, or says that something
/// may cross.
pub(super) fn run(
    binary: &Binary,
    graph: &Graph,
    check: &RangeCheck,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    // The items inside, worked out once for the checks and the boundary.
    let items = check.inside.items(binary);
    validate::check(binary, graph, check, &items)?;
    let boundary = Boundary::around(binary, graph, items);
    let crosses = match check.direction {
        Direction::Out => {
            let answer = Leaving::find(binary, check, &boundary, all);
            answer.write(format, out)?;
            answer.crosses()
        }
        Direction::In => {
            let answer = Entering::find(binary, graph, check, &boundary, all);
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
