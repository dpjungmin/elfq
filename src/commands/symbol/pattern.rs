//! Patterns, such as `uart_*` or `core::fmt::*`, which pick the items whose
//! names they match.
//!
//! A `*` in a pattern stands for any text, none included, and it crosses
//! `::`, `<`, `(` and spaces. A pattern matches an item when it matches all
//! of one of the texts of the item's names, which
//! [`names::for_each_pattern_text`] makes, and [`Names::matching`] gives
//! the items that a pattern matches, each with how it matched and the text
//! it matched as ([`Match`]). [`Names::look_up_or_match`] says when SYMBOL
//! is a pattern, for `retained`, which takes one in its place, and `elfq
//! FILE debug matches PATTERN...` lists the matches.
//!
//! # The rules, and why
//!
//! - **Ends.** A text can be the end of a longer path, so `Uart::*` takes
//!   `drv::Uart::read`, as SYMBOL takes `process_frame` for
//!   `app::process_frame`.
//! - **A method's owner.** A method in an impl block counts under its type,
//!   or under its trait when the type has no path of its own:
//!   `<bigfw::Uart as core::fmt::Write>::write_str` under `bigfw::Uart`, and
//!   `<u32 as core::fmt::Display>::fmt` under `core::fmt::Display`. With
//!   both paths, `core::fmt::*` would take every type's `Debug` impl; with
//!   the type's alone, it would leave out how numbers are formatted.
//! - **Class data.** `drv::Uart::*` takes `vtable for drv::Uart`, and the
//!   class's typeinfo, since the vtable is what keeps the class's virtual
//!   methods: a set of the class's items without it would leave them kept.
//!   A thunk, a guard or a TLS function has the texts of the function or
//!   variable it is for, since it is part of that.
//! - **Copies.** `*_l` takes `_strtol_l.isra.0`, since a copy that a
//!   compiler made of a function is that function to whoever wrote it.
//! - **Pointers.** Inside `( )`, a `*` right after a letter, digit, `_`,
//!   `>` or another such `*` is a pointer, part of the name, since the
//!   demangler never puts a space before one. So
//!   `read(unsigned char*, unsigned int)`, copied from what elfq prints, is
//!   a name, not a pattern that would take another `read`, and `read(*)`
//!   takes every `read`.
//! - **Mangled aliases don't count.** elfq never prints them, so a match by
//!   one, as `_ZN3drv*` by `_ZN3drv4Uart4readEPhj`, would surprise.
//! - **A leading `::`.** A pattern that starts with `::` matches only from
//!   the root: whole names and paths, never the end of a longer path. It is
//!   the only way to take the crate `gimli` without std's own module
//!   `std::backtrace_rs::symbolize::gimli`, and since no name starts with
//!   `::`, it can't mean anything else.
//! - **Case counts.** C keeps families of names apart by case, as curl
//!   keeps `curl_*` and `Curl_*`.
//! - **Items, never imports.** A pattern picks items, PLT stubs among them,
//!   as a range does. An import isn't an item, so a pattern doesn't take
//!   one.
//!
//! Most names have no text that a pattern matches, and a quick check of the
//! pattern's words rules them out before their texts are worked out (see
//! [`QuickCheck`]).

use std::borrow::Cow;

use serde::Serialize;

use anyhow::Result;

use super::{Lookup, Names, PatternMatches};
use crate::binary::Item;
use crate::binary::names::{self, TextKind};

/// A pattern, as the user typed it, made ready to match names.
pub(in crate::commands) struct Pattern {
    /// The pattern as typed, with its leading `::` if it has one.
    typed: String,
    /// Whether the pattern starts with `::`, so that it matches only from
    /// the root: texts of the kind [`TextKind::Path`].
    from_root: bool,
    /// What a text that the pattern matches starts with: what comes before
    /// the first `*` that stands for any text, after a leading `::`.
    prefix: String,
    /// What comes between the `*`s that stand for any text, which a text
    /// that the pattern matches has in this order between its prefix and
    /// its suffix.
    middle: Vec<String>,
    /// What a text that the pattern matches ends with: what comes after the
    /// last `*` that stands for any text.
    suffix: String,
    /// What a name needs for one of its texts to match.
    quick_check: QuickCheck,
    /// Whether case counts, as it does in a pattern the user gives, or is
    /// ignored, as when the error for one that matches nothing looks for
    /// what it may mean. With case ignored, the pieces above are in
    /// lowercase.
    case: Case,
}

/// Whether a pattern tells letters in uppercase from those in lowercase.
#[derive(Clone, Copy)]
enum Case {
    /// Case counts, as C keeps `curl_*` and `Curl_*` apart.
    Counts,
    /// Case is ignored, for the error's suggestions only: `UART_*` matches
    /// `uart_putc`.
    Ignored,
}

/// How a pattern matched an item: the first of these that applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(in crate::commands) enum MatchedBy {
    /// The item's name, or one of its paths, which are its texts of the
    /// kind [`TextKind::Path`]: `app::uart::*` matches `app::uart::putc`
    /// and `<app::uart::Port>::putc`.
    Name,
    /// Only an end of a longer path of the item's name, a text of the kind
    /// [`TextKind::End`]: `uart::*` matches `app::uart::putc` by
    /// `uart::putc`.
    End,
    /// Only another of the item's names, which isn't a mangled symbol:
    /// `CAN1_*` matches `Default_Handler` by its alias
    /// `CAN1_RX0_IRQHandler`.
    Alias,
}

/// An item that a pattern matches, and how.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::commands) struct Match {
    /// The item's index in `Binary::items`.
    pub(in crate::commands) item: usize,
    /// Whether the pattern matched the item's name, an end of it, or an
    /// alias.
    pub(in crate::commands) matched_by: MatchedBy,
    /// The longest of the texts of that kind that the pattern matched, and
    /// of those as long, the one that sorts first.
    pub(in crate::commands) matched_as: String,
}

impl Names<'_> {
    /// The items that `pattern` matches, in address order.
    ///
    /// It reads only the items, but it is a method of `Names` so that a
    /// command asks one `Names` what each of its arguments finds, a name or
    /// a pattern alike.
    pub(in crate::commands) fn matching(&self, pattern: &Pattern) -> Vec<Match> {
        self.binary
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| pattern.match_item(index, item))
            .collect()
    }

    /// What `query`, SYMBOL, finds in a command that takes a pattern in its
    /// place: what [`Names::look_up`] finds, or when that is nothing and
    /// `query` has a `*` that stands for any text (see [`Pattern::parse`]),
    /// the items that it matches as a pattern. So a full name with a `*` in
    /// it, as `<*const T as core::fmt::Debug>::fmt`, still finds its item,
    /// and no SYMBOL that finds something changes what it finds.
    pub(in crate::commands) fn look_up_or_match(&self, query: &str) -> Result<LookupOrMatch<'_>> {
        let lookup = self.look_up(query)?;
        let pattern = match lookup {
            Lookup::NoName => Pattern::parse(query),
            Lookup::One(_) | Lookup::Several(_) => None,
        };
        let Some(pattern) = pattern else {
            return Ok(LookupOrMatch::Name(lookup));
        };
        let matches = self.matching(&pattern);
        Ok(match matches.is_empty() {
            true => LookupOrMatch::NoMatch(pattern),
            false => LookupOrMatch::Pattern(PatternMatches { pattern, matches }),
        })
    }
}

/// What SYMBOL finds in a command that takes a pattern in its place (see
/// [`Names::look_up_or_match`]).
pub(in crate::commands) enum LookupOrMatch<'n> {
    /// What it finds as a name: one item or import, several, or for a SYMBOL
    /// that isn't a pattern, nothing.
    Name(Lookup<'n>),
    /// A pattern, with the items that it matches, one or more.
    Pattern(PatternMatches),
    /// A pattern that matches no item.
    NoMatch(Pattern),
}

impl Pattern {
    /// `typed` as a pattern, or `None` when it has no `*` that stands for
    /// any text, so that it is a name.
    pub(in crate::commands) fn parse(typed: &str) -> Option<Pattern> {
        Pattern::parse_with(typed, Case::Counts)
    }

    /// `typed` as a pattern, with case as `case` says.
    fn parse_with(typed: &str, case: Case) -> Option<Pattern> {
        let (from_root, body) = match typed.strip_prefix("::") {
            Some(body) => (true, body),
            None => (false, typed),
        };
        // Taking ASCII letters to lowercase changes no byte offset, nor
        // which `*` is a pointer.
        let body = &case.of(body);
        let wildcards = wildcards(body);
        let (&first, &last) = (wildcards.first()?, wildcards.last()?);
        let prefix = &body[..first];
        let suffix = &body[last + '*'.len_utf8()..];
        let middle = wildcards
            .windows(2)
            .map(|pair| body[pair[0] + '*'.len_utf8()..pair[1]].to_string())
            .collect();
        let quick_check = QuickCheck {
            words: names::words(body).map(str::to_string).collect(),
            first_word: names::words(prefix)
                .next()
                .filter(|word| prefix.starts_with(word))
                .map(str::to_string),
            last_word: names::words(suffix)
                .last()
                .filter(|word| suffix.ends_with(word))
                .map(str::to_string),
        };
        Some(Pattern {
            typed: typed.to_string(),
            from_root,
            prefix: prefix.to_string(),
            middle,
            suffix: suffix.to_string(),
            quick_check,
            case,
        })
    }

    /// The pattern as the user typed it.
    pub(in crate::commands) fn typed(&self) -> &str {
        &self.typed
    }

    /// Whether the pattern matches all of one of the texts of `name`, a
    /// name of an item or an import: the name itself, a path that means the
    /// same, or unless the pattern starts with `::`, an end of one.
    pub(super) fn matches_name(&self, name: &str) -> bool {
        self.longest_texts(name).any()
    }

    /// The other names of `item` that the pattern matches, as an item's
    /// aliases list them, leaving out mangled symbols, which elfq never
    /// prints: for an item that it matches only through them, the names
    /// that it matched.
    pub(super) fn matched_aliases<'i>(&self, item: &'i Item) -> Vec<&'i str> {
        (self.matched_aliases_with_texts(item))
            .map(|matched| matched.alias)
            .collect()
    }

    /// How the pattern matches the item at `index`, if it does: by a path
    /// of its name, else by an end of one, else by a text of another of its
    /// names that isn't a mangled symbol.
    fn match_item(&self, index: usize, item: &Item) -> Option<Match> {
        let found = |matched_by, matched_as| {
            Some(Match {
                item: index,
                matched_by,
                matched_as,
            })
        };
        let own = self.longest_texts(&item.name);
        if let Some(text) = own.path.text {
            return found(MatchedBy::Name, text);
        }
        if let Some(text) = own.end.text {
            return found(MatchedBy::End, text);
        }
        let mut longest = Longest::default();
        for MatchedAlias { texts, .. } in self.matched_aliases_with_texts(item) {
            for text in [texts.path.text, texts.end.text].iter().flatten() {
                longest.offer(text);
            }
        }
        found(MatchedBy::Alias, longest.text?)
    }

    /// Each of `item`'s aliases that the pattern matches and that isn't a
    /// mangled symbol, with the longest texts of it that the pattern
    /// matches.
    fn matched_aliases_with_texts<'i>(
        &self,
        item: &'i Item,
    ) -> impl Iterator<Item = MatchedAlias<'i>> {
        item.aliases.iter().filter_map(|alias| {
            let texts = self.longest_texts(alias);
            // elfq never prints a mangled symbol, so a match by one would
            // surprise. Telling one means demangling it, which takes longer
            // than matching, so it comes last.
            let matched = texts.any() && !names::is_mangled(alias);
            matched.then_some(MatchedAlias { alias, texts })
        })
    }

    /// The longest texts of `name` that the pattern matches, of each kind.
    fn longest_texts(&self, name: &str) -> LongestTexts {
        let mut longest = LongestTexts::default();
        if !self.quick_check.may_match(&self.case.of(name)) {
            return longest;
        }
        // Every other text of a name is shorter than the name, so when the
        // name matches, no other text needs working out.
        if self.matches(name) {
            longest.path.offer(name);
            return longest;
        }
        names::for_each_pattern_text(name, |text, kind| {
            if !self.matches(text) {
                return;
            }
            match kind {
                TextKind::Path => longest.path.offer(text),
                TextKind::End { .. } if self.from_root => {}
                TextKind::End { .. } => longest.end.offer(text),
            }
        });
        longest
    }

    /// Whether the pattern matches all of `text`.
    fn matches(&self, text: &str) -> bool {
        self.middle_at(&self.case.of(text), |_| {})
    }

    /// Whether the pattern matches all of `text`, with case as it is in
    /// `text`: it starts with the prefix, ends with the suffix, and has
    /// each piece of the middle in order between them. Taking each piece at
    /// the first place it is leaves the most room for the rest. Calls
    /// `piece_at` with where each piece of the middle is, as a byte offset.
    fn middle_at(&self, text: &str, mut piece_at: impl FnMut(usize)) -> bool {
        let Some(after_prefix) = text.strip_prefix(self.prefix.as_str()) else {
            return false;
        };
        let mut rest = after_prefix;
        for piece in &self.middle {
            let Some(at) = rest.find(piece.as_str()) else {
                return false;
            };
            piece_at(text.len() - rest.len() + at);
            rest = &rest[at + piece.len()..];
        }
        rest.ends_with(self.suffix.as_str())
    }

    /// The same pattern with case ignored, so that `UART_*` matches
    /// `uart_putc`.
    fn ignoring_case(&self) -> Option<Pattern> {
        Pattern::parse_with(&self.typed, Case::Ignored)
    }

    /// The pattern written as `text`, a text that it matches, writes each
    /// of its pieces: for a pattern that ignores case, `uart_*` for
    /// `UART_*` and `uart_putc`. `None` for a text that it doesn't match.
    fn written_as(&self, text: &str) -> Option<String> {
        let mut starts = Vec::new();
        if !self.middle_at(&self.case.of(text), |at| starts.push(at)) {
            return None;
        }
        let mut written = String::new();
        if self.from_root {
            written.push_str("::");
        }
        written.push_str(&text[..self.prefix.len()]);
        for (piece, start) in self.middle.iter().zip(starts) {
            written.push('*');
            written.push_str(&text[start..start + piece.len()]);
        }
        written.push('*');
        written.push_str(&text[text.len() - self.suffix.len()..]);
        Some(written)
    }
}

impl Case {
    /// `text` as a pattern with this case compares it: as it is, or with
    /// ASCII letters in lowercase.
    fn of(self, text: &str) -> Cow<'_, str> {
        match self {
            Case::Counts => Cow::Borrowed(text),
            Case::Ignored => Cow::Owned(text.to_ascii_lowercase()),
        }
    }
}

impl Names<'_> {
    /// `pattern`, which matches no item, as the file writes it, when only
    /// case keeps it from matching: with each piece between its `*`s as
    /// the text that it matches of the first item it matches with case
    /// ignored writes it, as `uart_*` for `UART_*`. `None` when it matches
    /// no item with case ignored either.
    pub(super) fn in_files_case(&self, pattern: &Pattern) -> Option<String> {
        let ignoring_case = pattern.ignoring_case()?;
        let first = (self.binary.items.iter().enumerate())
            .find_map(|(index, item)| ignoring_case.match_item(index, item))?;
        ignoring_case.written_as(&first.matched_as)
    }
}

/// Where `text` has a `*` that stands for any text, as byte offsets: every
/// `*` but those inside `( )` right after a letter, digit, `_`, `>` or
/// another such `*`, which are pointers, as in `(unsigned char*)` and
/// `(char**)`.
fn wildcards(text: &str) -> Vec<usize> {
    let mut wildcards = Vec::new();
    // How many `(` are open.
    let mut depth = 0usize;
    // Whether the character before can end a type that a pointer's `*`
    // follows.
    let mut after_type = false;
    for (at, c) in text.char_indices() {
        let pointer = c == '*' && depth > 0 && after_type;
        if c == '*' && !pointer {
            wildcards.push(at);
        }
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        after_type = c.is_alphanumeric() || c == '_' || c == '>' || pointer;
    }
    wildcards
}

/// What a name must have for a pattern to match one of its texts, which
/// rules out most names before their texts are worked out. Each word of a
/// text is a word of the name (see [`names::for_each_pattern_text`]), so
/// each word of the pattern is in the name; the word that the pattern
/// starts with, if it starts with one, starts a word of the name, as it
/// starts the text's first word; and the word that the pattern ends with,
/// if it ends with one, ends a word of the name, as it ends the text's last
/// word. A name where taking out generic arguments may join two words, as
/// in `a<b>c`, passes, since its texts can have words that it doesn't.
struct QuickCheck {
    /// The words of the pattern.
    words: Vec<String>,
    /// The word that the pattern starts with, if it starts with one rather
    /// than with `*` or a character that isn't part of a word.
    first_word: Option<String>,
    /// The word that the pattern ends with, if it ends with one.
    last_word: Option<String>,
}

impl QuickCheck {
    /// Whether the pattern may match a text of `name`. When this says no,
    /// it matches none.
    fn may_match(&self, name: &str) -> bool {
        if !names::may_have_words(name, &self.words) {
            return false;
        }
        // Where taking out generic arguments may join two words, a text can
        // start or end with a word that the name doesn't have.
        if names::may_join_words(name) {
            return true;
        }
        (self.first_word.as_deref()).is_none_or(|word| names::starts_a_word(name, word))
            && (self.last_word.as_deref()).is_none_or(|word| names::ends_a_word(name, word))
    }
}

/// Another of an item's names that a pattern matches, with the longest
/// texts of it that it matches.
struct MatchedAlias<'i> {
    alias: &'i str,
    texts: LongestTexts,
}

/// The longest texts of a name that a pattern matches, of each kind.
#[derive(Default)]
struct LongestTexts {
    /// The longest text of the kind [`TextKind::Path`].
    path: Longest,
    /// The longest text of the kind [`TextKind::End`].
    end: Longest,
}

impl LongestTexts {
    /// Whether the pattern matched any text of the name.
    fn any(&self) -> bool {
        self.path.text.is_some() || self.end.text.is_some()
    }
}

/// The longest of the texts offered, and of those as long, the one that
/// sorts first: what a match says it matched as. The order in which the
/// texts come doesn't change it.
#[derive(Default)]
struct Longest {
    text: Option<String>,
}

impl Longest {
    /// Keeps `text` if it is longer than the text kept so far, or as long
    /// and sorts before it.
    fn offer(&mut self, text: &str) {
        let better = match self.text.as_deref() {
            None => true,
            Some(best) => text.len() > best.len() || text.len() == best.len() && text < best,
        };
        if better {
            self.text = Some(text.to_string());
        }
    }
}

#[cfg(test)]
mod tests;
