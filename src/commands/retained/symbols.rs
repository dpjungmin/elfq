//! The SYMBOLs that `retained` is given, and the ranges given in their
//! place: what each finds, the functions it leaves out, and the error that
//! lists every argument it can't use.
//!
//! Each SYMBOL is found as in every other command (see `commands::symbol`).
//! A range in place of SYMBOL stands for every item with a byte in it, in
//! address order, as `callers` reads a range, and an empty one gets the
//! error that `callers` gives it. An item that two arguments find counts
//! once. A function whose end elfq doesn't know, such as glibc's `_init`,
//! is left out: elfq can't read its code, so it can't tell what the
//! function keeps, but it can still answer about the others, and says which
//! it left out. When that leaves none, the error names every one.
//!
//! Every argument that `retained` can't use is listed at once, so that a
//! long list of them can be fixed in one go: one with the error it gets
//! alone, several each with that error, under a line that counts them, and
//! calls them SYMBOLs, or arguments once a range is among them. The errors
//! are those of every command, with two hints of `retained`'s own, since it
//! takes several SYMBOLs: a name that could mean several items says to use
//! one or more of them, and a name with `*` in it that finds nothing says
//! that elfq doesn't take patterns yet, unless elfq can say why no item has
//! the name, or which it may mean.

use std::collections::HashSet;

use anyhow::{Result, bail};

use super::refuse_shared_library;
use crate::binary::{AddressRange, Binary};
use crate::commands::checks::{ends_unknown, item_in_file};
use crate::commands::range_check::check_holds_items;
use crate::commands::symbol::{self, Argument, Lookup, Names};
use crate::commands::text::{self, Naming};
use crate::graph::Node;

/// What `retained` answers about: the SYMBOLs that it found, and the
/// functions that it leaves out.
pub(super) struct Symbols {
    /// The SYMBOLs' items, and those of the ranges given in place of
    /// SYMBOL, each once, in the order given, and a range's in address
    /// order.
    pub(super) found: Vec<Found>,
    /// The functions given, or in a range given, whose end elfq doesn't
    /// know, as indexes into [`Binary::items`], each once, in the order
    /// given. elfq can't read their code, so the answer doesn't count them,
    /// and says so (see [`left_out_note`]).
    pub(super) left_out: Vec<usize>,
}

/// An item that `retained` was given: the one that a SYMBOL found, or one
/// in a range given in place of SYMBOL.
pub(super) struct Found {
    /// The item, as an index into [`Binary::items`].
    pub(super) index: usize,
    /// The item's name as the answer gives it (see [`Symbol::lead_name`]),
    /// or for an item in a range, its name.
    ///
    /// [`Symbol::lead_name`]: crate::commands::symbol::Symbol::lead_name
    pub(super) lead_name: String,
}

/// What `retained` was given: SYMBOLs, ranges in place of SYMBOL, or both,
/// as far as its words depend on it.
enum Given {
    /// Only SYMBOLs.
    Symbols,
    /// SYMBOLs and ranges.
    SymbolsAndRanges,
    /// Only ranges.
    Ranges,
}

impl Given {
    /// What `arguments` are, each as [`symbol::argument`] read it. It
    /// fails only for an argument meant as a range, so one that it can't
    /// read counts as a range here.
    fn of(arguments: &[Result<Argument>]) -> Given {
        let symbols = (arguments.iter())
            .filter(|argument| matches!(argument, Ok(Argument::Symbol(_))))
            .count();
        match symbols {
            0 => Given::Ranges,
            symbols if symbols == arguments.len() => Given::Symbols,
            _ => Given::SymbolsAndRanges,
        }
    }
}

/// Finds what each of `queries` names, by `names`, or for a range, the
/// items in it, each item once, in the order given, and leaves out each
/// function whose end elfq doesn't know. Fails with an error that lists
/// every argument that it can't use, such as an import or a range that
/// holds no item, the first 10 of them unless `all` (see
/// [`unusable_error`]); when it leaves out every item, with an error that
/// names them; and after those checks, for a shared library.
pub(super) fn find_symbols(
    binary: &Binary,
    names: &Names,
    queries: &[String],
    all: bool,
) -> Result<Symbols> {
    // Each argument is read once, for what it finds and for what the error
    // that lists those it can't use calls them.
    let arguments: Vec<Result<Argument>> = (queries.iter())
        .map(|query| symbol::argument(query))
        .collect();
    let given = Given::of(&arguments);
    let mut found: Vec<Found> = Vec::new();
    let mut left_out = Vec::new();
    let mut unusable: Vec<Unusable> = Vec::new();
    let mut seen = HashSet::new();
    for (query, argument) in queries.iter().zip(arguments) {
        let finds = match argument {
            Ok(Argument::Symbol(query)) => {
                find_symbol(binary, names, query).map(|found| vec![found])
            }
            Ok(Argument::Range(range)) => items_in(binary, range).map_err(Reason::from),
            Err(error) => Err(Reason::from(error)),
        };
        let finds = match finds {
            Ok(finds) => finds,
            Err(reason) => {
                unusable.push(Unusable { query, reason });
                continue;
            }
        };
        for one in finds {
            // An item that two arguments find counts once.
            if !seen.insert(one.index) {
                continue;
            }
            if binary.items[one.index].end_unknown() {
                left_out.push(one.index);
            } else {
                found.push(one);
            }
        }
    }
    if !unusable.is_empty() {
        let error = unusable_error(&unusable, queries.len(), &given, all, names);
        bail!("{error}");
    }
    if found.is_empty() && !left_out.is_empty() {
        // Nothing else is left to answer about.
        let left_out: Vec<&str> = (left_out.iter())
            .map(|&index| binary.items[index].name.as_str())
            .collect();
        bail!("{}", ends_unknown(&left_out));
    }
    let items: Vec<&str> = (found.iter())
        .map(|found| binary.items[found.index].name.as_str())
        .collect();
    refuse_shared_library(binary, &items)?;
    Ok(Symbols { found, left_out })
}

/// The item that `query`, SYMBOL, finds, with the name that the answer
/// gives it, or why `retained` can't use it.
fn find_symbol<'n>(binary: &Binary, names: &'n Names, query: &str) -> Result<Found, Reason<'n>> {
    let symbol = match names.look_up(query)? {
        Lookup::One(symbol) => symbol,
        Lookup::NoName => return Err(Reason::NoName),
        Lookup::Several(nodes) => return Err(Reason::Several(nodes)),
    };
    let index = item_in_file(binary, symbol.node)?;
    let lead_name = symbol.lead_name(binary);
    Ok(Found { index, lead_name })
}

/// The items with a byte in `range`, in address order, each with its name,
/// as `callers` reads a range. Fails for a range that holds no item, with
/// the error that `callers` gives it (see [`check_holds_items`]).
fn items_in(binary: &Binary, range: AddressRange) -> Result<Vec<Found>> {
    check_holds_items(binary, range)?;
    Ok((binary.items.iter().enumerate())
        .filter(|(_, item)| item.is_in(&[range]))
        .map(|(index, item)| Found {
            index,
            lead_name: item.name.clone(),
        })
        .collect())
}

/// A SYMBOL, or a range in its place, that `retained` can't use.
struct Unusable<'q, 'n> {
    /// The SYMBOL or range, as typed.
    query: &'q str,
    reason: Reason<'n>,
}

/// Why `retained` can't use a SYMBOL, or a range in its place.
enum Reason<'n> {
    /// No item or import has the name.
    NoName,
    /// Several items or imports have the name.
    Several(&'n [Node]),
    /// The error that every command gives it, as elfq prints it: an
    /// address that no item holds, or that isn't one, an import, whose
    /// code isn't in the file, or a range that holds no item, or that isn't
    /// one.
    Error(String),
}

impl From<anyhow::Error> for Reason<'_> {
    /// The error that every command gives, as elfq prints it.
    fn from(error: anyhow::Error) -> Self {
        Reason::Error(format!("{error:#}"))
    }
}

/// How many of the arguments that it can't use `retained`'s error lists
/// before "and 5 more", unless `--all` asks for every one.
const LISTED: usize = 10;

/// What `retained`'s error for SYMBOL that finds several things says to do,
/// since it takes several SYMBOLs, where other commands say to use one.
const USE_ONE_OR_MORE: &str = "use one or more on the left instead";

/// What `retained`'s error for a name with `*` in it that finds nothing
/// adds (see [`is_pattern`]).
const NO_PATTERNS: &str =
    "elfq doesn't take patterns yet: give each name, or each address from top --json";

/// The error for `unusable`, the arguments that `retained` can't use of the
/// `count` that it was `given`, in the order given. One whose error says
/// what another's does, such as a name given twice, is listed once. When
/// that leaves one, it is the error that it gets alone; otherwise, a line
/// that counts every argument that it can't use, then the errors of the
/// first [`LISTED`], or of every one if `all`, each indented under it, then
/// how many more.
fn unusable_error(
    unusable: &[Unusable],
    count: usize,
    given: &Given,
    all: bool,
    names: &Names,
) -> String {
    let mut said = HashSet::new();
    let distinct: Vec<&Unusable> = (unusable.iter())
        .filter(|&one| said.insert(one.says()))
        .collect();
    if let [one] = distinct[..] {
        return one.error(names);
    }
    let room = if all { usize::MAX } else { LISTED };
    let listed = text::shown_count(distinct.len(), room);
    let arguments = match given {
        Given::Symbols => "SYMBOLs",
        // A range in place of SYMBOL isn't one.
        Given::SymbolsAndRanges | Given::Ranges => "arguments",
    };
    let mut message = format!(
        "elfq can't use {} of the {count} {arguments}:",
        unusable.len()
    );
    for one in &distinct[..listed] {
        for line in one.error(names).lines() {
            message += &format!("\n  {line}");
        }
    }
    if listed < distinct.len() {
        message += &format!("\n  and {} more", distinct.len() - listed);
    }
    message
}

impl Unusable<'_, '_> {
    /// The error for this argument alone: every command's, with
    /// `retained`'s hints.
    fn error(&self, names: &Names) -> String {
        let query = self.query;
        match &self.reason {
            Reason::NoName => names.nothing_named(query, is_pattern(query).then_some(NO_PATTERNS)),
            Reason::Several(nodes) => names.choices(query, nodes, USE_ONE_OR_MORE),
            Reason::Error(error) => error.clone(),
        }
    }

    /// What two arguments' errors are compared by, to list each error once:
    /// the error that every command gives the argument, which is the same
    /// for an address typed two ways, such as 0x1 and 0x01; or else the
    /// SYMBOL as typed, which the errors that `retained` words name.
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
    /// The error that every command gives the argument.
    Error(&'u str),
    /// The SYMBOL as typed.
    Query(&'u str),
}

/// Whether `query` may be meant as a pattern, such as `core::fmt::*`,
/// which elfq doesn't take yet. A C++ name with a pointer in it has `*` in
/// it too, but when such a name is mistyped, elfq most often suggests the
/// right one, and its error gives that in place of the hint (see
/// [`Names::nothing_named`]).
fn is_pattern(query: &str) -> bool {
    query.contains('*')
}

/// Says that the answer doesn't count the functions at `left_out`, since
/// elfq doesn't know where they end, naming them as `naming` says, as in
/// "Not counted, since elfq doesn't know where it ends: _init." `None`
/// when it leaves out none.
pub(super) fn left_out_note(
    binary: &Binary,
    left_out: &[usize],
    naming: &Naming,
) -> Option<String> {
    let names: Vec<&str> = (left_out.iter())
        .map(|&index| binary.items[index].name.as_str())
        .collect();
    let end = match names[..] {
        [] => return None,
        [_] => "it ends",
        _ => "they end",
    };
    Some(format!(
        "Not counted, since elfq doesn't know where {end}: {}.",
        naming.phrase(&names)
    ))
}
