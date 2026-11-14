//! The names to suggest for SYMBOL that finds nothing: those that have it
//! in any case, or differ from it by a typo or two. The names in lowercase
//! that it compares with serve the go-to box in `browse` too.

use super::Names;
use crate::binary::names;
use crate::commands::text;
use crate::graph::Node;

/// Every name of every item and import in lowercase, one after another,
/// for what looks for names that have some text in any case: the go-to
/// box, and [`Names::similar`].
pub(super) struct Lowercase {
    text: String,
    /// Where each name ends in `text`.
    ends: Vec<usize>,
    /// Where the names of each item, by its index, and then of each import
    /// start in `ends`, and after the last, where they end.
    starts: Vec<usize>,
    /// Whether every name is ASCII. Then each character is a byte, and a
    /// part of a name in lowercase is that part of it in lowercase.
    ascii: bool,
}

/// The short names, in lowercase, of every item and import that
/// [`Names::similar`] can suggest, so that each query needn't work them out
/// again. Their names are in [`Lowercase`]. Most short names are parts of
/// those names, and are kept as where they are in them.
pub(super) struct Suggestable {
    /// Each item or import, with where its short names start.
    nodes: Vec<SuggestableNode>,
    /// The short names that aren't kept as parts, one after another.
    text: String,
    /// Where each of those ends in `text`.
    ends: Vec<usize>,
    /// The short names that are parts of names.
    parts: Vec<NamePart>,
}

/// An item or import that [`Names::similar`] can suggest, with where its
/// short names start in [`Suggestable`].
struct SuggestableNode {
    node: Node,
    /// Where its short names that aren't kept as parts start in `ends`.
    first_short_name: usize,
    /// Where those that are start in `parts`.
    first_part: usize,
}

/// A short name that is a part of a name in [`Lowercase`]. Parts are kept
/// only when every name is ASCII. Each number is a `u32`, so that a part
/// takes 12 bytes.
#[derive(PartialEq)]
struct NamePart {
    /// The name, by where it is in `Lowercase`'s `ends`.
    name: u32,
    /// Where the short name starts in it.
    start: u32,
    /// The short name's length, in bytes.
    length: u32,
}

/// An item or import that [`Names::similar`] can suggest, with its short
/// names in lowercase, as [`Suggestable::short_names`] gives them: those
/// that aren't kept as parts, and those that are.
struct SuggestableNames<'s, ShortNames> {
    node: Node,
    /// Its short names that aren't kept as parts.
    short_names: ShortNames,
    /// Those that are, as parts of its names in [`Lowercase`].
    parts: &'s [NamePart],
}

/// Text to look for in names. `str::contains` is quick for text of up to
/// 32 bytes, so longer text is looked for by its first 32 bytes first.
pub(in crate::commands) struct Needle<'t> {
    text: &'t str,
    start: &'t str,
}

/// What [`Names::similar`] compares names with: the query, in lowercase,
/// and how many typos a name may have from it and still come close.
struct Closeness<'q> {
    /// The query, in lowercase.
    query: &'q str,
    /// Its characters.
    query_chars: Vec<char>,
    /// The query, to look for in names.
    needle: Needle<'q>,
    /// The most typos that a name that comes close has: one for each three
    /// characters of the query, and at most 2.
    typos: usize,
    /// Whether every name is ASCII, as [`Lowercase`] says.
    ascii: bool,
}

impl<'a> Names<'a> {
    /// Up to five things to type for items or imports whose names contain
    /// `query`, ignoring case, or differ from it by a typo or two.
    pub(super) fn similar(&self, query: &str) -> Vec<String> {
        let query = query.to_lowercase();
        let closeness = Closeness::new(&query, self.lowercase().ascii);
        // Closest first, then shortest. How close each item or import
        // comes is worked out from its names in lowercase, made once; which
        // of its names came closest only for those that the loop below
        // gets to.
        let suggestable = self.suggestable();
        let mut similar: Vec<(usize, usize, &str, Node)> = (0..suggestable.nodes.len())
            .filter_map(|at| {
                let node = suggestable.nodes[at].node;
                let name = node.name(self.binary);
                Some((self.nearest(at, &closeness)?, name.len(), name, node))
            })
            .collect();
        similar.sort();

        let mut suggestions: Vec<String> = Vec::new();
        for (distance, _, _, node) in similar {
            let Some(suggestion) = self.suggestion(node, distance, &closeness) else {
                continue;
            };
            let suggestion = text::shell_quote(&suggestion);
            if !suggestions.contains(&suggestion) {
                suggestions.push(suggestion);
            }
            if suggestions.len() == 5 {
                break;
            }
        }
        suggestions
    }

    /// How close the item or import at `at` in [`Suggestable`]'s `nodes`
    /// comes to the query, if it comes close: the least distance of its
    /// names and short names, in lowercase (see [`Closeness::distance`]).
    fn nearest(&self, at: usize, closeness: &Closeness) -> Option<usize> {
        let lowercase = self.lowercase();
        let SuggestableNames {
            node,
            short_names,
            parts,
        } = self.suggestable().short_names(at);
        let lowercase_names = lowercase.names(self.position(node));
        let mut nearest = None;
        for name in lowercase_names.chain(short_names) {
            match closeness.distance(name) {
                Some(0) => {
                    nearest = Some(0);
                    break;
                }
                Some(distance) => nearest = nearest.min(Some(distance)).or(Some(distance)),
                None => {}
            }
        }
        // A part of a name has the query only if the name has it, so it can
        // only come close by typos.
        if nearest != Some(0) {
            for part in parts {
                let length = part.length as usize;
                if !closeness.length_comes_close(length) {
                    continue;
                }
                let start = part.start as usize;
                let name = &lowercase.name(part.name as usize)[start..start + length];
                if let Some(distance) = closeness.typos_from(name) {
                    nearest = nearest.min(Some(distance)).or(Some(distance));
                }
            }
        }
        nearest
    }

    /// Those of `node`'s names and their short names to suggest: of those
    /// that come close to the query, the ones with the fewest typos from
    /// it, and of those, the ones that contain it. So `SV_Handler` gets
    /// `SVC_Handler`, not `PendSV_Handler`, and `iprintf` gets `fiprintf`,
    /// not `fprintf`. `None` when none of them comes close.
    fn closest_names(&self, node: Node, closeness: &Closeness) -> Option<Vec<String>> {
        let close: Vec<(usize, String)> = self
            .names_of(node)
            .into_iter()
            .flat_map(|name| {
                names::short_names(name)
                    .into_iter()
                    .chain([name.to_string()])
            })
            .filter_map(|name| Some((closeness.distance(&name.to_lowercase())?, name)))
            .collect();
        let rank = |(distance, name): &(usize, String)| {
            // More typos than `typos` count the same.
            let typos_from = closeness
                .typos_from(&name.to_lowercase())
                .unwrap_or(closeness.typos + 1);
            (typos_from, *distance > 0)
        };
        let best = close.iter().map(rank).min()?;
        let closest: Vec<String> = close
            .iter()
            .filter(|name| rank(name) == best)
            .map(|(_, name)| name.clone())
            .collect();
        Some(closest)
    }

    /// What to type for `node`, whose names come `distance` from the query
    /// (see [`Names::nearest`]): the shortest of the names that came
    /// closest (see [`Names::closest_names`]), which needn't be the item's
    /// shortest name: `sniprintf` for `sniprin`, though its alias
    /// `snprintf` is shorter. Failing that, when none of them finds only
    /// this item, or they are all mangled symbols, the shortest thing to
    /// type that does, but not one that has the query only in a part that
    /// it leaves out, such as a generic argument. `None` when none of its
    /// names comes close, or there is nothing else to type.
    fn suggestion(&self, node: Node, distance: usize, closeness: &Closeness) -> Option<String> {
        let closest = self.closest_names(node, closeness)?;
        closest
            .into_iter()
            .filter(|name| names::demangle(name).is_none() && self.finds_only(name, node))
            .min_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)))
            .or_else(|| {
                let query_for = self.query_for(node);
                (distance > 0 || query_for.to_lowercase().contains(closeness.query))
                    .then_some(query_for)
            })
    }

    /// Whether one of the names of `node`, an item or an import, in
    /// lowercase, has `text`, which is lowercase too.
    #[cfg(feature = "tui")]
    pub(in crate::commands) fn name_has(&self, node: Node, text: &Needle) -> bool {
        self.lowercase()
            .names(self.position(node))
            .any(|name| text.is_in(name))
    }

    /// Every name of every item and import in lowercase.
    fn lowercase(&self) -> &Lowercase {
        self.lowercase.get_or_init(|| {
            let items = &self.binary.items;
            let imports = &self.binary.imports;
            // The table's size first, so that it is made once, no larger.
            let (mut count, mut length) = (0, 0);
            let names = items.iter().flat_map(|item| item.names());
            for name in names.chain(imports.iter().map(String::as_str)) {
                count += 1;
                length += name.len();
            }
            let mut lowercase = Lowercase {
                text: String::with_capacity(length),
                ends: Vec::with_capacity(count),
                starts: Vec::with_capacity(items.len() + imports.len() + 1),
                ascii: true,
            };
            for item in items {
                lowercase.starts.push(lowercase.ends.len());
                for name in item.names() {
                    lowercase.push(name);
                }
            }
            for import in imports {
                lowercase.starts.push(lowercase.ends.len());
                lowercase.push(import);
            }
            lowercase.starts.push(lowercase.ends.len());
            lowercase
        })
    }

    /// The short names of the items and imports that [`Names::similar`]
    /// can suggest, in lowercase. A stub for one of the file's own
    /// functions is left out: its function is the one to suggest.
    fn suggestable(&self) -> &Suggestable {
        self.suggestable.get_or_init(|| {
            let lowercase = self.lowercase();
            let items = (0..self.binary.items.len())
                .filter(|index| !self.own_function_stubs().contains(index))
                .map(Node::Item);
            let imports = (0..self.binary.imports.len()).map(Node::Import);
            let mut suggestable = Suggestable {
                nodes: Vec::new(),
                text: String::new(),
                ends: Vec::new(),
                parts: Vec::new(),
            };
            for node in items.chain(imports) {
                let (first_end, first_part) = (suggestable.ends.len(), suggestable.parts.len());
                suggestable.nodes.push(SuggestableNode {
                    node,
                    first_short_name: first_end,
                    first_part,
                });
                let starts = lowercase.starts[self.position(node)]..;
                for (at, name) in starts.zip(self.names_of(node)) {
                    // Each short name once, where it is a part of `name`
                    // when it can be.
                    names::for_each_short_name(name, |short_name, start| {
                        if short_name == name {
                            return;
                        }
                        let part = start.filter(|_| lowercase.ascii).and_then(|start| {
                            let number = |at: usize| u32::try_from(at).ok();
                            Some(NamePart {
                                name: number(at)?,
                                start: number(start)?,
                                length: number(short_name.len())?,
                            })
                        });
                        match part {
                            Some(part) => {
                                if !suggestable.parts[first_part..].contains(&part) {
                                    suggestable.parts.push(part);
                                }
                            }
                            None => {
                                let start = suggestable.text.len();
                                match short_name.is_ascii() {
                                    true => suggestable
                                        .text
                                        .extend(short_name.chars().map(|c| c.to_ascii_lowercase())),
                                    false => suggestable.text += &short_name.to_lowercase(),
                                }
                                // The item's earlier short names are
                                // walked with a running start, not
                                // through `packed`, which the compiler
                                // doesn't inline here: that took about
                                // 0.7% more instructions on the error
                                // for a name that finds nothing.
                                let text = &suggestable.text;
                                let mut from = match first_end {
                                    0 => 0,
                                    _ => suggestable.ends[first_end - 1],
                                };
                                let seen = suggestable.ends[first_end..].iter().any(|&end| {
                                    let earlier = &text[from..end];
                                    from = end;
                                    earlier == &text[start..]
                                });
                                match seen {
                                    true => suggestable.text.truncate(start),
                                    false => suggestable.ends.push(suggestable.text.len()),
                                }
                            }
                        }
                    });
                }
            }
            suggestable
        })
    }
}

impl Lowercase {
    /// Adds `name` in lowercase.
    fn push(&mut self, name: &str) {
        if name.is_ascii() {
            let start = self.text.len();
            self.text += name;
            self.text[start..].make_ascii_lowercase();
        } else {
            self.text += &name.to_lowercase();
            self.ascii = false;
        }
        self.ends.push(self.text.len());
    }

    /// The name at `at` in `ends`.
    fn name(&self, at: usize) -> &str {
        packed(&self.text, &self.ends, at)
    }

    /// The names of the item or import at `at` in `starts`.
    fn names(&self, at: usize) -> impl Iterator<Item = &str> {
        (self.starts[at]..self.starts[at + 1]).map(|name| self.name(name))
    }
}

impl Suggestable {
    /// The item or import at `at` in `nodes`, with its short names that
    /// aren't kept as parts, and those that are.
    fn short_names(&self, at: usize) -> SuggestableNames<'_, impl Iterator<Item = &str>> {
        let SuggestableNode {
            node,
            first_short_name: first,
            first_part,
        } = self.nodes[at];
        let (last, last_part) = self
            .nodes
            .get(at + 1)
            .map_or((self.ends.len(), self.parts.len()), |next| {
                (next.first_short_name, next.first_part)
            });
        SuggestableNames {
            node,
            short_names: (first..last).map(move |name| packed(&self.text, &self.ends, name)),
            parts: &self.parts[first_part..last_part],
        }
    }
}

impl<'t> Needle<'t> {
    pub(in crate::commands) fn new(text: &'t str) -> Needle<'t> {
        let mut end = text.len().min(32);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Needle {
            text,
            start: &text[..end],
        }
    }

    /// Whether `name` has the text.
    fn is_in(&self, name: &str) -> bool {
        name.contains(self.start)
            && (self.start.len() == self.text.len() || name.contains(self.text))
    }
}

impl<'q> Closeness<'q> {
    /// Compares names, which are all ASCII if `ascii`, with `query`, which
    /// is in lowercase.
    fn new(query: &'q str, ascii: bool) -> Closeness<'q> {
        let query_chars: Vec<char> = query.chars().collect();
        let typos = (query_chars.len() / 3).min(2);
        Closeness {
            query,
            needle: Needle::new(query),
            query_chars,
            typos,
            ascii,
        }
    }

    /// How far `name`, in lowercase, is from the query, if it is close: 0
    /// when it contains the query, or else the typos between them.
    fn distance(&self, name: &str) -> Option<usize> {
        match self.needle.is_in(name) {
            true => Some(0),
            false => self.typos_from(name),
        }
    }

    /// The typos between `name`, in lowercase, and the query, if there are
    /// `typos` or fewer.
    fn typos_from(&self, name: &str) -> Option<usize> {
        let length = match self.ascii {
            true => name.len(),
            false => name.chars().count(),
        };
        if !self.length_comes_close(length) {
            return None;
        }
        if self.ascii && self.query.is_ascii() {
            typos_within(name.as_bytes(), self.query.as_bytes(), self.typos)
        } else {
            let name: Vec<char> = name.chars().collect();
            typos_within(&name, &self.query_chars, self.typos)
        }
    }

    /// Whether a name `length` characters long can come close: one whose
    /// length differs from the query's by more than `typos` has more typos
    /// than that.
    fn length_comes_close(&self, length: usize) -> bool {
        length.abs_diff(self.query_chars.len()) <= self.typos
    }
}

/// The name at `at` of names kept one after another in `text`, where
/// `ends` says where each ends.
fn packed<'t>(text: &'t str, ends: &[usize], at: usize) -> &'t str {
    let start = at.checked_sub(1).map_or(0, |before| ends[before]);
    &text[start..ends[at]]
}

/// The number of typos that turn `a` into `b`, if there are `most` or
/// fewer, where a typo is a character inserted, deleted or replaced, or two
/// neighbors swapped (the optimal string alignment distance).
fn typos_within<T: PartialEq>(a: &[T], b: &[T], most: usize) -> Option<usize> {
    if a.len().abs_diff(b.len()) > most {
        return None;
    }
    // `row[j]` is the distance between the first i characters of `a` and
    // the first j of `b`, or `over` when it is more than `most`. Only the
    // rows for the two i before are needed, and in each only the j within
    // `most` of i: the others are more than `most`.
    let over = most + 1;
    let mut two_before = vec![over; b.len() + 1];
    let mut before: Vec<usize> = (0..=b.len()).map(|j| j.min(over)).collect();
    let mut row = vec![over; b.len() + 1];
    for i in 1..=a.len() {
        let first = i.saturating_sub(most).max(1);
        let last = (i + most).min(b.len());
        row[0] = i.min(over);
        // The j just outside the band, which the next rows read.
        if first > 1 {
            row[first - 1] = over;
        }
        if last < b.len() {
            row[last + 1] = over;
        }
        let mut least = row[0];
        for j in first..=last {
            let replace = before[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            let mut best = replace.min(before[j] + 1).min(row[j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(two_before[j - 2] + 1);
            }
            row[j] = best.min(over);
            least = least.min(row[j]);
        }
        // No row after has a smaller distance than this one's least.
        if least > most {
            return None;
        }
        // The row just made comes before the next, which reuses the oldest.
        std::mem::swap(&mut two_before, &mut before);
        std::mem::swap(&mut before, &mut row);
    }
    Some(before[b.len()]).filter(|&typos| typos <= most)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typos_are_counted_up_to_a_most() {
        let typos = |a: &str, b: &str, most| {
            let a: Vec<char> = a.chars().collect();
            let b: Vec<char> = b.chars().collect();
            typos_within(&a, &b, most)
        };
        assert_eq!(typos("process_frame", "process_frame", 2), Some(0));
        assert_eq!(typos("process_frame", "proces_frame", 2), Some(1));
        assert_eq!(typos("log_msg", "log_mgs", 1), Some(1));
        assert_eq!(typos("write", "wirte", 2), Some(1));
        assert_eq!(typos("log_msg", "lgo_mgs", 2), Some(2));
        assert_eq!(typos("log_msg", "lgo_mgs", 1), None);
        assert_eq!(typos("", "abc", 3), Some(3));
        assert_eq!(typos("abc", "", 2), None);
        // A swap counts once, but a character can't move further after it.
        assert_eq!(typos("ab", "ba", 1), Some(1));
        assert_eq!(typos("ca", "abc", 3), Some(3));
        assert_eq!(typos("ca", "abc", 2), None);
        assert_eq!(typos("größe", "grösse", 2), Some(2));
        // Typos far apart, and at both ends.
        assert_eq!(typos("xbcdefghiy", "abcdefghij", 2), Some(2));
        assert_eq!(typos("abcdefghij", "bcdefghijk", 2), Some(2));
        assert_eq!(typos("abcdefghij", "cdefghijkl", 3), None);
        assert_eq!(typos("abcdefghij", "cdefghijkl", 4), Some(4));
    }

    #[test]
    fn typos_within_a_most_are_those_of_every_alignment() {
        // Each pair of short strings over a small alphabet, against the
        // distance worked out without a band.
        let full = |a: &[u8], b: &[u8]| {
            let mut table = vec![vec![0; b.len() + 1]; a.len() + 1];
            for i in 0..=a.len() {
                for j in 0..=b.len() {
                    table[i][j] = if i == 0 || j == 0 {
                        i + j
                    } else {
                        let mut best = (table[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]))
                            .min(table[i - 1][j] + 1)
                            .min(table[i][j - 1] + 1);
                        if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                            best = best.min(table[i - 2][j - 2] + 1);
                        }
                        best
                    };
                }
            }
            table[a.len()][b.len()]
        };
        let strings: Vec<Vec<u8>> = (0..1093u32)
            .map(|mut number| {
                let mut string = Vec::new();
                while number > 0 {
                    string.push(b"abc"[(number % 3) as usize]);
                    number /= 3;
                }
                string
            })
            .collect();
        for a in &strings {
            for b in strings.iter().step_by(7) {
                let distance = full(a, b);
                for most in 0..4 {
                    let expected = Some(distance).filter(|&distance| distance <= most);
                    assert_eq!(typos_within(a, b, most), expected, "{a:?} {b:?} {most}");
                }
            }
        }
    }
}
