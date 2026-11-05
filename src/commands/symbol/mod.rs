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
//! of memory in place of SYMBOL (see [`argument`]).
//!
//! This file tells ranges from SYMBOL. `lookup.rs` holds `Names`, which
//! finds what SYMBOL names and names items for the user in return,
//! `not_found.rs` the error for SYMBOL that finds several items or none,
//! in the words of every command, which `retained` adds to, and
//! `similar.rs` the names that error suggests. Unit tests build a file of
//! functions with `fixtures.rs`.

#[cfg(test)]
mod fixtures;
mod lookup;
mod not_found;
mod similar;

pub(super) use lookup::{Lookup, Names};
#[cfg(feature = "tui")]
pub(super) use not_found::USE_ONE;
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
    /// One item or import.
    Symbol(Symbol),
    /// Everything in some ranges of memory, together.
    Ranges(Vec<AddressRange>),
}

/// Works out what `queries` ask about: one SYMBOL (see [`find`]), or one or
/// more ranges of memory.
pub(super) fn subject(binary: &Binary, queries: &[String]) -> Result<Subject> {
    if let [query] = queries
        && let Argument::Symbol(query) = argument(query)?
    {
        return Ok(Subject::Symbol(find(binary, query)?));
    }
    let ranges = queries
        .iter()
        .map(|query| match argument(query)? {
            Argument::Range(range) => Ok(range),
            Argument::Symbol(query) => {
                bail!("`{query}` isn't a range; give one SYMBOL, or ranges such as 0x0+256K")
            }
        })
        .collect::<Result<_>>()?;
    Ok(Subject::Ranges(ranges))
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
}
