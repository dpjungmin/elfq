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
//!    [`short_names`](crate::binary::names::short_names));
//! 4. a pattern, such as `uart_*`: SYMBOL with a `*` that stands for any
//!    text in it, when no item or import has it as its name, so that a full
//!    name with a `*` in it, as `<*const T as core::fmt::Debug>::fmt`, still
//!    finds its item (see [`Names::look_up_or_match`]). `retained`,
//!    `callers` and `callees` take every item that it matches; the other
//!    commands take the one item that it matches, and when it matches
//!    several, give an error that lists them (see [`Names::find`]).
//!
//! When SYMBOL gives another of an item's names, an address inside it, or a
//! pattern that matches only it, the answer's first line says so (see
//! [`Symbol::lead_name`]).
//!
//! For `callers`, `callees` and `retained`, an argument can also be a range
//! of memory in place of SYMBOL (see [`Names::read_argument`]). They take
//! several SYMBOLs, patterns and ranges, and put what they pick together
//! in a [`Set`], each item once; `callers` and `callees` read theirs with
//! [`subject`].
//!
//! The files:
//!
//! - `mod.rs`, this file, reads what `callers` and `callees` ask about.
//! - `argument.rs` reads one argument of a command that takes a set: a
//!   range, or SYMBOL ([`Names::read_argument`]), with what it picks for a
//!   set ([`Pick::new`]); or why the command can't use it, an
//!   [`Unusable`], which is worded only when the error is built
//!   ([`Names::unusable_error`]). Its module doc says how a new command
//!   that takes a set reads its arguments.
//! - `lookup.rs` holds [`Names`], which finds what SYMBOL names, or the one
//!   item that a pattern matches, in the commands that take one item
//!   ([`Names::find`]), with the line of `shell_hint.rs` under the error
//!   for one that finds nothing, and names items for the user in return.
//! - `not_found.rs` words the error for SYMBOL that finds several items or
//!   none, as a command that takes one item or a set needs it ([`Takes`]),
//!   and for a pattern that matches none, or several in a command that
//!   takes one item. For a name that finds nothing, it offers first the
//!   pattern for what is in a module of that name, where the command can
//!   take what it matches. `similar.rs` finds the names that the error
//!   suggests.
//! - `pattern.rs` holds [`Pattern`], such as `uart_*`, when SYMBOL is one,
//!   and finds the items that one matches ([`Names::matching`]), which
//!   every command that takes SYMBOL takes in its place, and `debug
//!   matches` lists. Its unit tests are in `pattern/tests.rs`.
//! - `matched.rs` words what patterns matched, for an answer's first line
//!   and its notes, and gives the one item that a pattern matched
//!   ([`PatternMatches::only_item`]).
//! - `set.rs` holds [`Set`], the items that several SYMBOLs, patterns and
//!   ranges pick together, each once, and words how an answer's first line
//!   names them, part by part.
//! - `shell_hint.rs` words the line that says which arguments given in
//!   place of SYMBOL are files in the current folder, which the shell may
//!   have put there in place of a pattern, as it puts `uart_driver.c
//!   uart_hal.c` in place of `uart_*` ([`FilesHere`]). The errors of
//!   [`Names::find`] and [`subject`], and `retained`'s, end with it.
//! - `fixtures.rs` builds the file of functions that unit tests read.

mod argument;
#[cfg(test)]
mod fixtures;
mod lookup;
mod matched;
mod not_found;
mod pattern;
mod set;
mod shell_hint;
mod similar;

pub(super) use argument::{Argument, Unusable, is_meant_as_range, is_range};
pub(super) use lookup::{Lookup, Names};
pub(super) use matched::{PatternMatches, pattern_notes};
pub(super) use not_found::Takes;
#[cfg(feature = "tui")]
pub(super) use not_found::USE_ONE;
pub(super) use pattern::{LookupOrMatch, Match, MatchedBy, Pattern};
pub(super) use set::{Found, LeadName, Pick, Set};
pub(super) use shell_hint::{FilesHere, is_here, with_shell_hint};
#[cfg(feature = "tui")]
pub(super) use similar::Needle;

use anyhow::{Result, anyhow};

use crate::binary::{AddressRange, Binary, distinct_ranges};
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
    /// Ranges of memory alone, each once, in the order given: everything
    /// in them, together.
    Ranges(Vec<AddressRange>),
    /// Anything else: patterns, several SYMBOLs, or SYMBOLs with ranges.
    /// They answer about the items picked together, as about ranges, even
    /// for a pattern that matches one item, so that the answer's shape
    /// doesn't change when a build adds or drops a match.
    Set(Set),
}

/// How a command takes the item of a SYMBOL among others into a set (see
/// [`Pick::new`]): its index, or the command's error for an import, whose
/// code isn't in the file. `callers` and `callees` each have their own,
/// and `retained` takes `item_in_file`.
pub(super) type ItemInSet = fn(&Binary, Node) -> Result<usize>;

/// Works out what `queries` ask about, reading each with
/// [`Names::read_argument`] and in the order given: one SYMBOL alone (see
/// [`Subject::Symbol`]), ranges of memory alone, or else the [`Set`] of the
/// items they pick (see [`Pick::new`]). When it fails at an argument that
/// is a file here, its error ends with the line that says so, naming that
/// file and the files after it, which the shell may have put in place of
/// one pattern (see [`with_shell_hint`]). Stops at the first that it can't
/// use, such as a range it can't parse or a name or pattern that finds
/// nothing, with the error of a command that takes a set (see
/// [`Names::unusable_error`]); whether each range holds something is
/// checked after, by `range_check`. A set holds only items, so
/// `item_in_set` gives the item of a SYMBOL among others, or the command's
/// error for an import.
pub(super) fn subject(
    binary: &Binary,
    queries: &[String],
    item_in_set: ItemInSet,
) -> Result<Subject> {
    let names = Names::new(binary);
    let mut picks = Vec::new();
    for (index, query) in queries.iter().enumerate() {
        let pick = match names.read_argument(query) {
            // One SYMBOL alone may find an import, which no set holds, so
            // it is answered about as itself.
            Ok(Argument::Symbol(symbol)) if queries.len() == 1 => {
                return Ok(Subject::Symbol(symbol));
            }
            read => read.and_then(|argument| Pick::new(binary, argument, item_in_set)),
        };
        let pick = pick.map_err(|unusable| {
            let error = anyhow!(names.unusable_error(query, &unusable));
            // The shell puts all the files in place of one pattern, so when
            // the argument it stopped at is a file here, the line names it
            // and the files after it, which it hasn't read; those before it
            // found their items.
            match shell_hint::is_file_here(query) {
                true => with_shell_hint(error, &queries[index..]),
                false => error,
            }
        })?;
        picks.push(pick);
    }
    // Some only when every argument is a range: then they are ranges alone,
    // not a set, which would copy the name of every item in them.
    let ranges: Option<Vec<AddressRange>> = (picks.iter())
        .map(|pick| match pick {
            Pick::Range(range) => Some(*range),
            Pick::Symbol { .. } | Pick::Pattern(_) => None,
        })
        .collect();
    Ok(match ranges {
        // A range given twice is named once, as a set names it.
        Some(ranges) => Subject::Ranges(distinct_ranges(ranges)),
        None => Subject::Set(Set::new(binary, picks)),
    })
}

/// Finds the item or import that `query` names, or the one item that it
/// matches as a pattern, as `Names::find` does.
/// Use it when the answer names nothing else; when it does, as in
/// `retained` and `why`, keep one `Names` for the lookup and the naming,
/// so that the names are worked out once. Here they are freed before the
/// answer, so that what the command builds next can take their memory.
pub fn find(binary: &Binary, query: &str) -> Result<Symbol> {
    Names::new(binary).find(query)
}

#[cfg(test)]
mod tests {
    use anyhow::bail;

    use super::*;

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
        // It stops at the first argument that it can't use.
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
