//! What the patterns given in place of SYMBOL matched, and how an answer
//! about those items words it: the part of its first line that names
//! them, as "the 3 items named on_*", and the notes on the items that a
//! pattern matched only by the end of a longer path, or only through
//! another of their names, since those are the matches to check. A set
//! names its patterns so in its first line (see `set.rs`), and `retained`
//! puts the notes before its others.

use std::cmp::Reverse;
use std::collections::HashSet;

use super::{LeadName, Match, MatchedBy, Pattern};
use crate::binary::Binary;
use crate::binary::names::{self, TextKind};
use crate::commands::text::{self, Naming};

/// A pattern given in place of SYMBOL, and the items it matched.
pub(in crate::commands) struct PatternMatches {
    pub(in crate::commands) pattern: Pattern,
    /// The items it matched, in address order (see `Names::matching`).
    pub(in crate::commands) matches: Vec<Match>,
}

/// The items that `patterns` matched, each once, in address order.
fn matched_items(patterns: &[PatternMatches]) -> Vec<usize> {
    let mut items: Vec<usize> = (patterns.iter())
        .flat_map(|matched| matched.matches.iter().map(|found| found.item))
        .collect();
    items.sort_unstable();
    items.dedup();
    items
}

/// How an answer's first line names the items that `patterns` matched, as
/// one part of what it names, counting every item they matched: "the 3
/// items named on_*", "the 12 items named a*, b* or c*", or for one item,
/// with each pattern that matched it, "on_button (the one item named
/// on_b*)". `None` for more than three patterns that matched several
/// items, which are too many to name there.
pub(super) fn named_by_patterns(binary: &Binary, patterns: &[PatternMatches]) -> Option<LeadName> {
    let typed: Vec<&str> = (patterns.iter())
        .map(|matched| matched.pattern.typed())
        .collect();
    let named = match &typed[..] {
        [] => return None,
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    };
    match matched_items(patterns)[..] {
        [one] => Some(LeadName::name(format!(
            "{} (the one item named {named})",
            binary.items[one].name
        ))),
        _ if typed.len() > 3 => None,
        ref items => Some(LeadName::count(format!(
            "the {} items named {named}",
            items.len()
        ))),
    }
}

/// The notes on how `patterns` matched, which come before an answer's
/// others: for each pattern, the note on the items it matched only by the
/// end of a longer path (see [`ends_note`]), and then one note on the
/// items that the patterns matched only through other names of theirs
/// (see [`aliases_note`]), each naming the names in it as `naming` says.
pub(in crate::commands) fn pattern_notes(
    binary: &Binary,
    patterns: &[PatternMatches],
    naming: &Naming,
) -> Vec<String> {
    let several = match patterns {
        [_] => Patterns::One,
        _ => Patterns::Several,
    };
    (patterns.iter())
        .filter_map(|matched| ends_note(binary, matched, several, naming))
        .chain(aliases_note(binary, patterns, naming))
        .collect()
}

/// Whether one pattern was given or several, which changes how the note
/// on ends names the pattern.
#[derive(Clone, Copy)]
enum Patterns {
    One,
    Several,
}

/// The note on the items that `matched`'s pattern matched only by the end
/// of a longer path, when it also matched some by their names: "16 of the
/// 119 match only by the end of a longer path, under
/// std::backtrace_rs::symbolize; '::gimli::*' leaves them out." It names
/// the paths they are under as `naming` says, those under the most first.
/// With `several` patterns, the note names the pattern.
///
/// A pattern that matched only by ends was typed as an end, so it needs no
/// note. One that starts with `*` or `::` never matches only by an end:
/// `*` matches the path before the end too, and `::` leaves ends out.
fn ends_note(
    binary: &Binary,
    matched: &PatternMatches,
    several: Patterns,
    naming: &Naming,
) -> Option<String> {
    let by = |by: MatchedBy| (matched.matches.iter()).filter(move |found| found.matched_by == by);
    let ends: Vec<&Match> = by(MatchedBy::End).collect();
    if ends.is_empty() || by(MatchedBy::Name).next().is_none() {
        return None;
    }
    // Each path that they are under, with how many are under it, in the
    // order first seen, which a stable sort keeps among those as many.
    let mut unders: Vec<(String, usize)> = Vec::new();
    for found in &ends {
        let Some(under) = under(&binary.items[found.item].name, &found.matched_as) else {
            continue;
        };
        match unders.iter_mut().find(|(seen, _)| *seen == under) {
            Some((_, count)) => *count += 1,
            None => unders.push((under, 1)),
        }
    }
    unders.sort_by_key(|&(_, count)| Reverse(count));
    let unders: Vec<&str> = unders.iter().map(|(under, _)| under.as_str()).collect();
    let typed = matched.pattern.typed();
    let count = matched.matches.len();
    let which = match (several, ends.len()) {
        (Patterns::One, 1) => format!("1 of the {count} matches"),
        (Patterns::One, ends) => format!("{ends} of the {count} match"),
        (Patterns::Several, ends) => format!("{typed} matches {ends} of its {count}"),
    };
    let them = match ends.len() {
        1 => "it",
        _ => "them",
    };
    Some(format!(
        "{which} only by the end of a longer path, under {}; {} leaves {them} out.",
        naming.phrase(&unders),
        text::shell_quote(&format!("::{typed}")),
    ))
}

/// The parts of the path before `end`, a text of `name` that is the end
/// of a longer path, as the note on ends names them:
/// `std::backtrace_rs::symbolize` for `gimli::Cache::new` of
/// `<std::backtrace_rs::symbolize::gimli::Cache>::new`. Of the paths that
/// `end` ends, the first one that [`names::for_each_pattern_text`] visits
/// counts, which is the name as elfq prints it before the paths that mean
/// the same: so a method's end in an impl block is under the block as
/// written, as `_strip_prefix` of `<std::path::Path>::_strip_prefix` is
/// under `<std::path::Path>`. `None` when `end` isn't such a text of
/// `name`.
fn under(name: &str, end: &str) -> Option<String> {
    let mut found = None;
    names::for_each_pattern_text(name, |text, kind| {
        if let TextKind::End { under } = kind
            && found.is_none()
            && text == end
        {
            found = Some(under.to_string());
        }
    });
    found
}

/// The note on the items that `patterns` matched only through other names
/// of theirs, which no pattern matched by their own names, as handlers that
/// share one function are: "Default_Handler matches only through other
/// names: CAN1_RX0_IRQHandler, CAN1_RX1_IRQHandler, CAN1_SCE_IRQHandler and
/// 70 more.", or for several items, "These match only through other names:
/// __adddf3 (as __aeabi_dadd) and __aeabi_dsub (as __subdf3)." Each item's
/// names are those that matched, as its aliases list them, in a short list
/// (see [`Naming::few`]); the items are named as `naming` says. `None` when
/// there are no such items.
fn aliases_note(binary: &Binary, patterns: &[PatternMatches], naming: &Naming) -> Option<String> {
    let matches = || patterns.iter().flat_map(|matched| &matched.matches);
    let by_own_name: HashSet<usize> = (matches())
        .filter(|found| found.matched_by != MatchedBy::Alias)
        .map(|found| found.item)
        .collect();
    let mut items: Vec<usize> = (matches())
        .filter(|found| found.matched_by == MatchedBy::Alias && !by_own_name.contains(&found.item))
        .map(|found| found.item)
        .collect();
    items.sort_unstable();
    items.dedup();
    // Each item, with the names that matched.
    let items: Vec<(&str, Vec<&str>)> = (items.iter())
        .map(|&index| {
            let item = &binary.items[index];
            let matched: Vec<&str> = (patterns.iter())
                .flat_map(|matched| matched.pattern.matched_aliases(item))
                .collect();
            let names = (item.aliases.iter())
                .map(String::as_str)
                .filter(|alias| matched.contains(alias))
                .collect();
            (item.name.as_str(), names)
        })
        .collect();
    let names_of_one = naming.few();
    match &items[..] {
        [] => None,
        [(item, names)] => Some(match &names[..] {
            [one] => format!("{item} matches only through another of its names, {one}."),
            names => format!(
                "{item} matches only through other names: {}.",
                names_of_one.phrase(names)
            ),
        }),
        items => {
            let items: Vec<String> = (items.iter())
                .map(|(item, names)| format!("{item} (as {})", names_of_one.phrase(names)))
                .collect();
            let items: Vec<&str> = items.iter().map(String::as_str).collect();
            Some(format!(
                "These match only through other names: {}.",
                naming.phrase(&items)
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::symbol::{Names, fixtures};

    /// What each of `patterns` matches in `binary`, in the order given.
    fn matched(binary: &Binary, patterns: &[&str]) -> Vec<PatternMatches> {
        let names = Names::new(binary);
        (patterns.iter())
            .map(|typed| {
                let pattern = Pattern::parse(typed).expect("a pattern");
                let matches = names.matching(&pattern);
                PatternMatches { pattern, matches }
            })
            .collect()
    }

    #[test]
    fn the_first_line_names_the_patterns_and_how_many_items_they_match() {
        let binary = fixtures::functions(&["on_button", "on_timer", "on_data", "report"]);
        let named = |patterns: &[&str]| {
            named_by_patterns(&binary, &matched(&binary, patterns)).map(|named| named.to_string())
        };
        assert_eq!(named(&["on_*"]).unwrap(), "the 3 items named on_*");
        assert_eq!(
            named(&["on_b*"]).unwrap(),
            "on_button (the one item named on_b*)"
        );
        // An item that two match counts once.
        assert_eq!(
            named(&["on_b*", "*_button"]).unwrap(),
            "on_button (the one item named on_b* or *_button)"
        );
        assert_eq!(
            named(&["on_t*", "on_d*", "rep*"]).unwrap(),
            "the 3 items named on_t*, on_d* or rep*"
        );
        // Four are too many to name with what they matched, but not with
        // the one item that they matched.
        assert_eq!(named(&["on_t*", "on_d*", "rep*", "on_b*"]), None);
        assert_eq!(
            named(&["on_b*", "on_bu*", "on_but*", "*_button"]).unwrap(),
            "on_button (the one item named on_b*, on_bu*, on_but* or *_button)"
        );
    }

    #[test]
    fn a_note_says_where_the_items_matched_by_an_end_are() {
        let binary = fixtures::functions(&[
            "uart::init",
            "app::uart::putc",
            "drv::uart::putc",
            "drv::uart::getc",
            "uart::read",
        ]);
        let notes = |patterns: &[&str]| {
            pattern_notes(&binary, &matched(&binary, patterns), &Naming::of(false))
        };
        // The paths they are under, those under the most first.
        assert_eq!(
            notes(&["uart::*"]),
            [
                "3 of the 5 match only by the end of a longer path, under drv and app; \
                 '::uart::*' leaves them out."
            ]
        );
        // A pattern that matches only by ends was typed as one, and needs no
        // note.
        assert!(notes(&["uart::p*"]).is_empty());
        // With several patterns, the note names its own.
        assert_eq!(
            notes(&["uart::*", "uart::p*"]),
            [
                "uart::* matches 3 of its 5 only by the end of a longer path, under drv and app; \
                 '::uart::*' leaves them out."
            ]
        );
        let binary = fixtures::functions(&["uart::init", "app::uart::putc"]);
        assert_eq!(
            pattern_notes(&binary, &matched(&binary, &["uart::*"]), &Naming::of(false)),
            [
                "1 of the 2 matches only by the end of a longer path, under app; '::uart::*' \
                 leaves it out."
            ]
        );
    }

    #[test]
    fn a_note_names_the_items_matched_only_through_other_names() {
        let mut binary =
            fixtures::functions(&["Default_Handler", "__adddf3", "__aeabi_f2d", "main"]);
        let handlers = [
            "CAN1_RX0_IRQHandler",
            "CAN1_RX1_IRQHandler",
            "CAN1_SCE_IRQHandler",
        ];
        let aliases: [&[&str]; 3] = [
            &[
                &handlers[..],
                &[
                    "CAN1_TX_IRQHandler",
                    "CAN2_RX0_IRQHandler",
                    "USART1_IRQHandler",
                ],
            ]
            .concat(),
            &["__aeabi_dadd"],
            &["__extendsfdf2"],
        ];
        for (item, aliases) in binary.items.iter_mut().zip(aliases) {
            item.aliases = aliases.iter().map(|alias| alias.to_string()).collect();
        }
        let notes = |patterns: &[&str], all| {
            pattern_notes(&binary, &matched(&binary, patterns), &Naming::of(all))
        };
        assert_eq!(
            notes(&["__aeabi_d*"], false),
            ["__adddf3 matches only through another of its names, __aeabi_dadd."]
        );
        // Up to 3 of an item's names, or 4 when that is all, in the order of
        // its aliases; or with `all`, every one.
        assert_eq!(
            notes(&["CAN1_*"], false),
            [
                "Default_Handler matches only through other names: CAN1_RX0_IRQHandler, \
                 CAN1_RX1_IRQHandler, CAN1_SCE_IRQHandler and CAN1_TX_IRQHandler."
            ]
        );
        assert_eq!(
            notes(&["CAN*"], false),
            [
                "Default_Handler matches only through other names: CAN1_RX0_IRQHandler, \
                 CAN1_RX1_IRQHandler, CAN1_SCE_IRQHandler and 2 more."
            ]
        );
        assert_eq!(
            notes(&["CAN*"], true),
            [
                "Default_Handler matches only through other names: CAN1_RX0_IRQHandler, \
                 CAN1_RX1_IRQHandler, CAN1_SCE_IRQHandler, CAN1_TX_IRQHandler and \
                 CAN2_RX0_IRQHandler."
            ]
        );
        // Several items, each with the names that matched.
        assert_eq!(
            notes(&["__aeabi_d*", "*df2"], false),
            [
                "These match only through other names: __adddf3 (as __aeabi_dadd) and \
                 __aeabi_f2d (as __extendsfdf2)."
            ]
        );
        // An item that a pattern matched by its own name needs no note.
        assert!(notes(&["__aeabi_d*", "__add*"], false).is_empty());
    }
}
