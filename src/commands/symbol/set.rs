//! A set: the items that several SYMBOLs, patterns and ranges given in
//! place of SYMBOL pick together, as `retained`, `callers` and `callees`
//! take them, and how an answer's first line names them.
//!
//! Each argument picks items ([`Pick`]): a SYMBOL its item, a pattern every
//! item it matches, and a range every item with a byte in it, both in
//! address order. [`Set::new`] puts them together in the order given, each
//! item once, where it was first picked, so that an item given by a name
//! and by an address, or that two patterns match, counts once.
//!
//! The first line names the set by what picked its items
//! ([`Set::lead_name`]):
//!
//! - SYMBOLs, alone or with ranges: "crc32", "crc32 and report", or from
//!   three items on, "these 3 items", so that it stays short however many
//!   there are;
//! - ranges alone: by the ranges, as `callers` names them, "the 6 items in
//!   0x00000000..0x000000e0 (.itcm)", since the user named no item;
//! - a pattern among them, or a range that holds only code, as in a chip's
//!   ROM, which no item names: each part of what was given in turn, a
//!   [`Named`], up to three, as "the 3 items named on_* and process_frame"
//!   or "ram_calls_rom and 0x1fff0000..0x20000000".
//!
//! Those words are a [`LeadName`], which says whether they start with a
//! name or with elfq's own words, which take a capital letter when the
//! sentence starts with them, as in `callers`' "The 3 items named on_* are
//! used by".

use std::collections::{HashMap, HashSet};
use std::fmt;

use super::PatternMatches;
use super::matched::named_by_patterns;
use crate::binary::{AddressRange, Binary};
use crate::commands::text;

/// What one argument picks, as a set takes it: the item that a SYMBOL
/// names, or the items of a pattern or a range given in its place.
pub(in crate::commands) enum Pick {
    /// The item that a SYMBOL names, as an index into [`Binary::items`],
    /// with its name as the answer's first line gives it (see
    /// [`Symbol::lead_name`]).
    ///
    /// [`Symbol::lead_name`]: super::Symbol::lead_name
    Symbol { index: usize, lead_name: String },
    /// A pattern, with every item it matches, in address order.
    Pattern(PatternMatches),
    /// A range of memory, which picks every item with a byte in it.
    Range(AddressRange),
}

/// An item of a set.
pub(in crate::commands) struct Found {
    /// The item, as an index into [`Binary::items`].
    pub(in crate::commands) index: usize,
    /// The item's name as the answer gives it: for an item that a SYMBOL
    /// picked first, as [`Symbol::lead_name`] gives it, and for one that a
    /// pattern or range picked first, its name.
    ///
    /// [`Symbol::lead_name`]: super::Symbol::lead_name
    pub(in crate::commands) lead_name: String,
    /// Whether a SYMBOL picked it, and not only a pattern or range.
    /// `retained`'s note on the items that add nothing names only those,
    /// since the user didn't name the others one by one.
    pub(in crate::commands) named: bool,
}

/// The items that some SYMBOLs, patterns and ranges pick together, each
/// once, and what the first line names, part by part.
pub(in crate::commands) struct Set {
    /// The items picked, each once, in the order given, and a pattern's or
    /// a range's in address order.
    items: Vec<Found>,
    /// The patterns given, each once, in the order given, with every item
    /// they matched, even one that [`Set::leave_out_unknown_ends`] took out
    /// of `items`.
    patterns: Vec<PatternMatches>,
    /// What was given, part by part, in the order given, as the first line
    /// names it.
    parts: Vec<Named>,
}

/// A part of what was given, as a set's first line names it: one SYMBOL,
/// the ranges or the patterns.
enum Named {
    /// A SYMBOL: its item, as an index into [`Binary::items`], and the name
    /// that the answer gives it.
    Symbol { index: usize, lead_name: String },
    /// The ranges given in place of SYMBOL, each once, in the order given,
    /// which the first line names together, by the items in them, or by
    /// their addresses when they hold only code.
    Ranges(Vec<AddressRange>),
    /// The patterns given in place of SYMBOL, which the first line names
    /// together, by the items they matched.
    Patterns,
}

impl Set {
    /// The set of the items that `picks` pick, in the order given. An item
    /// picked again counts once, where it was first picked, and counts as
    /// named once a SYMBOL picks it. A pattern given again picks nothing
    /// more.
    pub(in crate::commands) fn new(binary: &Binary, picks: impl IntoIterator<Item = Pick>) -> Set {
        let mut set = Set {
            items: Vec::new(),
            patterns: Vec::new(),
            parts: Vec::new(),
        };
        // Where each item is in `items`.
        let mut places: HashMap<usize, usize> = HashMap::new();
        for pick in picks {
            for found in set.add_part(binary, pick) {
                // An item picked again stays where it was first picked.
                match places.get(&found.index) {
                    Some(&place) if found.named => set.items[place].named = true,
                    Some(_) => {}
                    None => {
                        places.insert(found.index, set.items.len());
                        set.items.push(found);
                    }
                }
            }
        }
        set
    }

    /// Adds what `pick` is to the parts that the first line names, and
    /// gives the items it picks, some of which the set may hold already.
    fn add_part(&mut self, binary: &Binary, pick: Pick) -> Vec<Found> {
        match pick {
            Pick::Symbol { index, lead_name } => {
                // Each item that SYMBOLs name has one part.
                let named_before = (self.parts.iter()).any(|part| match part {
                    Named::Symbol { index: earlier, .. } => *earlier == index,
                    Named::Ranges(_) | Named::Patterns => false,
                });
                if !named_before {
                    self.parts.push(Named::Symbol {
                        index,
                        lead_name: lead_name.clone(),
                    });
                }
                vec![Found {
                    index,
                    lead_name,
                    named: true,
                }]
            }
            Pick::Pattern(matched) => {
                if !(self.parts.iter()).any(|part| matches!(part, Named::Patterns)) {
                    self.parts.push(Named::Patterns);
                }
                let typed = matched.pattern.typed();
                if (self.patterns.iter()).any(|seen| seen.pattern.typed() == typed) {
                    return Vec::new();
                }
                let items = (matched.matches.iter())
                    .map(|found| Found {
                        index: found.item,
                        lead_name: binary.items[found.item].name.clone(),
                        named: false,
                    })
                    .collect();
                self.patterns.push(matched);
                items
            }
            Pick::Range(range) => {
                self.add_range(range);
                items_in(binary, range)
            }
        }
    }

    /// Adds `range` to the ranges that the first line names, once.
    fn add_range(&mut self, range: AddressRange) {
        let ranges = self.parts.iter_mut().find_map(|part| match part {
            Named::Ranges(ranges) => Some(ranges),
            Named::Symbol { .. } | Named::Patterns => None,
        });
        match ranges {
            Some(ranges) if !ranges.contains(&range) => ranges.push(range),
            Some(_) => {}
            None => self.parts.push(Named::Ranges(vec![range])),
        }
    }

    /// Takes the functions whose end elfq doesn't know out of the set's
    /// items, as `retained` leaves them out, since it can't read their
    /// code, and gives them, as indexes into [`Binary::items`], in the
    /// order given. The first line then names only the items left, but
    /// still counts a pattern's items by every item it matched (see
    /// [`Set::lead_name`]).
    pub(in crate::commands) fn leave_out_unknown_ends(&mut self, binary: &Binary) -> Vec<usize> {
        let (left_out, kept): (Vec<Found>, Vec<Found>) = (std::mem::take(&mut self.items))
            .into_iter()
            .partition(|found| binary.items[found.index].end_unknown());
        self.items = kept;
        left_out.into_iter().map(|found| found.index).collect()
    }

    /// The items picked, each once, in the order given, and a pattern's or
    /// a range's in address order.
    pub(in crate::commands) fn items(&self) -> &[Found] {
        &self.items
    }

    /// The patterns given, each once, in the order given, with every item
    /// they matched, or `None` when no pattern was given.
    pub(in crate::commands) fn patterns(&self) -> Option<&[PatternMatches]> {
        (!self.patterns.is_empty()).then_some(&self.patterns[..])
    }

    /// The ranges given, each once, in the order given, when only ranges
    /// were given, so that the user named none of the items.
    pub(in crate::commands) fn ranges_alone(&self) -> Option<&[AddressRange]> {
        match &self.parts[..] {
            [Named::Ranges(ranges)] => Some(ranges),
            _ => None,
        }
    }

    /// The ranges given, each once, in the order given, among whatever else
    /// was given; none when no range was.
    pub(in crate::commands) fn ranges(&self) -> &[AddressRange] {
        (self.parts.iter())
            .find_map(|part| match part {
                Named::Ranges(ranges) => Some(&ranges[..]),
                Named::Symbol { .. } | Named::Patterns => None,
            })
            .unwrap_or_default()
    }

    /// The items that the SYMBOLs given name, as indexes into
    /// [`Binary::items`], each once, in the order given; none when no
    /// SYMBOL was given.
    pub(in crate::commands) fn symbols(&self) -> Vec<usize> {
        (self.parts.iter())
            .filter_map(|part| match part {
                Named::Symbol { index, .. } => Some(*index),
                Named::Ranges(_) | Named::Patterns => None,
            })
            .collect()
    }

    /// Whether the set holds the item at `index`, an index into
    /// [`Binary::items`].
    fn holds(&self, index: usize) -> bool {
        self.items.iter().any(|found| found.index == index)
    }

    /// Whether a range given holds no item at all, only code outside every
    /// item that the file calls or jumps to, as in a chip's ROM. `callers`
    /// takes such a range; `retained` refuses it, so a range of its set
    /// always holds an item.
    pub(in crate::commands) fn has_range_of_code_only(&self, binary: &Binary) -> bool {
        let holds_an_item =
            |range: AddressRange| (binary.items.iter()).any(|item| item.is_in(&[range]));
        self.ranges().iter().any(|&range| !holds_an_item(range))
    }

    /// How the answer's first line names the set's items, by what picked
    /// them: by the ranges when only ranges were given (see [`in_ranges`]);
    /// part by part when a pattern was given, or a range that holds only
    /// code, which no item names (see [`Set::by_parts`]); and otherwise by
    /// their names, "a" or "a and b", or from three on, by how many, "these
    /// 6 items", so that it stays short however many there are. With
    /// `all`, it names every range and section.
    pub(in crate::commands) fn lead_name(&self, binary: &Binary, all: bool) -> LeadName {
        let names: Vec<&str> = (self.items.iter())
            .map(|found| found.lead_name.as_str())
            .collect();
        if let Some(ranges) = self.ranges_alone() {
            return in_ranges(binary, ranges, &names, all);
        }
        if !self.patterns.is_empty() || self.has_range_of_code_only(binary) {
            return self.by_parts(binary, all);
        }
        match &names[..] {
            [one] => LeadName::name(one.to_string()),
            [first, second] => LeadName::name(format!("{first} and {second}")),
            _ => LeadName::count(format!("these {} items", names.len())),
        }
    }

    /// How the first line names the set's items when a pattern is among
    /// what was given, or a range that holds only code: each part, in the
    /// order given, as "a", "a and b" or "a, b and c", with a SYMBOL by its
    /// name, the ranges as [`in_ranges`] names them, or by their addresses
    /// when they hold only code, as in a chip's ROM, and the patterns as
    /// [`named_by_patterns`] does, by every item they matched, as in "the 3
    /// items named on_* and process_frame". An item that two parts pick is
    /// named by each, as in "on_button (the one item named on_b*) and
    /// on_button". A SYMBOL or ranges whose items are no longer in the set,
    /// since `retained` left them out, aren't named. With more than three
    /// parts, or patterns, it counts the items, "these 6 items", a
    /// pattern's by what it matched, or names the one.
    fn by_parts(&self, binary: &Binary, all: bool) -> LeadName {
        // The items that the parts name: those in the set, and every item
        // that the patterns matched, which they count even when `retained`
        // leaves it out.
        let named_items: HashSet<usize> = (self.items.iter().map(|found| found.index))
            .chain(
                (self.patterns.iter())
                    .flat_map(|matched| matched.matches.iter().map(|found| found.item)),
            )
            .collect();
        let these = match &self.items[..] {
            [one] if named_items.len() == 1 => LeadName::name(one.lead_name.clone()),
            _ => LeadName::count(format!("these {} items", named_items.len())),
        };
        let mut part_names: Vec<LeadName> = Vec::new();
        for part in &self.parts {
            let words_for_part = match part {
                Named::Symbol { index, .. } if !self.holds(*index) => continue,
                Named::Symbol { lead_name, .. } => LeadName::name(lead_name.clone()),
                Named::Ranges(ranges) => {
                    let names: Vec<&str> = (self.items.iter())
                        .map(|found| &binary.items[found.index])
                        .filter(|item| item.is_in(ranges))
                        .map(|item| item.name.as_str())
                        .collect();
                    if !names.is_empty() {
                        in_ranges(binary, ranges, &names, all)
                    } else if (binary.items.iter()).any(|item| item.is_in(ranges)) {
                        // `retained` left out every item in them.
                        continue;
                    } else {
                        // Code only, as in a chip's ROM, which `callers`
                        // takes: no item names it, so its addresses do, as
                        // for ranges alone.
                        LeadName::name(text::ranges_text(binary, ranges, all))
                    }
                }
                Named::Patterns => match named_by_patterns(binary, &self.patterns) {
                    Some(named) => named,
                    None => return these,
                },
            };
            part_names.push(words_for_part);
        }
        match &part_names[..] {
            [first, ..] if part_names.len() <= 3 => {
                let words: Vec<&str> = (part_names.iter())
                    .map(|part| part.words.as_str())
                    .collect();
                LeadName {
                    words: text::names_text(&words, true),
                    opening: first.opening,
                }
            }
            _ => these,
        }
    }
}

/// How an answer's first line names what it is about, as [`Set::lead_name`]
/// words a set: words that start with a name, as in "crc32 and report", or
/// with elfq's own words, which count the items, as in "the 3 items named
/// on_*". Written as it is, it reads in the middle of a sentence;
/// [`LeadName::at_start`] gives it at the start of one.
pub(in crate::commands) struct LeadName {
    /// The words, as in the middle of a sentence.
    words: String,
    opening: Opening,
}

/// What the words of a [`LeadName`] start with.
#[derive(Clone, Copy)]
enum Opening {
    /// A name or a range, as "crc32" or "0x00000000..0x00040000 (.itcm)",
    /// which keeps its case at the start of a sentence.
    Name,
    /// elfq's own words, which count the items, as "the 3 items named on_*"
    /// or "these 6 items", and take a capital letter there.
    Count,
}

impl LeadName {
    /// Words that start with a name or a range, such as SYMBOL's name as
    /// [`Symbol::lead_name`] gives it.
    ///
    /// [`Symbol::lead_name`]: super::Symbol::lead_name
    pub(in crate::commands) fn name(words: String) -> LeadName {
        LeadName {
            words,
            opening: Opening::Name,
        }
    }

    /// elfq's own words, which count the items, as "the 3 items named
    /// on_*".
    pub(super) fn count(words: String) -> LeadName {
        LeadName {
            words,
            opening: Opening::Count,
        }
    }

    /// The words at the start of a sentence, where elfq's own take a
    /// capital letter: "The 3 items named on_*", but "crc32 and report".
    pub(in crate::commands) fn at_start(&self) -> String {
        match self.opening {
            Opening::Name => self.words.clone(),
            Opening::Count => {
                let mut letters = self.words.chars();
                (letters.next().into_iter())
                    .flat_map(char::to_uppercase)
                    .chain(letters)
                    .collect()
            }
        }
    }
}

impl fmt::Display for LeadName {
    /// The words, as in the middle of a sentence.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.words)
    }
}

/// The items with a byte in `range`, in address order, each with its name.
fn items_in(binary: &Binary, range: AddressRange) -> Vec<Found> {
    (binary.items.iter().enumerate())
        .filter(|(_, item)| item.is_in(&[range]))
        .map(|(index, item)| Found {
            index,
            lead_name: item.name.clone(),
            named: false,
        })
        .collect()
}

/// How the first line names the items called `names` in `ranges`, as
/// `callers` names ranges: by the ranges, each with the sections in it by
/// where they run, every one with `all`, as in "the 6 items in
/// 0x00000000..0x000000e0 (.itcm)". The one item of ranges keeps its name,
/// with the ranges as a note, as an address given as SYMBOL does in "crc32
/// (which holds 0x14)": "SysTick_Handler (the one item in
/// 0x00000000..0x00000010)".
fn in_ranges(binary: &Binary, ranges: &[AddressRange], names: &[&str], all: bool) -> LeadName {
    match names {
        [one] => {
            let ranges: Vec<String> = ranges
                .iter()
                .map(|&range| text::range_argument(range))
                .collect();
            let ranges: Vec<&str> = ranges.iter().map(String::as_str).collect();
            LeadName::name(format!(
                "{one} (the one item in {})",
                text::names_text(&ranges, all)
            ))
        }
        _ => {
            let ranges = text::ranges_text(binary, ranges, all);
            LeadName::count(format!("the {} items in {ranges}", names.len()))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::commands::symbol::{Names, Pattern, fixtures};

    /// The Cortex-M7 sample, with code in ITCM at 0x0 and flash at
    /// 0x30000000.
    fn sample() -> Binary {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/bin/c-cortex-m7.elf");
        Binary::load(&path).unwrap()
    }

    /// What a SYMBOL that names the item called `name` picks.
    fn symbol(binary: &Binary, name: &str) -> Pick {
        let index = (binary.items.iter())
            .position(|item| item.name == name)
            .unwrap();
        Pick::Symbol {
            index,
            lead_name: name.to_string(),
        }
    }

    /// What `typed`, a pattern, picks.
    fn pattern(binary: &Binary, typed: &str) -> Pick {
        let pattern = Pattern::parse(typed).expect("a pattern");
        let matches = Names::new(binary).matching(&pattern);
        Pick::Pattern(PatternMatches { pattern, matches })
    }

    /// What the range `start..end` picks.
    fn range(start: u64, end: u64) -> Pick {
        Pick::Range(AddressRange { start, end })
    }

    /// The names of the set's items, in its order.
    fn names(set: &Set) -> Vec<&str> {
        (set.items().iter())
            .map(|found| found.lead_name.as_str())
            .collect()
    }

    #[test]
    fn the_lead_names_few_symbols_counts_more_and_names_ranges() {
        let binary = sample();
        let lead = |picks: Vec<Pick>| {
            Set::new(&binary, picks)
                .lead_name(&binary, false)
                .to_string()
        };
        assert_eq!(lead(vec![symbol(&binary, "crc32")]), "crc32");
        assert_eq!(
            lead(vec![
                symbol(&binary, "process_frame"),
                symbol(&binary, "report")
            ]),
            "process_frame and report"
        );
        // __app_tick_veneer is the one item in 0xd0..0xd8.
        assert_eq!(
            lead(vec![
                symbol(&binary, "crc32"),
                symbol(&binary, "apply_filter"),
                range(0xd0, 0xd8)
            ]),
            "these 3 items"
        );
        // With only ranges, the ranges, with the sections in them; for one
        // item, its name, with the ranges as a note.
        assert_eq!(
            lead(vec![range(0x0, 0x10), range(0xd0, 0xd8)]),
            "the 2 items in 0x00000000..0x00000010 (.itcm) and 0x000000d0..0x000000d8 (.itcm)"
        );
        assert_eq!(
            lead(vec![range(0x0, 0x10), range(0x8, 0x10)]),
            "SysTick_Handler (the one item in 0x00000000..0x00000010 and 0x00000008..0x00000010)"
        );
    }

    #[test]
    fn with_a_pattern_the_lead_names_each_part_in_the_order_given() {
        let binary = sample();
        let lead = |picks: Vec<Pick>| {
            Set::new(&binary, picks)
                .lead_name(&binary, false)
                .to_string()
        };
        assert_eq!(
            lead(vec![pattern(&binary, "on_*"), symbol(&binary, "crc32")]),
            "the 3 items named on_* and crc32"
        );
        // The ranges are one part, where the first was given, named by the
        // items in them.
        assert_eq!(
            lead(vec![
                range(0x0, 0x10),
                pattern(&binary, "on_b*"),
                range(0xd0, 0xd8)
            ]),
            "the 2 items in 0x00000000..0x00000010 (.itcm) and 0x000000d0..0x000000d8 (.itcm) \
             and on_button (the one item named on_b*)"
        );
        // An item that two parts pick is named by each.
        assert_eq!(
            lead(vec![
                pattern(&binary, "on_b*"),
                symbol(&binary, "on_button")
            ]),
            "on_button (the one item named on_b*) and on_button"
        );
        // Past three parts, the items are counted.
        assert_eq!(
            lead(vec![
                pattern(&binary, "on_*"),
                symbol(&binary, "crc32"),
                symbol(&binary, "report"),
                symbol(&binary, "main")
            ]),
            "these 6 items"
        );
    }

    #[test]
    fn an_item_picked_twice_counts_once_where_it_was_first_picked() {
        let binary = sample();
        let set = Set::new(
            &binary,
            vec![
                pattern(&binary, "on_*"),
                symbol(&binary, "on_data"),
                pattern(&binary, "on_*"),
                symbol(&binary, "crc32"),
                symbol(&binary, "crc32"),
            ],
        );
        assert_eq!(names(&set), ["on_timer", "on_data", "on_button", "crc32"]);
        // A SYMBOL that picks an item that a pattern picked first names it.
        let named: Vec<bool> = set.items().iter().map(|found| found.named).collect();
        assert_eq!(named, [false, true, false, true]);
        // The pattern given twice is one.
        assert_eq!(set.patterns().map(<[PatternMatches]>::len), Some(1));
        assert_eq!(set.ranges_alone(), None);
        let ranges = Set::new(&binary, vec![range(0x0, 0x10), range(0x0, 0x10)]);
        assert_eq!(names(&ranges), ["SysTick_Handler"]);
        assert_eq!(
            ranges.ranges_alone(),
            Some(
                &[AddressRange {
                    start: 0,
                    end: 0x10
                }][..]
            )
        );
    }

    #[test]
    fn a_set_gives_its_ranges_and_symbols_each_once_in_the_order_given() {
        let binary = sample();
        let index = |name: &str| (binary.items.iter()).position(|item| item.name == name);
        let set = Set::new(
            &binary,
            vec![
                range(0xd0, 0xd8),
                symbol(&binary, "crc32"),
                pattern(&binary, "on_*"),
                range(0x0, 0x10),
                symbol(&binary, "report"),
                range(0xd0, 0xd8),
                symbol(&binary, "crc32"),
            ],
        );
        assert_eq!(
            set.ranges(),
            [
                AddressRange {
                    start: 0xd0,
                    end: 0xd8
                },
                AddressRange {
                    start: 0x0,
                    end: 0x10
                }
            ]
        );
        assert_eq!(
            set.symbols(),
            [index("crc32").unwrap(), index("report").unwrap()]
        );
        // A pattern alone gives neither.
        let set = Set::new(&binary, vec![pattern(&binary, "on_*")]);
        assert!(set.ranges().is_empty());
        assert!(set.symbols().is_empty());
    }

    #[test]
    fn a_lead_takes_a_capital_letter_only_where_elfq_words_start_it() {
        let binary = sample();
        let at_start = |picks: Vec<Pick>| {
            Set::new(&binary, picks)
                .lead_name(&binary, false)
                .at_start()
        };
        assert_eq!(
            at_start(vec![pattern(&binary, "on_*"), symbol(&binary, "crc32")]),
            "The 3 items named on_* and crc32"
        );
        assert_eq!(
            at_start(vec![range(0x0, 0x10), range(0xd0, 0xd8)]),
            "The 2 items in 0x00000000..0x00000010 (.itcm) and 0x000000d0..0x000000d8 (.itcm)"
        );
        assert_eq!(
            at_start(vec![
                symbol(&binary, "crc32"),
                symbol(&binary, "report"),
                symbol(&binary, "main")
            ]),
            "These 3 items"
        );
        assert_eq!(
            at_start(vec![symbol(&binary, "crc32"), pattern(&binary, "on_*")]),
            "crc32 and the 3 items named on_*"
        );
        assert_eq!(
            at_start(vec![pattern(&binary, "crc3*")]),
            "crc32 (the one item named crc3*)"
        );
        // A name keeps its case, even one that reads as elfq's own words.
        let binary = fixtures::functions(&["these", "the"]);
        let lead = Set::new(
            &binary,
            vec![symbol(&binary, "these"), symbol(&binary, "the")],
        )
        .lead_name(&binary, false);
        assert_eq!(lead.at_start(), "these and the");
        assert_eq!(lead.to_string(), "these and the");
    }
}
