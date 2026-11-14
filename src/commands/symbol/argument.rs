//! Reading one argument given in place of SYMBOL, as the commands that
//! take a set read it: `retained`, `callers` and `callees`.
//!
//! [`Names::read_argument`] reads an argument as a range of memory when it
//! reads as one (see [`is_range`]), or else as SYMBOL: a name, an address
//! or a pattern. It gives what the argument finds, an [`Argument`], or why
//! the command can't use it, an [`Unusable`], which isn't worded until
//! [`Names::unusable_error`] words it, since that can take a while.
//! [`Pick::new`] then gives what the argument picks for a
//! [`Set`](super::Set), with the command's rule for the item of a SYMBOL
//! among others, which may be an import ([`ItemInSet`]).
//!
//! To stop at the first argument that it can't use, a new command that
//! takes a set calls [`subject`](super::subject) with its [`ItemInSet`], as
//! `callers` and `callees` do; `subject` also answers about one SYMBOL
//! alone as itself, since it may be an import, which no set holds, and
//! keeps ranges alone apart from a set. To list every argument that it
//! can't use instead, it reads each one as `retained::find_symbols` does; a
//! second command that lists them moves `Refused` and `refused_error`, with
//! what they use, here rather than copying them. Neither `read_argument`
//! nor `Pick::new` checks that a range holds an item: `retained` checks it
//! with `check_holds_items` as it reads each one, and `callers` and
//! `callees` check it once the graph is built (`range_check/validate.rs`).

use anyhow::{Result, anyhow, bail};

use super::{
    ItemInSet, Lookup, LookupOrMatch, Names, Pattern, PatternMatches, Pick, Symbol, Takes,
};
use crate::binary::{AddressRange, Binary, range_hint};
use crate::graph::Node;

/// What an argument given in place of SYMBOL finds, as
/// [`Names::read_argument`] reads it.
pub(in crate::commands) enum Argument {
    /// The item or import that SYMBOL finds, as a name or an address.
    Symbol(Symbol),
    /// A pattern, with the items that it matches, one or more.
    Pattern(PatternMatches),
    /// A range of memory.
    Range(AddressRange),
}

/// Why a command that takes a set can't use a SYMBOL, or a pattern or
/// range in its place. It is worded only when the error is built (see
/// [`Names::unusable_error`]), since the error for a name that finds
/// nothing compares it with every name in the file, and `retained` words
/// only the arguments that its error lists.
pub(in crate::commands) enum Unusable<'n> {
    /// No item or import has the name.
    NoName,
    /// The pattern matches no item. It is boxed, since a pattern takes far
    /// more room than any other reason.
    NoMatch(Box<Pattern>),
    /// Several items or imports have the name.
    Several(&'n [Node]),
    /// The error that every command gives it, as elfq prints it: an
    /// address that no item holds, or that isn't one, an import, whose
    /// code isn't in the file, or a range that holds no item, or that isn't
    /// one.
    Error(String),
}

impl From<anyhow::Error> for Unusable<'_> {
    /// The error that every command gives, as elfq prints it.
    fn from(error: anyhow::Error) -> Self {
        Unusable::Error(format!("{error:#}"))
    }
}

impl Names<'_> {
    /// What `query`, given in place of SYMBOL, finds: a range of memory
    /// when it reads as one (see [`is_range`]), or else what it finds as
    /// SYMBOL, a name or an address, or a pattern (see
    /// [`Names::look_up_or_match`]); or why a command that takes a set
    /// can't use it. A range that it can't read, or one typed with `-` or
    /// `:` between its ends, which can't be SYMBOL either, gets the error
    /// that says why.
    pub(in crate::commands) fn read_argument(&self, query: &str) -> Result<Argument, Unusable<'_>> {
        if let Some(range) = range(query) {
            return Ok(Argument::Range(range?));
        }
        check_not_a_range(query)?;
        Ok(match self.look_up_or_match(query)? {
            LookupOrMatch::Name(Lookup::One(symbol)) => Argument::Symbol(symbol),
            LookupOrMatch::Name(Lookup::NoName) => return Err(Unusable::NoName),
            LookupOrMatch::Name(Lookup::Several(nodes)) => return Err(Unusable::Several(nodes)),
            LookupOrMatch::Pattern(matched) => Argument::Pattern(matched),
            LookupOrMatch::NoMatch(pattern) => return Err(Unusable::NoMatch(Box::new(pattern))),
        })
    }

    /// The error for `query`, given in place of SYMBOL, that a command that
    /// takes a set can't use, as `unusable` says why: for a name or a
    /// pattern, as every such command words it, and otherwise the error in
    /// [`Unusable::Error`]. A name that finds nothing offers first the
    /// pattern for what is in a module of that name whenever it matches an
    /// item ([`Takes::Set`]), and one that could mean several items says
    /// to use one or more of them, and ends with the pattern for all of
    /// them, when it takes just those (see [`Names::choices`]).
    pub(in crate::commands) fn unusable_error(&self, query: &str, unusable: &Unusable) -> String {
        match unusable {
            Unusable::NoName => self.nothing_named(query, Takes::Set),
            Unusable::NoMatch(pattern) => self.nothing_matches(pattern),
            Unusable::Several(nodes) => self.choices(query, nodes),
            Unusable::Error(error) => error.clone(),
        }
    }
}

impl Pick {
    /// What `argument` picks for a set: the item of a SYMBOL, as
    /// `item_in_set` gives it, with its name as the answer's first line
    /// gives it (see [`Symbol::lead_name`]), or the items of a pattern or
    /// a range. Fails as `item_in_set` does, as for an import, whose code
    /// isn't in the file.
    pub(in crate::commands) fn new(
        binary: &Binary,
        argument: Argument,
        item_in_set: ItemInSet,
    ) -> Result<Pick, Unusable<'static>> {
        Ok(match argument {
            Argument::Symbol(symbol) => Pick::Symbol {
                index: item_in_set(binary, symbol.node)?,
                lead_name: symbol.lead_name(binary),
            },
            Argument::Pattern(matched) => Pick::Pattern(matched),
            Argument::Range(range) => Pick::Range(range),
        })
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
/// ends (see [`is_dashed_range`]).
fn check_not_a_range(query: &str) -> Result<()> {
    if is_dashed_range(query) {
        bail!("`{query}` isn't a range: {}", range_hint(query));
    }
    Ok(())
}

/// Whether `query` is read as a range of memory rather than a symbol: it
/// starts with a digit, as no name does, and has `..` or `+` in it.
pub(in crate::commands) fn is_range(query: &str) -> bool {
    query.starts_with(|c: char| c.is_ascii_digit()) && (query.contains("..") || query.contains('+'))
}

/// Whether `query` is a range typed with `-` or `:` between its ends, as
/// in `0x0-0x40000`, which neither a range nor SYMBOL can be: no name
/// starts with a digit, and no address has `-` or `:` in it.
fn is_dashed_range(query: &str) -> bool {
    query.starts_with(|c: char| c.is_ascii_digit()) && query.contains(['-', ':'])
}

/// Whether `query` is meant as a range of memory, which
/// [`Names::read_argument`] reads as one or refuses: one that it reads as
/// a range (see [`is_range`]), or one typed with `-` or `:` between its
/// ends (see [`is_dashed_range`]). `retained`'s error calls its arguments
/// SYMBOLs until one is.
pub(in crate::commands) fn is_meant_as_range(query: &str) -> bool {
    is_range(query) || is_dashed_range(query)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::symbol::fixtures;

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
    fn a_range_typed_with_a_dash_is_meant_as_one_and_refused() {
        // Read as a range, or refused as one, and so not SYMBOL.
        for query in ["0x0+256K", "0x40000..0x0", "0x0-0x40000", "0x20000000:256K"] {
            assert!(is_meant_as_range(query), "{query}");
        }
        for query in ["0x1700", "operator+", "uart_*", "main"] {
            assert!(!is_meant_as_range(query), "{query}");
        }
        let binary = fixtures::functions(&["main"]);
        let refused = match Names::new(&binary).read_argument("0x0-0x40000") {
            Err(Unusable::Error(error)) => error,
            _ => panic!("0x0-0x40000 isn't refused"),
        };
        assert_eq!(
            refused,
            "`0x0-0x40000` isn't a range: did you mean 0x0..0x40000?"
        );
    }
}
