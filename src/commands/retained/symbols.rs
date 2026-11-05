//! The SYMBOLs that `retained` is given: what each finds, and the error
//! that lists every SYMBOL it can't use.
//!
//! Each SYMBOL is found as in every other command (see `commands::symbol`),
//! and an item that two of them find counts once. Every SYMBOL that
//! `retained` can't use is listed at once, so that a long list of them can
//! be fixed in one go: one with the error it gets alone, several each with
//! that error, under a line that counts them. The errors are those of every
//! command, with two hints of `retained`'s own, since it takes several
//! SYMBOLs: a name that could mean several items says to use one or more of
//! them, and a name with `*` in it that finds nothing says that elfq
//! doesn't take patterns yet, unless elfq can say why no item has the name,
//! or which it may mean. A range gets an error of its own, until `retained`
//! takes ranges.

use std::collections::HashSet;

use anyhow::{Result, bail};

use super::refuse_shared_library;
use crate::binary::Binary;
use crate::commands::checks::readable_item;
use crate::commands::symbol::{self, Lookup, Names};
use crate::commands::text;
use crate::graph::Node;

/// A SYMBOL that `retained` was given, and found.
pub(super) struct Found {
    /// Its item, as an index into [`Binary::items`].
    pub(super) index: usize,
    /// The item's name as the answer gives it (see [`Symbol::lead_name`]).
    ///
    /// [`Symbol::lead_name`]: crate::commands::symbol::Symbol::lead_name
    pub(super) lead_name: String,
}

/// Finds what each of `queries` names, by `names`, each item once, in the
/// order given. Fails with an error that lists every SYMBOL that it can't
/// use, such as an import or a function whose end elfq doesn't know, the
/// first 10 of them unless `all` (see [`unusable_error`]); and after that
/// check, for a shared library.
pub(super) fn find_symbols(
    binary: &Binary,
    names: &Names,
    queries: &[String],
    all: bool,
) -> Result<Vec<Found>> {
    let mut found: Vec<Found> = Vec::new();
    let mut unusable: Vec<Unusable> = Vec::new();
    let mut seen = HashSet::new();
    for query in queries {
        let symbol = match find_symbol(binary, names, query) {
            Ok(symbol) => symbol,
            Err(reason) => {
                unusable.push(Unusable { query, reason });
                continue;
            }
        };
        // An item that two SYMBOLs find counts once.
        if seen.insert(symbol.index) {
            found.push(symbol);
        }
    }
    if !unusable.is_empty() {
        bail!("{}", unusable_error(&unusable, queries.len(), all, names));
    }
    let items: Vec<&str> = (found.iter())
        .map(|found| binary.items[found.index].name.as_str())
        .collect();
    refuse_shared_library(binary, &items)?;
    Ok(found)
}

/// The item that `query` finds, with the name that the answer gives it, or
/// why `retained` can't use it.
fn find_symbol<'n>(binary: &Binary, names: &'n Names, query: &str) -> Result<Found, Reason<'n>> {
    if symbol::is_range(query) {
        return Err(Reason::Range);
    }
    let symbol = match names.look_up(query) {
        Ok(Lookup::One(symbol)) => symbol,
        Ok(Lookup::NoName) => return Err(Reason::NoName),
        Ok(Lookup::Several(nodes)) => return Err(Reason::Several(nodes)),
        Err(error) => return Err(Reason::Error(format!("{error:#}"))),
    };
    let index = match readable_item(binary, symbol.node) {
        Ok(index) => index,
        Err(error) => return Err(Reason::Error(format!("{error:#}"))),
    };
    let lead_name = symbol.lead_name(binary);
    Ok(Found { index, lead_name })
}

/// A SYMBOL that `retained` can't use.
struct Unusable<'q, 'n> {
    /// The SYMBOL, as typed.
    query: &'q str,
    reason: Reason<'n>,
}

/// Why `retained` can't use a SYMBOL.
enum Reason<'n> {
    /// It is a range of memory, which `retained` can't take yet.
    Range,
    /// No item or import has the name.
    NoName,
    /// Several items or imports have the name.
    Several(&'n [Node]),
    /// The error that every command gives it, as elfq prints it: an
    /// address that no item holds, or that isn't one, an import, whose
    /// code isn't in the file, or a function whose end elfq doesn't know.
    Error(String),
}

/// How many of the SYMBOLs that it can't use `retained`'s error lists before
/// "and 5 more", unless `--all` asks for every one.
const LISTED: usize = 10;

/// What `retained`'s error for SYMBOL that finds several things says to do,
/// since it takes several SYMBOLs, where other commands say to use one.
const USE_ONE_OR_MORE: &str = "use one or more on the left instead";

/// What `retained`'s error for a name with `*` in it that finds nothing
/// adds (see [`is_pattern`]).
const NO_PATTERNS: &str =
    "elfq doesn't take patterns yet: give each name, or each address from top --json";

/// The error for `unusable`, the SYMBOLs of the `given` that `retained`
/// can't use, in the order given. A SYMBOL whose error says what another's
/// does, such as a name given twice, is listed once. When that leaves one,
/// it is the error that SYMBOL gets alone; otherwise, a line that counts
/// every SYMBOL it can't use, then the errors of the first [`LISTED`], or
/// of every one if `all`, each indented under it, then how many more.
fn unusable_error(unusable: &[Unusable], given: usize, all: bool, names: &Names) -> String {
    let mut said = HashSet::new();
    let distinct: Vec<&Unusable> = (unusable.iter())
        .filter(|&one| said.insert(one.says()))
        .collect();
    if let [one] = distinct[..] {
        return one.error(names);
    }
    let room = if all { usize::MAX } else { LISTED };
    let listed = text::shown_count(distinct.len(), room);
    let mut message = format!("elfq can't use {} of the {given} SYMBOLs:", unusable.len());
    for one in &distinct[..listed] {
        for line in one.in_list(names).lines() {
            message += &format!("\n  {line}");
        }
    }
    if listed < distinct.len() {
        message += &format!("\n  and {} more", distinct.len() - listed);
    }
    message
}

impl Unusable<'_, '_> {
    /// The error for this SYMBOL alone: every command's, with `retained`'s
    /// hints.
    fn error(&self, names: &Names) -> String {
        let query = self.query;
        match &self.reason {
            Reason::Range => range_refused(None),
            Reason::NoName => names.nothing_named(query, is_pattern(query).then_some(NO_PATTERNS)),
            Reason::Several(nodes) => names.choices(query, nodes, USE_ONE_OR_MORE),
            Reason::Error(error) => error.clone(),
        }
    }

    /// The error for this SYMBOL as the error that lists several gives it:
    /// the error it gets alone, but for a range, which it names, since a
    /// range's error alone doesn't.
    fn in_list(&self, names: &Names) -> String {
        match self.reason {
            Reason::Range => range_refused(Some(self.query)),
            _ => self.error(names),
        }
    }

    /// What two SYMBOLs' errors are compared by, to list each error once:
    /// the error that every command gives the SYMBOL, which is the same for
    /// an address typed two ways, such as 0x1 and 0x01; or else the SYMBOL
    /// as typed, which the errors that `retained` words name.
    fn says(&self) -> Says<'_> {
        match &self.reason {
            Reason::Error(error) => Says::Error(error),
            _ => Says::Query(self.query),
        }
    }
}

/// What two [`Unusable`]s' errors are compared by (see
/// [`Unusable::says`]).
#[derive(PartialEq, Eq, Hash)]
enum Says<'u> {
    /// The error that every command gives the SYMBOL.
    Error(&'u str),
    /// The SYMBOL as typed.
    Query(&'u str),
}

/// Refuses a range in place of SYMBOL, which `retained` can't take yet,
/// naming `range` when it is given, as the error that lists several does.
fn range_refused(range: Option<&str>) -> String {
    let range = match range {
        Some(range) => format!("the range `{range}`"),
        None => "a range".to_string(),
    };
    format!("retained can't yet take {range} in place of SYMBOL; give each item's name or address")
}

/// Whether `query` may be meant as a pattern, such as `core::fmt::*`,
/// which elfq doesn't take yet. A C++ name with a pointer in it has `*` in
/// it too, but when such a name is mistyped, elfq most often suggests the
/// right one, and its error gives that in place of the hint (see
/// [`Names::nothing_named`]).
fn is_pattern(query: &str) -> bool {
    query.contains('*')
}
