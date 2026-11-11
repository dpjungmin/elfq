//! The SYMBOLs that `retained` is given, and the patterns and ranges given
//! in their place: what each finds, the functions it leaves out, and the
//! error that lists every argument it can't use.
//!
//! Each SYMBOL is found as in every other command (see `commands::symbol`).
//! One with a `*` in it that names nothing is a pattern, such as `uart_*`
//! (see `Names::look_up_or_match`), which stands for every item it
//! matches, in address order, as a range in place of SYMBOL stands for
//! every item with a byte in it; an empty range gets the error that
//! `callers` gives it. What they pick goes in a `symbol::Set`, where an
//! item that two arguments find counts once. A function whose end elfq
//! doesn't know, such as glibc's `_init`, is left out: elfq can't read its
//! code, so it can't tell what the function keeps, but it can still answer
//! about the others, and says which it left out. When that leaves none,
//! the error names every one.
//!
//! Every argument that `retained` can't use is listed at once, so that a
//! long list of them can be fixed in one go: one with the error it gets
//! alone, several each with that error, under a line that counts them, and
//! calls them SYMBOLs, or arguments once a range is among them. The errors
//! are those of every command, with a hint of `retained`'s own, since it
//! takes several SYMBOLs: a name that could mean several items says to use
//! one or more of them. A pattern that matches nothing gets the error for
//! a name that finds nothing, with a pattern like it that matches some,
//! if there is one.

use std::collections::HashSet;

use anyhow::{Result, bail};

use super::refuse_shared_library;
use crate::binary::Binary;
use crate::commands::checks::{ends_unknown, item_in_file};
use crate::commands::range_check::check_holds_items;
use crate::commands::symbol::{self, Argument, Lookup, LookupOrMatch, Names, Pattern, Pick, Set};
use crate::commands::text::{self, Naming};
use crate::graph::Node;

/// What `retained` answers about: the items that its arguments pick, and
/// the functions that it leaves out.
pub(super) struct Symbols {
    /// What the arguments pick together: the SYMBOLs' items, and those of
    /// the patterns and ranges given in place of SYMBOL, each once, in the
    /// order given, and a pattern's or a range's in address order, less
    /// the functions in `left_out`; and how the first line names them.
    pub(super) set: Set,
    /// The functions given, or matched or in a range given, whose end elfq
    /// doesn't know, as indexes into [`Binary::items`], each once, in the
    /// order given. elfq can't read their code, so the answer doesn't count
    /// them, and says so (see [`left_out_note`]).
    pub(super) left_out: Vec<usize>,
}

/// Finds what each of `queries` names, by `names`, or for a pattern, the
/// items that it matches, or for a range, the items in it, each item once,
/// in the order given, and leaves out each function whose end elfq doesn't
/// know. Fails with an error that lists every argument that it can't use,
/// such as an import, a pattern that matches nothing or a range that holds
/// no item, the first 10 of them unless `all` (see [`unusable_error`]);
/// when it leaves out every item, with an error that names them; and after
/// those checks, for a shared library.
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
    let arguments_word = arguments_word(&arguments);
    let mut picks: Vec<Pick> = Vec::new();
    let mut unusable: Vec<Unusable> = Vec::new();
    for (query, argument) in queries.iter().zip(arguments) {
        let pick = match argument {
            Ok(Argument::Symbol(query)) => find_symbol(binary, names, query),
            Ok(Argument::Range(range)) => (check_holds_items(binary, range))
                .map(|()| Pick::Range(range))
                .map_err(Reason::from),
            Err(error) => Err(Reason::from(error)),
        };
        match pick {
            Ok(pick) => picks.push(pick),
            Err(reason) => unusable.push(Unusable { query, reason }),
        }
    }
    if !unusable.is_empty() {
        let error = unusable_error(&unusable, queries.len(), arguments_word, all, names);
        bail!("{error}");
    }
    let mut set = Set::new(binary, picks);
    let left_out = set.leave_out_unknown_ends(binary);
    if set.items().is_empty() && !left_out.is_empty() {
        // Nothing else is left to answer about.
        let left_out: Vec<&str> = (left_out.iter())
            .map(|&index| binary.items[index].name.as_str())
            .collect();
        bail!("{}", ends_unknown(&left_out));
    }
    refuse_shared_library(binary, &set, all)?;
    Ok(Symbols { set, left_out })
}

/// What `retained`'s error calls the arguments it was given: SYMBOLs, or
/// once a range is among them, arguments, since a range in place of SYMBOL
/// isn't one. [`symbol::argument`] fails only for an argument meant as a
/// range, so one that it can't read counts as a range.
fn arguments_word(arguments: &[Result<Argument>]) -> &'static str {
    match (arguments.iter()).all(|argument| matches!(argument, Ok(Argument::Symbol(_)))) {
        true => "SYMBOLs",
        false => "arguments",
    }
}

/// The item that `query`, SYMBOL, finds, with the name that the answer
/// gives it, or when it is a pattern, the items that it matches (see
/// [`Names::look_up_or_match`]), or why `retained` can't use it.
fn find_symbol<'n>(binary: &Binary, names: &'n Names, query: &str) -> Result<Pick, Reason<'n>> {
    let symbol = match names.look_up_or_match(query)? {
        LookupOrMatch::Name(Lookup::One(symbol)) => symbol,
        LookupOrMatch::Name(Lookup::NoName) => return Err(Reason::NoName),
        LookupOrMatch::Name(Lookup::Several(nodes)) => return Err(Reason::Several(nodes)),
        LookupOrMatch::Pattern(matched) => return Ok(Pick::Pattern(matched)),
        LookupOrMatch::NoMatch(pattern) => return Err(Reason::NoMatch(Box::new(pattern))),
    };
    Ok(Pick::Symbol {
        index: item_in_file(binary, symbol.node)?,
        lead_name: symbol.lead_name(binary),
    })
}

/// A SYMBOL, or a pattern or range in its place, that `retained` can't
/// use.
struct Unusable<'q, 'n> {
    /// The SYMBOL, pattern or range, as typed.
    query: &'q str,
    reason: Reason<'n>,
}

/// Why `retained` can't use a SYMBOL, or a pattern or range in its place.
enum Reason<'n> {
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

/// The error for `unusable`, the arguments that `retained` can't use of the
/// `count` that it was given, which it calls `arguments` (see
/// [`arguments_word`]), in the order given. One whose error says what
/// another's does, such as a name given twice, is listed once. When that
/// leaves one, it is the error that it gets alone; otherwise, a line that
/// counts every argument that it can't use, then the errors of the first
/// [`LISTED`], or of every one if `all`, each indented under it, then how
/// many more.
fn unusable_error(
    unusable: &[Unusable],
    count: usize,
    arguments: &str,
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
    /// `retained`'s own hint for a name that could mean several items.
    fn error(&self, names: &Names) -> String {
        let query = self.query;
        match &self.reason {
            Reason::NoName => names.nothing_named(query),
            Reason::NoMatch(pattern) => names.nothing_matches(pattern),
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
