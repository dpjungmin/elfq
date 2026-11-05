//! The answer about several SYMBOLs at once: what removing them all would
//! save.
//!
//! One search from where the program starts, which never enters a SYMBOL,
//! finds what goes (see `graph::Removal`): whole branches of the tree,
//! whose tops are SYMBOLs, and items that several SYMBOLs keep together
//! and no one of them alone. The table shows the trees under those tops:
//! the 30 largest items of all that goes, the tops among them. The row of
//! an item kept together names the SYMBOLs it needs, as in "stats, kept by
//! process_frame and report together": removing just those loses it, and
//! removing all of them but one keeps it.
//!
//! The notes say which SYMBOLs add nothing, since the others keep them,
//! and which one keeps all the rest. Both name only items that a SYMBOL
//! named, since an item that only a range found is one the user didn't
//! name, so neither shows when only ranges were given. Other notes say how
//! many bytes go only when several SYMBOLs go, which SYMBOLs nothing
//! reaches, whose own bytes the total counts, and which functions it left
//! out, since elfq doesn't know where they end. Then come the notes that
//! the answer about one SYMBOL has too.

use std::collections::{HashMap, HashSet};

use super::parts::Parts;
use super::{Found, Given, Symbols, left_out_note, subject, would_save};
use crate::cli::Format;
use crate::commands::retained_tree::RetainedTree;
use crate::commands::text::{self, Naming};
use crate::commands::tree_answer::TreeAnswer;
use crate::graph::Node;

/// What `retained SYMBOL...` writes as text for `symbols`, several, in the
/// tree `retained`: what removing them all at once would save, and the
/// trees of what goes, the largest 30 items or with `all`, every one, in
/// the shape that `format` asks for, then the notes, which name the
/// functions it left out too.
pub(super) fn tree_answer(
    retained: &RetainedTree,
    symbols: &Symbols,
    all: bool,
    format: Format,
) -> TreeAnswer {
    let removed: Vec<usize> = (symbols.found.iter()).map(|found| found.index).collect();
    let several = Several {
        retained,
        found: &symbols.found,
        left_out: &symbols.left_out,
        given: &symbols.given,
        parts: Parts::new(retained, &removed),
    };
    several.tree_answer(all, format)
}

/// The names of the SYMBOLs in `found`, as the answer gives them, in the
/// same order.
fn names<'f>(found: impl IntoIterator<Item = &'f Found>) -> Vec<&'f str> {
    (found.into_iter())
        .map(|found| found.lead_name.as_str())
        .collect()
}

/// Several SYMBOLs, and what removing them at once loses.
struct Several<'f, 't, 'g, 'a> {
    retained: &'t RetainedTree<'g, 'a>,
    /// The items given, by SYMBOL or in a range, each once, in the order
    /// given (see [`Symbols::found`]).
    found: &'f [Found],
    /// The functions given, by SYMBOL or in a range, whose end elfq
    /// doesn't know, which the answer leaves out, as indexes into
    /// [`Binary::items`].
    ///
    /// [`Binary::items`]: crate::binary::Binary::items
    left_out: &'f [usize],
    /// Whether SYMBOLs were given, ranges in their place, or both.
    given: &'f Given,
    parts: Parts<'t, 'g, 'a>,
}

impl Several<'_, '_, '_, '_> {
    /// The answer, with `all`, every row and every name in the notes, in
    /// the shape that `format` asks for. When nothing reaches any SYMBOL,
    /// it says so, and what their own bytes are, and has no notes but the
    /// one on the functions left out. When no row would show, it has no
    /// table.
    fn tree_answer(&self, all: bool, format: Format) -> TreeAnswer {
        let retained = self.retained;
        let naming = Naming::of(all);
        let range = (retained.range).map(|range| text::range_text(retained.binary, range, all));
        let removal = &self.parts.removal;
        if removal.tops().is_empty() {
            let sentence = self.unreachable_answer(range.as_deref(), &naming);
            return TreeAnswer {
                notes: left_out_note(retained.binary, self.left_out, &naming)
                    .into_iter()
                    .collect(),
                ..TreeAnswer::sentences(vec![sentence])
            };
        }
        let subject = subject(retained.binary, &names(self.found), self.given, all);
        let saves = would_save(&subject, removal.retained(), range.as_deref());
        let notes = self.notes(&naming);
        let shown = self.shown(all);
        // With a range, the tops may keep none of its bytes: the SYMBOLs
        // that nothing reaches may have the only ones there, or none has.
        if shown.is_empty() {
            return TreeAnswer {
                notes,
                ..TreeAnswer::sentences(vec![format!("{saves}.")])
            };
        }
        let words = self.kept_together_words(&shown, all);
        TreeAnswer {
            lead: vec![format!("{saves}:")],
            table: Some(retained.table(&shown, removal.tops(), &words, format)),
            notes,
        }
    }

    /// Says that nothing reaches any of the SYMBOLs, naming them as
    /// `naming` names them, and that removing them would save their own
    /// bytes, with a `range`, those in it.
    fn unreachable_answer(&self, range: Option<&str>, naming: &Naming) -> String {
        let own = self.parts.removal.retained();
        let bytes = text::bytes_text(own);
        let saves = match range {
            Some(_) if own == 0 => would_save("them", own, range),
            Some(range) => format!("Removing them would save their own {bytes} in {range}"),
            None => format!("Removing them would save their own {bytes}"),
        };
        let nothing_reaches = text::nothing_reaches_any(&names(self.found), naming);
        format!("{nothing_reaches} {saves}.")
    }

    /// The SYMBOLs that nothing reaches, in the order given.
    fn unreachable(&self) -> Vec<&Found> {
        let tree = &self.retained.tree;
        (self.found.iter())
            .filter(|found| !tree.is_reachable(Node::Item(found.index)))
            .collect()
    }

    /// The items that the table shows: the 30 largest of all that goes, or
    /// with `all`, every one, and the items between each of them and its
    /// top. Unlike for one SYMBOL, the tops compete for rows too, so a
    /// SYMBOL that keeps little may have none, and the row that sums up the
    /// rest counts it. As for one SYMBOL, only items with bytes of their
    /// own compete: with a range, those with bytes in it.
    fn shown(&self, all: bool) -> HashSet<usize> {
        let retained = self.retained;
        let tops = self.parts.removal.tops();
        let items: Vec<usize> = (tops.iter())
            .filter_map(|&top| match top {
                Node::Item(index) => Some(index),
                Node::Import(_) => None,
            })
            .filter(|&index| retained.has_bytes_in_range(index))
            .chain(self.parts.under_tops.iter().copied())
            .collect();
        retained.largest(tops, &items, text::shown(all))
    }

    /// The words that end the row of each top in `shown` that several
    /// SYMBOLs keep together, as in "kept by process_frame and report
    /// together". They name the SYMBOLs it needs, in the order given (see
    /// [`Parts::keepers`]) and as rows name the items they hang under: up
    /// to 3, or 4 when that is all, and then how many more, or with `all`,
    /// every one.
    fn kept_together_words(&self, shown: &HashSet<usize>, all: bool) -> HashMap<usize, String> {
        let mut keepers = self.parts.keepers();
        keepers.retain(|index, _| shown.contains(index));
        // Each SYMBOL may keep many rows together; its name is worked out
        // once.
        let names = self.retained.names();
        let mut short_names: HashMap<usize, String> = HashMap::new();
        for &keeper in keepers.values().flatten() {
            (short_names.entry(keeper)).or_insert_with(|| names.short_name(Node::Item(keeper)));
        }
        keepers
            .into_iter()
            .map(|(index, keepers)| {
                let keepers: Vec<&str> = (keepers.iter())
                    .map(|keeper| short_names[keeper].as_str())
                    .collect();
                let words = format!("kept by {} together", text::keepers_text(&keepers, all));
                (index, words)
            })
            .collect()
    }

    /// The notes under the tree, naming the items they list as `naming`
    /// says: which SYMBOLs add nothing, unless only ranges were given, the
    /// bytes that go only when several go, the SYMBOLs that nothing
    /// reaches, the functions left out, and then the notes of every answer
    /// about removing items (see [`Parts::notes`]).
    fn notes(&self, naming: &Naming) -> Vec<String> {
        let adds_nothing = match self.given {
            Given::Symbols | Given::SymbolsAndRanges => {
                (self.keeps_the_rest_note()).or_else(|| self.add_nothing_note(naming))
            }
            // The user named none of the items, so which of them the others
            // keep is no news.
            Given::Ranges(_) => None,
        };
        [
            adds_nothing,
            self.together_note(),
            self.unreachable_note(naming),
            left_out_note(self.retained.binary, self.left_out, naming),
        ]
        .into_iter()
        .flatten()
        .chain(self.parts.notes(naming))
        .collect()
    }

    /// Says that one SYMBOL keeps all the others, as in "vtable for
    /// app::Gain keeps the other 3, so removing it alone saves the same.",
    /// if one does: something reaches every SYMBOL, and one is the only
    /// top. When nothing reaches some, removing the one alone doesn't save
    /// their own bytes, so the note on what adds nothing names the others
    /// instead.
    fn keeps_the_rest_note(&self) -> Option<String> {
        let &[top] = self.parts.removal.tops() else {
            return None;
        };
        if !self.unreachable().is_empty() {
            return None;
        }
        let keeper = (self.found.iter()).find(|found| Node::Item(found.index) == top)?;
        let others: Vec<&Found> = (self.found.iter())
            .filter(|found| Node::Item(found.index) != top)
            .collect();
        let others = match &others[..] {
            [other] => other.lead_name.clone(),
            others => format!("the other {}", others.len()),
        };
        let name = &keeper.lead_name;
        Some(format!(
            "{name} keeps {others}, so removing it alone saves the same."
        ))
    }

    /// Names the SYMBOLs that the others keep, alone or together, so that
    /// the answer is the same without them, as `naming` names them, as in
    /// "filter_tap and __memcpy_veneer add nothing: other items named here
    /// keep them." It names no item that only a range found: the user
    /// didn't name it, so that it adds nothing is no news. `None` when
    /// there are none.
    fn add_nothing_note(&self, naming: &Naming) -> Option<String> {
        let removal = &self.parts.removal;
        let names = names(
            (self.found.iter())
                .filter(|found| found.named && removal.is_kept_by_others(Node::Item(found.index))),
        );
        let (add, them) = match names.len() {
            0 => return None,
            1 => ("adds", "it"),
            _ => ("add", "them"),
        };
        Some(format!(
            "{} {add} nothing: other items named here keep {them}.",
            naming.phrase(&names)
        ))
    }

    /// Says how many of the bytes saved go only when several SYMBOLs go,
    /// as in "56 of these bytes go only when both go.", if any do.
    fn together_note(&self) -> Option<String> {
        let together = self.parts.bytes_kept_together();
        let go = match together {
            0 => return None,
            1 => "goes",
            _ => "go",
        };
        // Of all the SYMBOLs, as the first line counts them: those that
        // nothing reaches keep nothing, so they never make these go.
        let when = match self.found.len() {
            2 => "both go".to_string(),
            symbols => format!("two or more of the {symbols} go"),
        };
        Some(format!("{together} of these bytes {go} only when {when}."))
    }

    /// Names the SYMBOLs that nothing reaches, as `naming` names them, and
    /// says that the total counts their own bytes, if they have any. `None`
    /// when something reaches each SYMBOL.
    fn unreachable_note(&self, naming: &Naming) -> Option<String> {
        let names = names(self.unreachable());
        let its = match names[..] {
            [] => return None,
            [_] => "its",
            _ => "their",
        };
        let nothing_reaches = text::nothing_reaches_any(&names, naming);
        Some(match self.parts.unreachable_own() {
            0 => nothing_reaches,
            own => format!(
                "{nothing_reaches} The total counts {its} own {}.",
                text::bytes_text(own)
            ),
        })
    }
}
