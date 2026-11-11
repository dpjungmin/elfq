//! Working out what the user means by SYMBOL on the command line.
//!
//! SYMBOL can be any of these, tried in order:
//!
//! 1. an address inside an item, such as `0x1700`;
//! 2. the name of an item or an import, or one of an item's aliases, which
//!    include the symbol of a demangled name. An import's name finds its PLT
//!    stub if it has one, since the stub is what code calls;
//! 3. a short name: the end of an item's path, as in `process_frame` for
//!    `app::process_frame`, or `Gain::run` for
//!    `<app::Gain as app::Stage>::run` (see
//!    [`short_names`](crate::binary::names::short_names)).
//!
//! When SYMBOL gives another of an item's names, or an address inside it,
//! the answer's first line says so (see [`Symbol::lead_name`]).
//!
//! For `callers`, `callees` and `retained`, an argument can also be a range
//! of memory in place of SYMBOL (see [`argument`]), or a pattern, such as
//! `uart_*`, for every item it matches: SYMBOL with a `*` that stands for
//! any text in it, when no item or import has it as its name, so that a
//! full name with a `*` in it, as `<*const T as core::fmt::Debug>::fmt`,
//! still finds its item (see [`Names::look_up_or_match`]). They take
//! several of these, and put what they pick together in a [`Set`], each
//! item once; `callers` and `callees` read theirs with [`subject`].
//!
//! The files:
//!
//! - `mod.rs`, this file, tells ranges from SYMBOL, and reads what
//!   `callers` and `callees` ask about.
//! - `lookup.rs` holds [`Names`], which finds what SYMBOL names and names
//!   items for the user in return.
//! - `not_found.rs` words the error for SYMBOL that finds several items or
//!   none, in the words of every command, which `retained` adds to, and
//!   `similar.rs` finds the names that error suggests.
//! - `pattern.rs` holds [`Pattern`], such as `uart_*`, when SYMBOL is one,
//!   and finds the items that one matches ([`Names::matching`]), which
//!   `retained`, `callers` and `callees` take in place of SYMBOL and `debug
//!   matches` lists. Its unit tests are in `pattern/tests.rs`.
//! - `matched.rs` words what patterns matched, for an answer's first line
//!   and its notes.
//! - `set.rs` holds [`Set`], the items that several SYMBOLs, patterns and
//!   ranges pick together, each once, and words how an answer's first line
//!   names them, part by part.
//! - `fixtures.rs` builds the file of functions that unit tests read.

#[cfg(test)]
mod fixtures;
mod lookup;
mod matched;
mod not_found;
mod pattern;
mod set;
mod similar;

pub(super) use lookup::{Lookup, Names};
pub(super) use matched::{PatternMatches, pattern_notes};
#[cfg(feature = "tui")]
pub(super) use not_found::USE_ONE;
pub(super) use pattern::{LookupOrMatch, Match, MatchedBy, Pattern};
pub(super) use set::{Found, LeadName, Pick, Set};
#[cfg(feature = "tui")]
pub(super) use similar::Needle;

use anyhow::{Result, anyhow, bail};

use crate::binary::{AddressRange, Binary, range_hint};
use crate::graph::Node;

/// The item or import that SYMBOL finds.
#[derive(Debug)]
pub struct Symbol {
    pub node: Node,
    /// What the answer's first line adds to the item's name when SYMBOL
    /// gave something other than that name: another of its names, as in
    /// "also named snprintf", or an address inside it, as in "which holds
    /// 0x30000290".
    note: Option<String>,
}

impl Symbol {
    /// The item's name as the answer's first line gives it, with the note
    /// if there is one: `sniprintf (also named snprintf)`.
    pub fn lead_name(&self, binary: &Binary) -> String {
        let name = self.node.name(binary);
        match &self.note {
            Some(note) => format!("{name} ({note})"),
            None => name.to_string(),
        }
    }
}

/// What `callers` and `callees` ask about.
pub(super) enum Subject {
    /// One SYMBOL alone: an item or import, which they answer about as
    /// itself.
    Symbol(Symbol),
    /// Ranges of memory alone: everything in them, together.
    Ranges(Vec<AddressRange>),
    /// Anything else: patterns, several SYMBOLs, or SYMBOLs with ranges.
    /// They answer about the items picked together, as about ranges, even
    /// for a pattern that matches one item, so that the answer's shape
    /// doesn't change when a build adds or drops a match.
    Set(Set),
}

/// How `callers` or `callees` takes the item of a SYMBOL among others into
/// a set: its index, or the command's error for an import, whose code
/// isn't in the file.
pub(super) type ItemInSet = fn(&Binary, Node) -> Result<usize>;

/// Works out what `queries` ask about, reading each as [`argument`] does
/// and in the order given: one SYMBOL alone (see [`Names::find`]), ranges
/// of memory alone, or else the [`Set`] of the items they pick. Stops at
/// the first that it can't read, such as a range it can't parse or a name
/// or pattern that finds nothing, with its error; whether each range holds
/// something is checked after, by `range_check`. A set holds only items,
/// so `item_in_set` gives the item of a SYMBOL among others, or the
/// command's error for an import.
pub(super) fn subject(
    binary: &Binary,
    queries: &[String],
    item_in_set: ItemInSet,
) -> Result<Subject> {
    let names = Names::new(binary);
    let mut picks = Vec::new();
    for query in queries {
        let pick = match argument(query)? {
            Argument::Range(range) => Pick::Range(range),
            Argument::Symbol(query) => match names.look_up_or_match(query)? {
                LookupOrMatch::Name(lookup) => {
                    let symbol = names.one_or_error(query, lookup)?;
                    if let [_] = queries {
                        return Ok(Subject::Symbol(symbol));
                    }
                    Pick::Symbol {
                        index: item_in_set(binary, symbol.node)?,
                        lead_name: symbol.lead_name(binary),
                    }
                }
                LookupOrMatch::Pattern(matched) => Pick::Pattern(matched),
                LookupOrMatch::NoMatch(pattern) => bail!("{}", names.nothing_matches(&pattern)),
            },
        };
        picks.push(pick);
    }
    // Some only when every argument is a range: then they are ranges alone,
    // not a set.
    let ranges: Option<Vec<AddressRange>> = (picks.iter())
        .map(|pick| match pick {
            Pick::Range(range) => Some(*range),
            Pick::Symbol { .. } | Pick::Pattern(_) => None,
        })
        .collect();
    Ok(match ranges {
        Some(ranges) => Subject::Ranges(ranges),
        None => Subject::Set(Set::new(binary, picks)),
    })
}

/// What an argument given in place of SYMBOL is.
pub(super) enum Argument<'q> {
    /// SYMBOL, as typed, for [`Names`] to look up.
    Symbol(&'q str),
    /// A range of memory.
    Range(AddressRange),
}

/// Reads `query`, given in place of SYMBOL, as `callers`, `callees` and
/// `retained` read it: as a range when it is meant as one (see
/// [`is_range`]), or else as SYMBOL. Fails only for a range: one that it
/// can't read, or one typed with `-` or `:` between its ends, which can't
/// be SYMBOL either.
pub(super) fn argument(query: &str) -> Result<Argument<'_>> {
    match range(query) {
        Some(range) => Ok(Argument::Range(range?)),
        None => {
            check_not_a_range(query)?;
            Ok(Argument::Symbol(query))
        }
    }
}

/// The range that `query` gives, if it gives one (see [`is_range`]):
/// `START..END` or `START+SIZE`, as `--range` takes it. Ranges joined by
/// commas are an error that says to give each on its own.
fn range(query: &str) -> Option<Result<AddressRange>> {
    is_range(query).then(|| {
        if !query.contains(',') {
            return query
                .parse()
                .map_err(|error| anyhow!("`{query}` isn't a range: {error}"));
        }
        let parts: Vec<&str> = query
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect();
        for part in &parts {
            if let Err(error) = part.parse::<AddressRange>() {
                bail!("`{part}` isn't a range: {error}");
            }
        }
        match parts[..] {
            [_] => bail!("`{query}` isn't a range; leave out the comma"),
            _ => bail!(
                "`{query}` isn't a range; give each range as its own argument: {}",
                parts.join(" ")
            ),
        }
    })
}

/// Refuses `query` when it is a range typed with `-` or `:` between its
/// ends, as in `0x0-0x40000`, which neither a range nor SYMBOL can be:
/// no name starts with a digit, and no address has `-` or `:` in it.
fn check_not_a_range(query: &str) -> Result<()> {
    if query.starts_with(|c: char| c.is_ascii_digit()) && query.contains(['-', ':']) {
        bail!("`{query}` isn't a range: {}", range_hint(query));
    }
    Ok(())
}

/// Whether `query` is meant as a range of memory rather than a symbol: it
/// starts with a digit, as no name does, and has `..` or `+` in it.
pub(super) fn is_range(query: &str) -> bool {
    query.starts_with(|c: char| c.is_ascii_digit()) && (query.contains("..") || query.contains('+'))
}

/// Finds the item or import that `query` names, as `Names::find` does.
/// Use it when the answer names nothing else; when it does, as in
/// `retained` and `why`, keep one `Names` for the lookup and the naming,
/// so that the names are worked out once. Here they are freed before the
/// answer, so that what the command builds next can take their memory.
pub fn find(binary: &Binary, query: &str) -> Result<Symbol> {
    Names::new(binary).find(query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_that_starts_with_a_digit_can_be_a_range() {
        let range = |query| range(query).map(|range| range.map_err(|error| error.to_string()));
        let itcm = AddressRange {
            start: 0,
            end: 0x40000,
        };
        assert_eq!(range("0x0+256K"), Some(Ok(itcm)));
        assert_eq!(range("0..0x40000"), Some(Ok(itcm)));
        assert_eq!(
            range("0x40000..0x0"),
            Some(Err(
                "`0x40000..0x0` isn't a range: the range must end after it starts".to_string()
            ))
        );
        // An address, and names, even with `+` in them.
        assert_eq!(range("0x1700"), None);
        assert_eq!(range("operator+"), None);
        assert_eq!(range("Vec<T>::operator+="), None);
    }

    #[test]
    fn one_symbol_alone_is_itself_and_anything_else_but_ranges_is_a_set() {
        // The item of a SYMBOL among others; an import would be an error.
        fn item_in_set(_: &Binary, node: Node) -> Result<usize> {
            match node {
                Node::Item(index) => Ok(index),
                Node::Import(_) => bail!("an import"),
            }
        }
        let binary = fixtures::functions(&["main", "uart_putc", "uart_getc"]);
        let subject = |queries: &[&str]| {
            let queries: Vec<String> = queries.iter().map(|query| query.to_string()).collect();
            subject(&binary, &queries, item_in_set)
        };
        let set_names = |queries: &[&str]| match subject(queries) {
            Ok(Subject::Set(set)) => (set.items().iter())
                .map(|found| found.lead_name.clone())
                .collect::<Vec<String>>(),
            _ => panic!("{queries:?} isn't a set"),
        };
        assert!(matches!(
            subject(&["uart_putc"]),
            Ok(Subject::Symbol(Symbol {
                node: Node::Item(1),
                ..
            }))
        ));
        assert!(matches!(
            subject(&["0x1000+0x10", "0x1020+0x10"]),
            Ok(Subject::Ranges(ranges)) if ranges.len() == 2
        ));
        // A pattern is a set, even for one match.
        assert_eq!(set_names(&["uart_*"]), ["uart_putc", "uart_getc"]);
        assert_eq!(set_names(&["uart_p*"]), ["uart_putc"]);
        // And so is a SYMBOL given twice, or with a range.
        assert_eq!(set_names(&["main", "main"]), ["main"]);
        assert_eq!(set_names(&["0x1020+0x10", "main"]), ["uart_getc", "main"]);
        // It stops at the first argument that it can't read.
        let error = |queries: &[&str]| match subject(queries) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("{queries:?} isn't an error"),
        };
        assert_eq!(
            error(&["nosuch_*", "0x100..0x0"]),
            "no function or data object is named `nosuch_*`"
        );
        assert_eq!(
            error(&["0x100..0x0", "nosuch_*"]),
            "`0x100..0x0` isn't a range: the range must end after it starts"
        );
    }
}
