//! The SYMBOLs that `retained` is given, and the patterns and ranges given
//! in their place: what each finds, the functions it leaves out, and the
//! error that lists every argument it can't use.
//!
//! Each SYMBOL is found as in every other command (see `commands::symbol`).
//! One with a `*` in it that names nothing is a pattern, such as `uart_*`
//! (see `Names::look_up_or_match`), which stands for every item it
//! matches, in address order, as a range in place of SYMBOL stands for
//! every item with a byte in it; an empty range gets the error that
//! `callers` gives it. An item that two arguments find counts once. A
//! function whose end elfq doesn't know, such as glibc's `_init`, is left
//! out: elfq can't read its code, so it can't tell what the function
//! keeps, but it can still answer about the others, and says which it left
//! out. When that leaves none, the error names every one.
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

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};

use super::refuse_shared_library;
use crate::binary::{AddressRange, Binary};
use crate::commands::checks::{ends_unknown, item_in_file};
use crate::commands::range_check::check_holds_items;
use crate::commands::symbol::{
    self, Argument, Lookup, LookupOrMatch, Names, Pattern, PatternMatches,
};
use crate::commands::text::{self, Naming};
use crate::graph::Node;

/// What `retained` answers about: the SYMBOLs that it found, the functions
/// that it leaves out, and whether patterns or ranges were given in place
/// of SYMBOL.
pub(super) struct Symbols {
    /// The SYMBOLs' items, and those of the patterns and ranges given in
    /// place of SYMBOL, each once, in the order given, and a pattern's or a
    /// range's in address order.
    pub(super) found: Vec<Found>,
    /// The functions given, or matched or in a range given, whose end elfq
    /// doesn't know, as indexes into [`Binary::items`], each once, in the
    /// order given. elfq can't read their code, so the answer doesn't count
    /// them, and says so (see [`left_out_note`]).
    pub(super) left_out: Vec<usize>,
    /// Whether SYMBOLs were given, patterns or ranges in their place, or a
    /// mix.
    pub(super) given: Given,
}

/// An item that `retained` was given: the one that a SYMBOL found, or one
/// that a pattern matched or that is in a range given in place of SYMBOL.
pub(super) struct Found {
    /// The item, as an index into [`Binary::items`].
    pub(super) index: usize,
    /// The item's name as the answer gives it (see [`Symbol::lead_name`]),
    /// or for an item that a pattern or range found, its name.
    ///
    /// [`Symbol::lead_name`]: crate::commands::symbol::Symbol::lead_name
    pub(super) lead_name: String,
    /// Whether a SYMBOL found it, and not only a pattern or range. The note
    /// on the items that add nothing names only those, since the user
    /// didn't name the others one by one.
    pub(super) named: bool,
}

/// What `retained` was given: SYMBOLs, patterns or ranges in place of
/// SYMBOL, or a mix, as far as its words depend on it.
pub(super) enum Given {
    /// Only SYMBOLs.
    Symbols,
    /// SYMBOLs and ranges.
    SymbolsAndRanges,
    /// Only ranges, which stand for the items in them, each once, in the
    /// order given.
    Ranges(Vec<AddressRange>),
    /// One or more patterns, perhaps with SYMBOLs and ranges.
    Patterns {
        /// Each pattern given, once, in the order given, with every item
        /// it matched, those that the answer leaves out since elfq doesn't
        /// know where they end among them.
        matched: Vec<PatternMatches>,
        /// What the answer's first line names, part by part, in the order
        /// given.
        lead_parts: Vec<LeadPart>,
    },
}

/// A part of what `retained`'s first line, its lead, names when a pattern
/// is among what it was given (see [`subject`]): one SYMBOL, the ranges or
/// the patterns.
///
/// [`subject`]: super::subject
pub(super) enum LeadPart {
    /// A SYMBOL: its item, as an index into [`Binary::items`], and the
    /// name that the answer gives it.
    Symbol { index: usize, lead_name: String },
    /// The ranges given in place of SYMBOL, each once, in the order given,
    /// which the first line names together, by the items in them.
    Ranges(Vec<AddressRange>),
    /// The patterns given in place of SYMBOL, which the first line names
    /// together, by the items they matched.
    Patterns,
}

impl Given {
    /// The patterns given, with the items they matched, or `None` when no
    /// pattern was given.
    pub(super) fn patterns(&self) -> Option<&[PatternMatches]> {
        match self {
            Given::Patterns { matched, .. } => Some(matched),
            Given::Symbols | Given::SymbolsAndRanges | Given::Ranges(_) => None,
        }
    }
}

/// What a SYMBOL, pattern or range picks, as [`find_symbols`] reads each
/// argument.
enum Pick {
    /// The item that a SYMBOL names.
    Symbol(Found),
    /// The items that a pattern matches.
    Pattern(PatternMatches),
    /// A range, and the items with a byte in it, in address order.
    Range {
        range: AddressRange,
        items: Vec<Found>,
    },
}

/// What the arguments that [`find_symbols`] has read so far picked, each
/// item once, in the order given.
#[derive(Default)]
struct Picked {
    found: Vec<Found>,
    left_out: Vec<usize>,
    /// Where each item that a SYMBOL can name again is in `found`, so that
    /// a SYMBOL that names an item that a pattern or range found first can
    /// say that it is named.
    found_at: HashMap<usize, usize>,
    /// The items in `found` and `left_out`.
    seen: HashSet<usize>,
    /// The patterns, each once, as typed, with the items they match.
    patterns: Vec<PatternMatches>,
    /// What the first line names, part by part (see [`LeadPart`]).
    lead_parts: Vec<LeadPart>,
    /// The items that SYMBOLs named, so that each has one part of the
    /// first line.
    named: HashSet<usize>,
}

impl Picked {
    /// Adds what an argument picked: an item that it picks again counts
    /// once, where it was first picked, and a function whose end elfq
    /// doesn't know is left out.
    fn add(&mut self, binary: &Binary, pick: Pick) {
        let finds = match pick {
            Pick::Symbol(found) => {
                if self.named.insert(found.index) {
                    self.lead_parts.push(LeadPart::Symbol {
                        index: found.index,
                        lead_name: found.lead_name.clone(),
                    });
                }
                vec![found]
            }
            Pick::Pattern(matched) => {
                let patterns = |part: &LeadPart| matches!(part, LeadPart::Patterns);
                if !self.lead_parts.iter().any(patterns) {
                    self.lead_parts.push(LeadPart::Patterns);
                }
                let typed = matched.pattern.typed();
                if (self.patterns.iter()).any(|seen| seen.pattern.typed() == typed) {
                    return;
                }
                let finds = (matched.matches.iter())
                    .map(|found| Found {
                        index: found.item,
                        lead_name: binary.items[found.item].name.clone(),
                        named: false,
                    })
                    .collect();
                self.patterns.push(matched);
                finds
            }
            Pick::Range { range, items } => {
                self.add_range(range);
                items
            }
        };
        for one in finds {
            self.add_item(binary, one);
        }
    }

    /// Adds `range` to the ranges that the first line names, once.
    fn add_range(&mut self, range: AddressRange) {
        let ranges = self.lead_parts.iter_mut().find_map(|part| match part {
            LeadPart::Ranges(ranges) => Some(ranges),
            LeadPart::Symbol { .. } | LeadPart::Patterns => None,
        });
        match ranges {
            Some(ranges) if !ranges.contains(&range) => ranges.push(range),
            Some(_) => {}
            None => self.lead_parts.push(LeadPart::Ranges(vec![range])),
        }
    }

    /// Adds `one`, an item that an argument found, unless an argument
    /// found it first, or to the functions left out.
    fn add_item(&mut self, binary: &Binary, one: Found) {
        if !self.seen.insert(one.index) {
            if one.named
                && let Some(&at) = self.found_at.get(&one.index)
            {
                self.found[at].named = true;
            }
            return;
        }
        if binary.items[one.index].end_unknown() {
            self.left_out.push(one.index);
        } else {
            self.found_at.insert(one.index, self.found.len());
            self.found.push(one);
        }
    }

    /// What the arguments picked, now that every one is read, and what
    /// `retained` was given: without a pattern, SYMBOLs, ranges or both;
    /// with one, the patterns and the parts that the first line names.
    fn into_symbols(self) -> Symbols {
        let Picked {
            found,
            left_out,
            patterns,
            lead_parts,
            ..
        } = self;
        let given = if patterns.is_empty() {
            let symbols = (lead_parts.iter()).any(|part| matches!(part, LeadPart::Symbol { .. }));
            let ranges = lead_parts.into_iter().find_map(|part| match part {
                LeadPart::Ranges(ranges) => Some(ranges),
                LeadPart::Symbol { .. } | LeadPart::Patterns => None,
            });
            match ranges {
                None => Given::Symbols,
                Some(ranges) if !symbols => Given::Ranges(ranges),
                Some(_) => Given::SymbolsAndRanges,
            }
        } else {
            Given::Patterns {
                matched: patterns,
                lead_parts,
            }
        };
        Symbols {
            found,
            left_out,
            given,
        }
    }
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
    let mut picked = Picked::default();
    let mut unusable: Vec<Unusable> = Vec::new();
    for (query, argument) in queries.iter().zip(arguments) {
        let pick = match argument {
            Ok(Argument::Symbol(query)) => find_symbol(binary, names, query),
            Ok(Argument::Range(range)) => (items_in(binary, range))
                .map(|items| Pick::Range { range, items })
                .map_err(Reason::from),
            Err(error) => Err(Reason::from(error)),
        };
        match pick {
            Ok(pick) => picked.add(binary, pick),
            Err(reason) => unusable.push(Unusable { query, reason }),
        }
    }
    if !unusable.is_empty() {
        let error = unusable_error(&unusable, queries.len(), arguments_word, all, names);
        bail!("{error}");
    }
    let symbols = picked.into_symbols();
    if symbols.found.is_empty() && !symbols.left_out.is_empty() {
        // Nothing else is left to answer about.
        let left_out: Vec<&str> = (symbols.left_out.iter())
            .map(|&index| binary.items[index].name.as_str())
            .collect();
        bail!("{}", ends_unknown(&left_out));
    }
    refuse_shared_library(binary, &symbols.found, &symbols.given, all)?;
    Ok(symbols)
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
    Ok(Pick::Symbol(Found {
        index: item_in_file(binary, symbol.node)?,
        lead_name: symbol.lead_name(binary),
        named: true,
    }))
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
            named: false,
        })
        .collect())
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
