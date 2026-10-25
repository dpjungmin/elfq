//! Long C++ and Rust names made to fit a column, keeping the parts that
//! tell them apart. The Rust name of a function that splits a node of a
//! B-tree, `<alloc::collections::btree::node::Handle<…>>::split::<…>`,
//! takes almost 300 columns, and cut at the end, every such `split` reads
//! the same. So a name that is too wide is made shorter a step at a time,
//! and only as far as it needs to fit:
//!
//! 1. Inside brackets, each path keeps its last part:
//!    `<Handle<NodeRef<Mut, u64, Abbreviation, Internal>, KV>>::split::<Global>`,
//!    and `<&[u8] as <alloc::ffi::c_str::CString>::new::SpecNewImpl>::spec_new_impl`
//!    becomes `<&[u8] as SpecNewImpl>::spec_new_impl`.
//! 2. The modules at the start of the name go, keeping its last two parts:
//!    `alloc::raw_vec::finish_grow` becomes `raw_vec::finish_grow`.
//! 3. Brackets become `<…>`, `(…)` or `[…]`, the innermost first, and of
//!    those the longest first: `<Handle<NodeRef<…>, KV>>::split::<Global>`.
//! 4. The end is cut, with "…".
//!
//! Two rows that would then read the same, such as two overloads in C++,
//! show the part of the name where they differ instead.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

use super::cut;

/// `name` made to fit `width` columns, by the steps that the module's
/// documentation lists, only as many as it takes.
pub fn fit_name(name: &str, width: usize) -> String {
    if name.chars().count() <= width {
        return name.to_string();
    }
    // A name whose brackets don't pair up is only cut.
    let Some(mut pieces) = pieces(name) else {
        return cut(name, width);
    };
    shorten_inner_paths(&mut pieces);
    fit_shortened(pieces, width)
}

/// [`fit_name`] for `name` at one width after another, as when looking
/// for the widest that leaves a header short enough: step 1, which
/// doesn't depend on the width, is done once.
pub fn name_fitter(name: &str) -> impl Fn(usize) -> String {
    let length = name.chars().count();
    let shortened = pieces(name).map(|mut pieces| {
        shorten_inner_paths(&mut pieces);
        pieces
    });
    move |width| {
        if length <= width {
            return name.to_string();
        }
        match &shortened {
            Some(pieces) => fit_shortened(pieces.clone(), width),
            None => cut(name, width),
        }
    }
}

/// Steps 2 to 4 for `pieces`, a name whose paths in brackets are
/// shortened.
fn fit_shortened(mut pieces: Vec<Piece>, width: usize) -> String {
    if text(&pieces).chars().count() > width {
        drop_outer_modules(&mut pieces);
    }
    collapse_brackets(&mut pieces, width);
    cut(&text(&pieces), width)
}

/// Each of `names` made to fit `width` columns by [`fit_name`], except
/// that a name that would then read the same as another, which is a
/// different name, shows the part where they differ, with "…" on the
/// sides that are cut. Since a name shown that way can read as another
/// does, this is done again, a few times at most: each time with all the
/// names that the row has read the same as, or when those are the same as
/// before, with only the names that it still reads the same as. A row that
/// still reads as another then shows another part of its name, one that
/// no other row shows: around another place where it differs, if it can.
pub fn fit_names(names: &[&str], width: usize) -> Vec<String> {
    let mut fitted: Vec<String> = names.iter().map(|name| fit_name(name, width)).collect();
    // For each row, the names that it has read the same as.
    let mut twins: Vec<Vec<&str>> = vec![Vec::new(); names.len()];
    // The rows that read the same as a row of another name, in their
    // order, each with those other names.
    let alike = |fitted: &[String]| {
        let mut rows_of: HashMap<&str, Vec<usize>> = HashMap::new();
        for (row, text) in fitted.iter().enumerate() {
            rows_of.entry(text).or_default().push(row);
        }
        let mut alike: Vec<(usize, Vec<&str>)> = Vec::new();
        for rows in rows_of.values().filter(|rows| rows.len() > 1) {
            for &row in rows {
                let mut others: Vec<&str> = Vec::new();
                for &other in rows {
                    let other = names[other];
                    if other != names[row] && !others.contains(&other) {
                        others.push(other);
                    }
                }
                if !others.is_empty() {
                    alike.push((row, others));
                }
            }
        }
        alike.sort();
        alike
    };
    if alike(&fitted).is_empty() {
        return fitted;
    }
    let shorts: Vec<Option<String>> = names.iter().map(|name| shorten(name)).collect();
    let mut compared = Compared::new(names, &shorts);
    for _ in 0..8 {
        let mut changed = false;
        for (row, mut others) in alike(&fitted) {
            let before = twins[row].len();
            for &other in &others {
                if !twins[row].contains(&other) {
                    twins[row].push(other);
                }
            }
            if twins[row].len() > before {
                others = twins[row].clone();
            }
            let text = compared.difference(names[row], &others, width).first;
            changed |= text != fitted[row];
            fitted[row] = text;
        }
        if !changed {
            break;
        }
    }
    for (row, others) in alike(&fitted) {
        // The texts of the rows of other names.
        let taken: HashSet<&str> = (0..names.len())
            .filter(|&other| names[other] != names[row])
            .map(|other| fitted[other].as_str())
            .collect();
        let free = [&twins[row], &others]
            .into_iter()
            .filter(|twins| !twins.is_empty())
            .flat_map(|twins| compared.difference(names[row], twins, width).ways(width))
            .find(|way| !taken.contains(way.as_str()));
        if let Some(way) = free {
            // The rows of the same name read the same.
            for same in 0..names.len() {
                if names[same] == names[row] {
                    fitted[same] = way.clone();
                }
            }
        }
    }
    fitted
}

/// The names of the rows on the screen, as [`fit_names`] fits them, kept
/// while the same rows show in the same room, so that a key that only
/// moves the cursor among them doesn't fit them again: a few rows of C++
/// names of thousands of characters take a while to fit.
#[derive(Default)]
pub struct FittedRows {
    rows: Range<usize>,
    room: usize,
    names: Vec<String>,
}

impl FittedRows {
    /// The names of `rows` of a list fitted to `room` columns, from their
    /// full names, which `names` gives when they are needed.
    pub fn names<'a>(
        &mut self,
        rows: Range<usize>,
        room: usize,
        names: impl FnOnce() -> Vec<&'a str>,
    ) -> &[String] {
        if self.rows != rows || self.room != room {
            self.names = fit_names(&names(), room);
            (self.rows, self.room) = (rows, room);
        }
        &self.names
    }
}

/// The names that [`fit_names`] compares, each worked out once for all
/// the comparisons of a call, since a row is compared again in each
/// round, and with each set of names that it reads the same as.
struct Compared<'s> {
    /// Each name, with the paths in its brackets shortened as step 1
    /// does, or None when its brackets don't pair up.
    short: HashMap<&'s str, Option<&'s str>>,
    /// The form of each text compared, a name or a shortened one, or None
    /// when its brackets don't pair up.
    forms: HashMap<&'s str, Option<Form>>,
    /// The text of each pair of brackets of more than 5 characters, with
    /// the brackets, numbered in the order found, so that the brackets of
    /// two names compare as numbers.
    numbers: HashMap<&'s str, usize>,
    /// Each number's text, and its length in characters.
    texts: Vec<(&'s str, usize)>,
    /// What each name was found to be against each set of twins.
    differences: HashMap<(&'s str, Vec<&'s str>), Difference>,
}

/// A text's pieces and its brackets.
struct Form {
    pieces: Vec<Piece>,
    /// For each piece that opens brackets of more than 5 characters,
    /// where they close and the number of their text.
    pairs: Vec<Option<(usize, usize)>>,
    /// The numbers of the texts of its brackets.
    numbers: HashSet<usize>,
}

/// Where a name differs from its twins, as [`Compared::difference`]
/// finds it.
#[derive(Clone)]
struct Difference {
    /// The name as compared: with the paths in its brackets shortened,
    /// unless that makes it the same as a twin, and the brackets that it
    /// shares with them as `<…>` as far as that was done.
    name: String,
    /// The places where it first differs from each twin, in order.
    places: Vec<usize>,
    /// The first way to show it.
    first: String,
}

impl<'s> Compared<'s> {
    /// For `names`, whose paths in brackets, shortened, give `shorts`.
    fn new(names: &[&'s str], shorts: &'s [Option<String>]) -> Compared<'s> {
        Compared {
            short: (names.iter().copied())
                .zip(shorts.iter().map(Option::as_deref))
                .collect(),
            forms: HashMap::new(),
            numbers: HashMap::new(),
            texts: Vec::new(),
            differences: HashMap::new(),
        }
    }

    /// `width` columns of `name` around where it differs from `twins`.
    /// The names compared have the paths in their brackets shortened,
    /// unless that makes `name` the same as a twin. The places where it
    /// first differs from each twin are shown in one part of the name when
    /// they fit. When they don't, the brackets that every one of the names
    /// has, with the same text in them, become `<…>`, `(…)` or `[…]`, the
    /// longest first, as `basic_string<char, char_traits<char>,
    /// allocator<char> >` does in a set of C++ overloads, until they fit.
    /// Failing that, the first and the last place are shown in two parts,
    /// each half as wide: the last is where the name parts from the twin
    /// that it reads the most like. That is the first way to show it; the
    /// difference gives the others too ([`Difference::ways`]).
    fn difference(&mut self, name: &'s str, twins: &[&'s str], width: usize) -> Difference {
        let key = (name, twins.to_vec());
        if let Some(difference) = self.differences.get(&key) {
            return difference.clone();
        }
        let short = self.short[name];
        let short_twins: Option<Vec<&str>> = (twins.iter())
            .map(|&twin| self.short[twin].filter(|&twin| Some(twin) != short))
            .collect();
        let (whole, whole_twins) = match (short, short_twins) {
            (Some(short), Some(short_twins)) => (short, short_twins),
            _ => (name, twins.to_vec()),
        };
        for &text in std::iter::once(&whole).chain(&whole_twins) {
            self.add_form(text);
        }
        let forms: Option<Vec<&Form>> = (std::iter::once(&whole).chain(&whole_twins))
            .map(|text| self.forms[text].as_ref())
            .collect();
        // The brackets that every one of the names has, with the same text
        // in them, the longest first. Short ones are left out, as in step
        // 3, and so is everything when a name's brackets don't pair up.
        let mut shared: Vec<usize> = match &forms {
            Some(forms) => (forms[0].numbers.iter().copied())
                .filter(|number| forms[1..].iter().all(|form| form.numbers.contains(number)))
                .collect(),
            None => Vec::new(),
        };
        shared.sort_by_key(|&number| {
            let (text, length) = self.texts[number];
            (Reverse(length), text)
        });
        let rank: HashMap<usize, usize> = (shared.iter().enumerate())
            .map(|(rank, &number)| (number, rank))
            .collect();
        // The ranks of the shared brackets that aren't in other shared
        // ones, in any of the names. Shortening the brackets of any other
        // rank changes no name, so that count is passed over.
        let changes: HashSet<usize> = (forms.iter().flatten())
            .flat_map(|form| outermost(form, &rank))
            .collect();
        let (mut name, mut places) = (whole.to_string(), places_in(whole, &whole_twins));
        let mut first = None;
        for count in 0..=shared.len() {
            if count > 0 {
                if !changes.contains(&(count - 1)) {
                    continue;
                }
                let forms = forms
                    .as_ref()
                    .expect("only names that pair brackets share them");
                let shown = collapsed(forms[0], &rank, count);
                let shown_twins: Vec<String> = (forms[1..].iter())
                    .map(|form| collapsed(form, &rank, count))
                    .collect();
                // Brackets that hold the same text in a different order can
                // make two names read the same.
                if shown_twins.contains(&shown) {
                    break;
                }
                places = places_in(&shown, &shown_twins);
                name = shown;
            }
            // One part, a third of the way to the first place, if it shows
            // them all.
            let length = name.chars().count();
            let start = window_start(length, places[0], width);
            let last = places[places.len() - 1];
            if last < window_end(length, start, width) {
                first = Some(around(&name, places[0], width));
                break;
            }
        }
        let first =
            first.unwrap_or_else(|| two_parts(&name, places[0], places[places.len() - 1], width));
        let difference = Difference {
            name,
            places,
            first,
        };
        self.differences.insert(key, difference.clone());
        difference
    }

    /// Works out the form of `text`, unless it has been.
    fn add_form(&mut self, text: &'s str) {
        if self.forms.contains_key(text) {
            return;
        }
        let form = pieces(text).map(|pieces| {
            // Where each piece starts, in characters and in bytes: the
            // pieces keep every character of the text, in order.
            let mut starts = vec![(0, 0)];
            for piece in &pieces {
                let (chars, bytes) = starts[starts.len() - 1];
                let (piece_chars, piece_bytes) = piece.lengths();
                starts.push((chars + piece_chars, bytes + piece_bytes));
            }
            let mut pairs = vec![None; pieces.len()];
            let mut numbers = HashSet::new();
            let mut open = Vec::new();
            for (at, piece) in pieces.iter().enumerate() {
                match piece {
                    Piece::Open(_) => open.push(at),
                    Piece::Close(_) => {
                        let start = open.pop().expect("pieces pair their brackets");
                        let length = starts[at + 1].0 - starts[start].0;
                        if length > 5 {
                            let brackets = &text[starts[start].1..starts[at + 1].1];
                            let next = self.texts.len();
                            let number = *self.numbers.entry(brackets).or_insert(next);
                            if number == next {
                                self.texts.push((brackets, length));
                            }
                            pairs[start] = Some((at, number));
                            numbers.insert(number);
                        }
                    }
                    _ => {}
                }
            }
            Form {
                pieces,
                pairs,
                numbers,
            }
        });
        self.forms.insert(text, form);
    }
}

impl Difference {
    /// The ways to show the name: the first, then for when that reads as
    /// another row does, the part around each place by itself, the last
    /// first, then every part of the name that fits, from the start. They
    /// are made as they are needed, since a long name has thousands of
    /// parts.
    fn ways(self, width: usize) -> impl Iterator<Item = String> {
        let Difference {
            name,
            places,
            first,
        } = self;
        let arounds: Vec<String> = (places.iter().rev())
            .map(|&place| around(&name, place, width))
            .collect();
        let length = name.chars().count();
        let starts = (0..=(length + 1).saturating_sub(width)).filter(move |_| length > width);
        std::iter::once(first)
            .chain(arounds)
            .chain(starts.map(move |start| window(&name, start, width)))
    }
}

/// `name` with the paths in its brackets shortened, as step 1 does, or
/// None when its brackets don't pair up.
fn shorten(name: &str) -> Option<String> {
    pieces(name).map(|mut pieces| {
        shorten_inner_paths(&mut pieces);
        text(&pieces)
    })
}

/// The places where `name` first differs from each of `twins`, in order.
fn places_in(name: &str, twins: &[impl AsRef<str>]) -> Vec<usize> {
    let mut places: Vec<usize> = (twins.iter())
        .map(|twin| first_difference(name, twin.as_ref()))
        .collect();
    places.sort();
    places.dedup();
    places
}

/// The text of `form` with the brackets whose text is one of the first
/// `count` that `rank` ranks shortened to `<…>`, `(…)` or `[…]`, the
/// outermost of them.
fn collapsed(form: &Form, rank: &HashMap<usize, usize>, count: usize) -> String {
    let mut shown = String::new();
    let mut at = 0;
    while at < form.pieces.len() {
        if let Some((close, number)) = form.pairs[at]
            && rank.get(&number).is_some_and(|&rank| rank < count)
        {
            let (Piece::Open(open), Piece::Close(close_char)) =
                (&form.pieces[at], &form.pieces[close])
            else {
                unreachable!("brackets open and close with brackets");
            };
            shown.extend([*open, '…', *close_char]);
            at = close + 1;
            continue;
        }
        form.pieces[at].push_to(&mut shown);
        at += 1;
    }
    shown
}

/// The ranks, in `rank`, of the brackets of `form` that aren't in other
/// ranked ones.
fn outermost(form: &Form, rank: &HashMap<usize, usize>) -> Vec<usize> {
    let mut ranks = Vec::new();
    let mut at = 0;
    while at < form.pieces.len() {
        match form.pairs[at].and_then(|(close, number)| Some((close, *rank.get(&number)?))) {
            Some((close, rank)) => {
                ranks.push(rank);
                at = close + 1;
            }
            None => at += 1,
        }
    }
    ranks
}

/// `width` columns of `name` in two parts, each half as wide, around the
/// characters at `first` and `second`, with up to three "…": before,
/// between and after them. When the parts would meet, or are too narrow
/// to say much, only the part around `first`.
fn two_parts(name: &str, first: usize, second: usize, width: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    let room = width.saturating_sub(3) / 2;
    let first_part = part(chars.len(), first, room);
    let second_part = part(chars.len(), second, room);
    if room < 4 || second_part.start <= first_part.end {
        return around(name, first, width);
    }
    let mut shown = String::new();
    let mut at = 0;
    for part in [first_part, second_part] {
        if part.start > at {
            shown.push('…');
        }
        shown.extend(&chars[part.clone()]);
        at = part.end;
    }
    if at < chars.len() {
        shown.push('…');
    }
    shown
}

/// The `room` characters of a name of `length` characters that show the
/// one at `at` a third of the way in, or as near as the name allows.
fn part(length: usize, at: usize, room: usize) -> Range<usize> {
    let start = at.saturating_sub(room / 3).min(length.saturating_sub(room));
    start..(start + room).min(length)
}

/// The place, in characters, where `a` and `b` first differ.
fn first_difference(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(a, b)| a == b).count()
}

/// Where the window of `width` columns that [`around`] shows of a name of
/// `length` characters, from `start`, ends: the first character that it
/// doesn't show. A "…" takes the first column when the window starts
/// after the name does, and the last when the name goes on after it.
fn window_end(length: usize, start: usize, width: usize) -> usize {
    if length <= width {
        length
    } else if start == 0 {
        width.saturating_sub(1)
    } else if start + width > length {
        length
    } else {
        start + width - 2
    }
}

/// Where the window of `width` columns that [`around`] shows of a name of
/// `length` characters starts, for the character at `at`.
fn window_start(length: usize, at: usize, width: usize) -> usize {
    if length <= width || width < 3 {
        return 0;
    }
    // The last start that still fills the width, after the "…".
    at.saturating_sub(width / 3).min(length + 1 - width)
}

/// `width` columns of `name` around the character at `at`, which comes a
/// third of the way in, with "…" where the name goes on.
fn around(name: &str, at: usize, width: usize) -> String {
    window(name, window_start(name.chars().count(), at, width), width)
}

/// `width` columns of `name` from the character at `start`, with "…"
/// where the name goes on.
fn window(name: &str, start: usize, width: usize) -> String {
    if start == 0 {
        return cut(name, width);
    }
    // Enough of the rest to tell whether it goes on.
    let rest: String = name.chars().skip(start).take(width).collect();
    format!("…{}", cut(&rest, width.saturating_sub(1)))
}

/// A piece of a name, as `pieces` splits it.
#[derive(Clone)]
enum Piece {
    /// `<`, `(` or `[`.
    Open(char),
    /// `>`, `)` or `]`.
    Close(char),
    /// A name or a path, such as `alloc::vec::Vec` or `::new` after a
    /// bracket, with what reads as part of it: `{closure#0}`, C++'s
    /// `{lambda(unsigned int)#2}` and `(anonymous namespace)`, and the
    /// symbol of an operator, as in `operator<`.
    Word(String),
    /// What goes between the words: spaces, commas, `&`, `*`, `->`.
    Between(String),
}

/// The symbols of C++'s operators, the longest first.
const OPERATORS: [&str; 39] = [
    "<=>", "<<=", ">>=", "->*", "()", "[]", "->", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||",
    "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<", ">", "=", "!", "+", "-", "*",
    "/", "%", "^", "&", "|", "~", ",",
];

/// `name` split into words and brackets, or None when its brackets don't
/// pair up.
fn pieces(name: &str) -> Option<Vec<Piece>> {
    let chars: Vec<char> = name.chars().collect();
    let mut pieces = Vec::new();
    let mut open = Vec::new();
    let mut word = String::new();
    let mut at = 0;
    let rest_starts = |at: usize, text: &str| {
        chars[at..]
            .iter()
            .copied()
            .take(text.len())
            .eq(text.chars())
    };
    while at < chars.len() {
        let c = chars[at];
        // A word's own brackets.
        if c == '{' {
            let mut depth = 0;
            while at < chars.len() {
                word.push(chars[at]);
                depth += match chars[at] {
                    '{' => 1,
                    '}' => -1,
                    _ => 0,
                };
                at += 1;
                if depth == 0 {
                    break;
                }
            }
            continue;
        }
        if rest_starts(at, "(anonymous namespace)") {
            word.push_str("(anonymous namespace)");
            at += "(anonymous namespace)".len();
            continue;
        }
        if word == "operator" || word.ends_with("::operator") {
            // The longest that is an operator, so that the `<` after
            // `operator==` opens its template's arguments.
            if let Some(symbol) = OPERATORS.iter().find(|symbol| rest_starts(at, symbol)) {
                word.push_str(symbol);
                at += symbol.len();
                continue;
            }
        }
        let piece = match c {
            '<' | '(' | '[' => {
                open.push(c);
                Piece::Open(c)
            }
            '>' | ')' | ']' => {
                let pair = match c {
                    '>' => '<',
                    ')' => '(',
                    _ => '[',
                };
                if open.pop() != Some(pair) {
                    return None;
                }
                Piece::Close(c)
            }
            '-' if rest_starts(at, "->") => {
                at += 1;
                Piece::Between("->".to_string())
            }
            ' ' | ',' | '&' | '*' | ';' | '=' => Piece::Between(c.to_string()),
            c => {
                word.push(c);
                at += 1;
                continue;
            }
        };
        if !word.is_empty() {
            pieces.push(Piece::Word(std::mem::take(&mut word)));
        }
        pieces.push(piece);
        at += 1;
    }
    if !word.is_empty() {
        pieces.push(Piece::Word(word));
    }
    open.is_empty().then_some(pieces)
}

impl Piece {
    /// Adds its text to `text`.
    fn push_to(&self, text: &mut String) {
        match self {
            Piece::Open(c) | Piece::Close(c) => text.push(*c),
            Piece::Word(word) | Piece::Between(word) => text.push_str(word),
        }
    }

    /// Its length in characters, and in bytes.
    fn lengths(&self) -> (usize, usize) {
        match self {
            Piece::Open(c) | Piece::Close(c) => (1, c.len_utf8()),
            Piece::Word(word) | Piece::Between(word) => (word.chars().count(), word.len()),
        }
    }
}

/// The name that `pieces` make.
fn text(pieces: &[Piece]) -> String {
    let mut text = String::new();
    for piece in pieces {
        piece.push_to(&mut text);
    }
    text
}

/// Where `word` breaks into the parts of a path: the places of its `::`,
/// outside its own brackets, such as those of `{closure#0}`.
fn path_breaks(word: &str) -> Vec<usize> {
    let mut breaks = Vec::new();
    let mut depth = 0;
    let bytes = word.as_bytes();
    for (at, &byte) in bytes.iter().enumerate() {
        match byte {
            b'{' | b'(' => depth += 1,
            b'}' | b')' => depth -= 1,
            // A `::`, but not the second colon of the one before.
            b':' if depth == 0
                && bytes.get(at + 1) == Some(&b':')
                && breaks.last().is_none_or(|&last| last + 1 != at) =>
            {
                breaks.push(at);
            }
            _ => {}
        }
    }
    breaks
}

/// Step 1: each path inside brackets keeps its last part, and the `::`
/// after it when a turbofish follows, as in `drop_in_place::<T>`. A path
/// can start with a type in brackets, as in `<&[u8] as
/// <CString>::new::SpecNewImpl>`, which keeps `SpecNewImpl`. A word that
/// starts with `::` after any other bracket, as `::push` in `Vec<T>::push`,
/// stays.
fn shorten_inner_paths(pieces: &mut Vec<Piece>) {
    let mut depth = 0;
    let mut at = 0;
    while at < pieces.len() {
        let after_word = at > 0 && matches!(pieces[at - 1], Piece::Word(_));
        match &mut pieces[at] {
            Piece::Open('<') if depth > 0 && !after_word => {
                if let Some(last) = qualified_path_end(pieces, at) {
                    // The type and the parts before the last go, and the
                    // last is shortened as the next piece.
                    pieces.drain(at..last);
                    let Piece::Word(word) = &mut pieces[at] else {
                        unreachable!("a path's last part is a word");
                    };
                    word.replace_range(..2, "");
                    continue;
                }
                depth += 1;
            }
            Piece::Open(_) => depth += 1,
            Piece::Close(_) => depth -= 1,
            Piece::Word(word) if depth > 0 && !word.starts_with("::") => {
                let path = word.strip_suffix("::").unwrap_or(word);
                let start = kept_start(path);
                if start > 0 {
                    let turbofish = if path.len() < word.len() { "::" } else { "" };
                    *word = format!("{}{turbofish}", &path[start..]);
                }
            }
            _ => {}
        }
        at += 1;
    }
}

/// Where the part of `path` that step 1 keeps starts: its last part, and
/// when that is a closure or a shim, such as `{closure#0}`, the part
/// before it too, which names the function that it belongs to:
/// `resolve::{closure#1}`.
fn kept_start(path: &str) -> usize {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(path_breaks(path).into_iter().map(|at| at + 2))
        .collect();
    let mut kept = starts.len() - 1;
    while kept > 0 && path[starts[kept]..].starts_with('{') {
        kept -= 1;
    }
    starts[kept]
}

/// For a path that starts with a type in brackets at `open`, such as
/// `<CString>::new::SpecNewImpl` or `<Once>::call_once_force<F>::{closure#0}`,
/// the place of the word that holds the part that the path keeps:
/// `::new::SpecNewImpl`, or for a closure, the word before it,
/// `::call_once_force`. None when no path follows the brackets, as in
/// `<T>` alone.
fn qualified_path_end(pieces: &[Piece], open: usize) -> Option<usize> {
    let mut words = Vec::new();
    let mut at = close_of(pieces, open) + 1;
    while let Some(Piece::Word(word)) = pieces.get(at) {
        if !word.starts_with("::") {
            break;
        }
        words.push(at);
        at += 1;
        // Brackets after a part belong to it: `call_once_force<F>`.
        if let Some(Piece::Open(_)) = pieces.get(at) {
            at = close_of(pieces, at) + 1;
        }
    }
    // A word of closures and shims alone keeps the one before it.
    let closures = |at: usize| {
        let Piece::Word(word) = &pieces[at] else {
            unreachable!("only words are listed");
        };
        kept_start(&word[2..]) == 0 && word[2..].starts_with('{')
    };
    while words.len() > 1 && closures(words[words.len() - 1]) {
        words.pop();
    }
    words.last().copied()
}

/// The place of the bracket that closes the one at `open`.
fn close_of(pieces: &[Piece], open: usize) -> usize {
    let mut depth = 0;
    for (at, piece) in pieces.iter().enumerate().skip(open) {
        match piece {
            Piece::Open(_) => depth += 1,
            Piece::Close(_) => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return at;
        }
    }
    unreachable!("pieces pair their brackets")
}

/// Step 2: the modules at the start of the name go, keeping the name's
/// last two parts: `a::b::Vec<T>::push` becomes `Vec<T>::push`. A
/// turbofish counts with the part before it, so
/// `core::ptr::drop_in_place::<T>` becomes `ptr::drop_in_place::<T>`. Only
/// plain names go, so a name that starts with a bracket, as `<T as
/// Trait>::f` does, or with words, as `vtable for app::Gain` does, keeps
/// its start.
fn drop_outer_modules(pieces: &mut [Piece]) {
    // The parts of the name outside brackets.
    let mut parts = 1;
    let mut depth = 0;
    for (at, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Open(_) => depth += 1,
            Piece::Close(_) => depth -= 1,
            Piece::Word(word) if depth == 0 => {
                parts += path_breaks(word).len();
                let turbofish = matches!(pieces.get(at + 1), Some(Piece::Open('<')));
                if word.ends_with("::") && turbofish {
                    parts -= 1;
                }
            }
            _ => {}
        }
    }
    let Some(Piece::Word(word)) = pieces.first_mut() else {
        return;
    };
    // The modules: the parts of the first word before its last.
    let plain = |part: &str| {
        part == "(anonymous namespace)"
            || !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_alphanumeric() || "_$.".contains(c))
    };
    let mut start = 0;
    for (dropped, at) in path_breaks(word).into_iter().enumerate() {
        if parts - dropped <= 2 || !plain(&word[start..at]) {
            break;
        }
        start = at + 2;
    }
    word.replace_range(..start, "");
}

/// Step 3: brackets become `<…>`, `(…)` or `[…]` until the name fits in
/// `width` columns: those deepest in first, and of those the longest,
/// then the first. The brackets that a name starts with, as in `<T as
/// Trait>::f`, stay, since what follows them means little without them.
///
/// Brackets as deep in as each other don't hold each other, so what each
/// holds is counted once, when those deeper in are done, and what each
/// saves is taken from the name's length: a name of thousands of brackets
/// is shortened in a pass over it for each depth.
fn collapse_brackets(pieces: &mut Vec<Piece>, width: usize) {
    // Each pair of brackets: how deep in it is, where it opens and where
    // it closes, the deepest first.
    let mut pairs = Vec::new();
    let mut open = Vec::new();
    for (at, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Open(_) => open.push(at),
            Piece::Close(_) => {
                let start = open.pop().expect("pieces pair their brackets");
                pairs.push((open.len(), start, at));
            }
            _ => {}
        }
    }
    pairs.sort_by_key(|&(depth, start, _)| (Reverse(depth), start));
    // The characters that each piece shows, as brackets become "…".
    let mut shown: Vec<usize> = pieces.iter().map(|piece| piece.lengths().0).collect();
    let mut length: usize = shown.iter().sum();
    // Where each pair that becomes "…" closes, by where it opens.
    let mut collapsed: HashMap<usize, usize> = HashMap::new();
    for level in pairs.chunk_by(|a, b| a.0 == b.0) {
        if length <= width {
            break;
        }
        // The characters before each piece.
        let before: Vec<usize> = std::iter::once(0)
            .chain(shown.iter().scan(0, |sum, &count| {
                *sum += count;
                Some(*sum)
            }))
            .collect();
        // What each holds, but for the brackets that the name starts with.
        // Short ones, such as `[u8]` or `<'a>`, stay: they save little and
        // say much.
        let mut held: Vec<(usize, usize, usize)> = (level.iter())
            .filter(|&&(depth, start, _)| depth > 0 || start > 0)
            .map(|&(_, start, end)| (before[end] - before[start + 1], start, end))
            .filter(|&(inside, ..)| inside > 3)
            .collect();
        held.sort_by_key(|&(inside, start, _)| (Reverse(inside), start));
        for (inside, start, end) in held {
            if length <= width {
                break;
            }
            shown[start + 1..end].fill(0);
            shown[start + 1] = 1;
            length -= inside - 1;
            collapsed.insert(start, end);
        }
    }
    if collapsed.is_empty() {
        return;
    }
    let mut kept = Vec::with_capacity(pieces.len());
    let mut skip_to = 0;
    for (at, piece) in std::mem::take(pieces).into_iter().enumerate() {
        if at < skip_to {
            continue;
        }
        kept.push(piece);
        if let Some(&end) = collapsed.get(&at) {
            kept.push(Piece::Word("…".to_string()));
            skip_to = end;
        }
    }
    *pieces = kept;
}

/// For tests: the first two different names of `names`, a list's rows in
/// its order, that read the same in some window of rows that a screen
/// shows, of 8, 13 or 19 rows, once fitted to `room` columns, with what
/// they read. A name fitted wider than `room` counts too, as reading the
/// same as itself.
#[cfg(test)]
pub(super) fn alike_in_a_window<'a>(
    names: &[&'a str],
    room: usize,
) -> Option<(&'a str, &'a str, String)> {
    let mut shown = 0;
    for rows in [8, 13, 19] {
        if names.len() <= shown {
            break;
        }
        shown = rows;
        for window in names.windows(rows.min(names.len())) {
            let mut seen: HashMap<String, &str> = HashMap::new();
            for (&name, fitted) in window.iter().zip(fit_names(window, room)) {
                if fitted.chars().count() > room {
                    return Some((name, name, fitted));
                }
                match seen.get(&fitted) {
                    Some(&other) if other != name => return Some((other, name, fitted)),
                    _ => seen.insert(fitted, name),
                };
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::samples;
    use super::*;
    use crate::binary::Binary;
    use crate::graph::Node;

    const INTERNAL: &str = "<alloc::collections::btree::node::Handle<alloc::collections::btree::node::NodeRef<alloc::collections::btree::node::marker::Mut, u64, gimli::read::abbrev::Abbreviation, alloc::collections::btree::node::marker::Internal>, alloc::collections::btree::node::marker::KV>>::split::<alloc::alloc::Global>";
    const LEAF: &str = "<alloc::collections::btree::node::Handle<alloc::collections::btree::node::NodeRef<alloc::collections::btree::node::marker::Mut, u64, gimli::read::abbrev::Abbreviation, alloc::collections::btree::node::marker::Leaf>, alloc::collections::btree::node::marker::KV>>::split::<alloc::alloc::Global>";

    /// What each step makes of `name`, with the width set to stop at each
    /// one in turn.
    fn steps(name: &str, widths: &[usize]) -> Vec<String> {
        widths.iter().map(|&width| fit_name(name, width)).collect()
    }

    #[test]
    fn a_name_that_fits_is_left_alone() {
        assert_eq!(fit_name("log_msg", 7), "log_msg");
        assert_eq!(
            fit_name("alloc::raw_vec::finish_grow", 30),
            "alloc::raw_vec::finish_grow"
        );
    }

    #[test]
    fn a_rust_name_loses_its_paths_then_its_brackets() {
        assert_eq!(
            steps(INTERNAL, &[200, 52, 40, 20]),
            [
                "<Handle<NodeRef<Mut, u64, Abbreviation, Internal>, KV>>::split::<Global>",
                "<Handle<NodeRef<…>, KV>>::split::<Global>",
                "<Handle<…>>::split::<Global>",
                "<Handle<…>>::split:…",
            ]
        );
        // A path that starts with a type in brackets keeps its last part
        // too, and so does one with brackets after a part.
        assert_eq!(
            fit_name(
                "<&[u8] as <alloc::ffi::c_str::CString>::new::SpecNewImpl>::spec_new_impl",
                60
            ),
            "<&[u8] as SpecNewImpl>::spec_new_impl"
        );
        assert_eq!(
            fit_name(
                "<core::cell::once::OnceCell<core::result::Result<addr2line::line::Lines, gimli::read::Error>>>::try_init::<<core::cell::once::OnceCell<core::result::Result<addr2line::line::Lines, gimli::read::Error>>>::get_or_init<<addr2line::line::LazyLines>::borrow<gimli::read::endian_slice::EndianSlice<gimli::endianity::LittleEndian>>::{closure#0}>::{closure#0}, !>",
                80
            ),
            "<OnceCell<Result<…>>>::try_init::<get_or_init<…>::{closure#0}, !>"
        );
    }

    #[test]
    fn the_modules_at_the_start_go_but_the_last_two_parts_stay() {
        assert_eq!(
            fit_name("std::sys::pal::unix::thread_info::set_current_info", 40),
            "thread_info::set_current_info"
        );
        assert_eq!(
            fit_name("core::ptr::drop_in_place::<std::thread::Thread>", 32),
            "ptr::drop_in_place::<Thread>"
        );
        assert_eq!(fit_name("alloc::vec::Vec<u8>::push", 15), "Vec<u8>::push");
        // C++: the parameters belong to the last part.
        assert_eq!(
            fit_name(
                "app::(anonymous namespace)::crc32(unsigned char const*, unsigned long)",
                65
            ),
            "(anonymous namespace)::crc32(unsigned char const*, unsigned long)"
        );
        assert_eq!(
            fit_name("app::Gain::run(short*, unsigned long)", 33),
            "Gain::run(short*, unsigned long)"
        );
        // Words before the path stay.
        assert_eq!(
            fit_name("typeinfo name for app::DcBlock", 20),
            "typeinfo name for a…"
        );
    }

    #[test]
    fn what_reads_as_part_of_a_word_stays_in_it() {
        // Rust closures, a C++ lambda and an operator's symbol.
        assert_eq!(
            fit_name(
                "<std::backtrace_rs::symbolize::gimli::Cache>::with_global::<std::backtrace_rs::symbolize::gimli::resolve::{closure#1}>",
                60
            ),
            "<Cache>::with_global::<resolve::{closure#1}>"
        );
        assert_eq!(
            fit_name(
                "app::setup_handlers()::{lambda(unsigned int)#2}::_FUN(unsigned int)",
                51
            ),
            "setup_handlers()::{lambda(unsigned int)#2}::_FUN(…)"
        );
        assert_eq!(
            fit_name(
                "app::operator<(app::Reading const&, app::Reading const&)",
                50
            ),
            "app::operator<(Reading const&, Reading const&)"
        );
        assert_eq!(
            fit_name(
                "app::operator<(app::Reading const&, app::Reading const&)",
                20
            ),
            "app::operator<(…)"
        );
        // A closure or a shim keeps the function that it belongs to.
        assert_eq!(
            fit_name(
                "<std::sys::personality::gcc::find_eh_action::{closure#0} as core::ops::function::FnOnce<()>>::call_once::{shim:vtable#0}",
                64
            ),
            "<find_eh_action::{closure#0} as FnOnce<()>>::call_once::{shim:v…"
        );
        assert_eq!(
            fit_name(
                "<std::sys::backtrace::_print_fmt::{closure#0} as core::ops::function::FnOnce<(&mut core::fmt::Formatter, std::backtrace_rs::types::BytesOrWideString)>>::call_once::{shim:vtable#0}",
                64
            ),
            "<_print_fmt::{closure#0} as FnOnce<(…)>>::call_once::{shim:vtab…"
        );
        assert_eq!(
            fit_name(
                "<<std::sync::once::Once>::call_once<std::rt::cleanup::{closure#0}>::{closure#0} as core::ops::function::FnOnce<(&std::sync::once::OnceState,)>>::call_once::{shim:vtable#0}",
                64
            ),
            "<call_once<…>::{closure#0} as FnOnce<(…)>>::call_once::{shim:vt…"
        );
        // An operator's symbol ends where the template's arguments start.
        assert_eq!(
            fit_name(
                "std::operator==<char, std::char_traits<char>, std::allocator<char> >(std::__cxx11::basic_string<char, std::char_traits<char>, std::allocator<char> > const&, char const*)",
                30
            ),
            "std::operator==<…>(…)"
        );
        // A name whose brackets don't pair up is only cut.
        assert_eq!(fit_name("a::b<c::d(e>", 10), "a::b<c::d…");
    }

    #[test]
    fn rust_drop_glue_keeps_its_bounds_in_brackets() {
        let name = "core::ptr::drop_in_place::<alloc::boxed::Box<dyn for<'a, 'b> core::ops::function::Fn<(&'a std::panic::PanicHookInfo<'b>,), Output = ()> + core::marker::Send + core::marker::Sync>>";
        assert_eq!(
            steps(name, &[104, 98, 77, 57, 28]),
            [
                "core::ptr::drop_in_place::<Box<dyn for<'a, 'b> Fn<(&'a PanicHookInfo<'b>,), Output = ()> + Send + Sync>>",
                "ptr::drop_in_place::<Box<dyn for<'a, 'b> Fn<(&'a PanicHookInfo<'b>,), Output = ()> + Send + Sync>>",
                "ptr::drop_in_place::<Box<dyn for<'a, 'b> Fn<(…), Output = ()> + Send + Sync>>",
                "ptr::drop_in_place::<Box<dyn for<…> Fn<…> + Send + Sync>>",
                "ptr::drop_in_place::<Box<…>>",
            ]
        );
    }

    #[test]
    fn rows_that_would_read_the_same_show_where_they_differ() {
        // Rust: the same function for two kinds of node.
        let names = [INTERNAL, LEAF, "<alloc::raw_vec::RawVecInner>::deallocate"];
        assert_eq!(
            fit_names(&names, 42),
            [
                "…Abbreviation, Internal>, KV>>::split::<G…",
                "…Abbreviation, Leaf>, KV>>::split::<Globa…",
                "<alloc::raw_vec::RawVecInner>::deallocate",
            ]
        );
        // C++ overloads.
        assert_eq!(
            fit_names(&["app::log(char const*)", "app::log(unsigned int)"], 12),
            ["…log(char c…", "…log(unsign…"]
        );
        // Alone, a name is fitted as it would be by itself.
        assert_eq!(fit_names(&[INTERNAL], 42), [fit_name(INTERNAL, 42)]);
        // And the same name twice reads the same.
        assert_eq!(
            fit_names(&[LEAF, LEAF], 42),
            [fit_name(LEAF, 42), fit_name(LEAF, 42)]
        );
    }

    #[test]
    fn names_that_differ_in_two_places_show_both() {
        // Four closures: for stdout or for cleanup, each with a shim.
        let names: Vec<String> = ["cleanup", "stdout"]
            .iter()
            .flat_map(|which| {
                ["", "::{shim:vtable#0}"].map(|shim| {
                    format!(
                        "<<std::sync::once::Once>::call_once_force<<std::sync::once_lock::OnceLock<std::io::stdio::StdoutRaw>>::initialize<std::io::stdio::{which}::{{closure#0}}>> as core::ops::function::FnOnce<(&std::sync::once::OnceState,)>>::call_once{shim}"
                    )
                })
            })
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(
            fit_name(names[0], 40),
            "<call_once_force<…> as FnOnce<(…)>>::ca…"
        );
        assert_eq!(
            fit_names(&names, 40),
            [
                "…alize<cleanup::{cl…nce<…>>::call_once",
                "…alize<cleanup::{cl…l_once::{shim:vtab…",
                "…alize<stdout::{clo…nce<…>>::call_once",
                "…alize<stdout::{clo…l_once::{shim:vtab…",
            ]
        );
    }

    #[test]
    fn the_place_where_names_differ_comes_a_third_of_the_way_in() {
        assert_eq!(around("abcdefghijklmnopqrstuvwxyz", 12, 9), "…jklmnop…");
        // Not past the end, nor before the start.
        assert_eq!(around("abcdefghijklmnopqrstuvwxyz", 25, 9), "…stuvwxyz");
        assert_eq!(around("abcdefghijklmnopqrstuvwxyz", 1, 9), "abcdefgh…");
    }

    #[test]
    fn every_name_in_the_samples_fits() {
        for path in samples() {
            let binary = Binary::load(&path).unwrap();
            let items = (0..binary.items.len()).map(Node::Item);
            let imports = (0..binary.imports.len()).map(Node::Import);
            for name in items.chain(imports).map(|node| node.name(&binary)) {
                for width in [12, 40, 80] {
                    let fitted = fit_name(name, width);
                    assert!(
                        fitted.chars().count() <= width,
                        "{name} at {width}: {fitted}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_part_shown_holds_every_place_where_twins_differ() {
        let drift = "core::slice::sort::stable::drift::sort::<addr2line::function::FunctionAddress, <[addr2line::function::FunctionAddress]>::sort_by_key<u64, <addr2line::function::Functions<gimli::read::endian_slice::EndianSlice<gimli::endianity::LittleEndian>>>::parse::{closure#1}>::{closure#0}>";
        let inlined = "core::slice::sort::stable::drift::sort::<addr2line::function::InlinedFunctionAddress, <[addr2line::function::InlinedFunctionAddress]>::sort_by<<addr2line::function::Function<gimli::read::endian_slice::EndianSlice<gimli::endianity::LittleEndian>>>::parse::{closure#0}>::{closure#0}>";
        let quicksort = "core::slice::sort::stable::quicksort::quicksort::<addr2line::function::FunctionAddress, <[addr2line::function::FunctionAddress]>::sort_by_key<u64, <addr2line::function::Functions<gimli::read::endian_slice::EndianSlice<gimli::endianity::LittleEndian>>>::parse::{closure#1}>::{closure#0}>";
        // "drift" against "quicksort", and "F" against "I", in two parts.
        let names = [drift, inlined, quicksort];
        let shorts: Vec<Option<String>> = names.iter().map(|name| shorten(name)).collect();
        assert_eq!(
            Compared::new(&names, &shorts)
                .difference(drift, &[inlined, quicksort], 23)
                .first,
            "…e::drift::…::<Functio…"
        );
        assert_eq!(
            fit_names(&[drift, inlined, quicksort], 23),
            [
                "…sort::<FunctionAddres…",
                "…sort::<InlinedFunctio…",
                "quicksort::quicksort::…",
            ]
        );
        // The last character before the closing "…" isn't where they
        // differ.
        let thunks = [
            "__Thumbv7ABSLongThunk_ram_calls_flash",
            "__Thumbv7ABSLongThunk_ram_calls_rom",
            "__Thumbv7ABSLongThunk_flash_work",
        ];
        for width in [17, 18] {
            let fitted = fit_names(&thunks, width);
            assert_eq!(fitted[..2], ["…k_ram_c…s_flash", "…k_ram_c…lls_rom"]);
            assert!(fitted[2].ends_with("Thunk_flash_work"), "{fitted:?}");
        }
    }

    #[test]
    fn overloads_that_differ_in_many_places_drop_what_they_share() {
        let replace = |iterators: &str, rest: &str| {
            let string =
                "std::__cxx11::basic_string<char, std::char_traits<char>, std::allocator<char> >";
            let iterator = format!("__gnu_cxx::__normal_iterator<{iterators}, {string} >");
            format!("{string}::replace({iterator}, {iterator}, {rest})")
        };
        let names = [
            replace("char*", "char const*, char const*"),
            replace("char const*", "char const*, char const*"),
            replace("char*", "char*, char*"),
            replace("char const*", "unsigned long, char"),
        ];
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        // Each shows both places where it differs, its iterators and its
        // last parameters, once what they all share is <…>.
        assert_eq!(
            fit_names(&names, 50),
            [
                "…or<char*, basic_string<…>, char const*, char co…",
                "…or<char const*, basic_s…<…> >, char const*, cha…",
                "…or<char*, basic_string<…ing<…> >, char*, char*)",
                "…or<char const*, basic_s…<…> >, unsigned long, c…",
            ]
        );
    }
}
