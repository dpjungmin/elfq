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
//! calls them SYMBOLs, or arguments once a range is among them. Each
//! argument is read as in `callers` and `callees` (see
//! `Names::read_argument`), and its error is theirs (see
//! `Names::unusable_error`).

use std::collections::HashSet;

use anyhow::{Result, anyhow, bail};

use super::refuse_shared_library;
use crate::binary::Binary;
use crate::commands::checks::{check_holds_items, item_in_file};
use crate::commands::symbol::{self, Argument, Names, Pick, Set, Unusable};
use crate::commands::text::{self, Detail, Naming};

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
/// no item, the first 10 of them unless `detail` is [`Detail::All`] (see
/// [`refused_error`]), with the line that says which of the arguments it
/// can't use are files here under it, as the shell may have put them in
/// place of a pattern (see [`symbol::with_shell_hint`]); when it leaves out
/// every item, with an error that names them; and after those checks, for a
/// shared library.
pub(super) fn find_symbols(
    binary: &Binary,
    names: &Names,
    queries: &[String],
    detail: Detail,
) -> Result<Symbols> {
    let mut picks: Vec<Pick> = Vec::new();
    let mut refused: Vec<Refused> = Vec::new();
    for query in queries {
        let pick = names.read_argument(query).and_then(|argument| {
            // Only `retained` checks here that a range holds an item:
            // `callers` and `callees` check it once the graph is built,
            // since `callers` can ask what calls code outside every item.
            if let Argument::Range(range) = argument {
                check_holds_items(binary, range)?;
            }
            Pick::new(binary, argument, item_in_file)
        });
        match pick {
            Ok(pick) => picks.push(pick),
            Err(unusable) => refused.push(Refused { query, unusable }),
        }
    }
    if !refused.is_empty() {
        let arguments = arguments_word(queries);
        let error = refused_error(&refused, queries.len(), arguments, detail, names);
        // Only an argument that it can't use may be what the shell put in
        // place of a pattern: one that finds its item was typed as meant.
        let refused: Vec<&str> = refused.iter().map(|argument| argument.query).collect();
        return Err(symbol::with_shell_hint(anyhow!(error), &refused));
    }
    let mut set = Set::new(binary, picks);
    let left_out = set.leave_out_unknown_ends(binary);
    if set.items().is_empty() && !left_out.is_empty() {
        // Nothing else is left to answer about.
        let left_out: Vec<&str> = (left_out.iter())
            .map(|&index| binary.items[index].name.as_str())
            .collect();
        // An error names every one, however many there are.
        bail!("{}", text::ends_unknown(&left_out, Detail::All));
    }
    refuse_shared_library(binary, &set, detail)?;
    Ok(Symbols { set, left_out })
}

/// What `retained`'s error calls `queries`, the arguments it was given:
/// SYMBOLs, or once one meant as a range is among them, even one that it
/// can't read, arguments, since a range in place of SYMBOL isn't one (see
/// [`symbol::is_meant_as_range`]).
fn arguments_word(queries: &[String]) -> &'static str {
    match (queries.iter()).any(|query| symbol::is_meant_as_range(query)) {
        true => "arguments",
        false => "SYMBOLs",
    }
}

/// A SYMBOL, or a pattern or range in its place, that `retained` can't
/// use, with why.
struct Refused<'q, 'n> {
    /// The SYMBOL, pattern or range, as typed.
    query: &'q str,
    unusable: Unusable<'n>,
}

/// How many of the arguments that it can't use `retained`'s error lists
/// before "and 5 more", unless `--all` asks for every one.
const LISTED: usize = 10;

/// The error for `refused`, the arguments that `retained` can't use of
/// the `count` that it was given, which it calls `arguments` (see
/// [`arguments_word`]), in the order given. One whose error says what
/// another's does, such as a name given twice, is listed once. When that
/// leaves one, it is the error that it gets alone (see
/// [`Names::unusable_error`]); otherwise, a line that counts every argument
/// that it can't use, then the errors of the first [`LISTED`], or of every
/// one if `detail` is [`Detail::All`], each indented under it, then how
/// many more. Only those errors are worded.
fn refused_error(
    refused: &[Refused],
    count: usize,
    arguments: &str,
    detail: Detail,
    names: &Names,
) -> String {
    let mut said = HashSet::new();
    let distinct: Vec<&Refused> = (refused.iter())
        .filter(|&one| said.insert(one.says()))
        .collect();
    if let [one] = distinct[..] {
        return names.unusable_error(one.query, &one.unusable);
    }
    let room = detail.room(LISTED);
    let listed = text::shown_count(distinct.len(), room);
    let mut message = format!(
        "elfq can't use {} of the {count} {arguments}:",
        refused.len()
    );
    for one in &distinct[..listed] {
        for line in names.unusable_error(one.query, &one.unusable).lines() {
            message += &format!("\n  {line}");
        }
    }
    if listed < distinct.len() {
        message += &format!("\n  and {} more", distinct.len() - listed);
    }
    message
}

impl Refused<'_, '_> {
    /// What two arguments' errors are compared by, to list each error once:
    /// the error that every command gives the argument, which is the same
    /// for an address typed two ways, such as 0x1 and 0x01; or else the
    /// SYMBOL as typed, which the errors that [`Names::unusable_error`]
    /// words name.
    fn says(&self) -> Says<'_> {
        match &self.unusable {
            Unusable::Error(error) => Says::Error(error),
            Unusable::NoName | Unusable::NoMatch(_) | Unusable::Several(_) => {
                Says::Query(self.query)
            }
        }
    }
}

/// What two [`Refused`]s' errors are compared by (see
/// [`Refused::says`]).
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
