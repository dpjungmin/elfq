//! Symbol names: making mangled names readable, and the shorter names that
//! people type for long ones.
//!
//! Rust and C++ compilers encode a function's full path into its symbol
//! ("mangling"): Rust's `app::process_frame` becomes something like
//! `_RNvCsjtrlrMDykj5_3app13process_frame`, and C++'s
//! `app::process_frame(app::Frame const&)` becomes
//! `_ZN3app13process_frameERKNS_5FrameE`. elfq shows the readable name and
//! keeps the symbol as an alias, so both find the item ([`demangle`]).
//!
//! # Short names
//!
//! A *short name* is a shorter name that also means a long one: an end of
//! its path, as `process_frame` for `app::process_frame`, with or without
//! generic arguments and parameters, or a method's path through its type
//! or trait. [`short_names`] says which. A name in C has none, and
//! [`can_be_shorter`] tells it apart quickly.
//!
//! A large file has hundreds of thousands of names, too many to work out
//! the short names of each when SYMBOL is looked up. So each short name
//! has a *key*, the last part of its path ([`short_name_key`]), as
//! `run(short*) const` for `Gain::run(short*) const`, and a query is
//! looked up in two steps:
//!
//! 1. [`short_name_keys`] gives every key that a name's short names can
//!    have, without working them out, so that each item is filed once
//!    under the keys of its names;
//! 2. the query's key picks the few items filed under it, and
//!    [`short_names_with_key`] works out only their short names that have
//!    that key.
//!
//! A key such as `fmt`, in a Rust program, can file thousands of items.
//! For those, two quick checks come first. The *words* of a name are its
//! runs of letters, digits, `_` and characters outside ASCII ([`words`]),
//! and each word of a short name is a word of the name, unless taking out
//! generic arguments joins two, which [`may_join_words`] tells. So only the
//! items whose names have every word of the query can have it. Of those,
//! [`ends_with_short_name`] says yes for most at once, and
//! [`has_short_name`] answers for the rest.
//! `commands::symbol::Names` asks these questions, in that order, in its
//! `nodes` and `finds_only`.
//!
//! [`for_each_short_name`] visits a name's short names without putting
//! them in order, for the names that the error for a SYMBOL that finds
//! nothing suggests. [`without_copy_suffixes`] gives a function and the
//! copies that compilers make of it, such as `dev_read.constprop.0`, one
//! name, for `diff`.
//!
//! # Texts for patterns
//!
//! A pattern, such as `uart_*` or `core::fmt::*`, matches a name when it
//! matches all of one of the name's *texts*, which
//! [`for_each_pattern_text`] visits: the name itself, the paths that mean
//! the same and their ends, as for short names, with three differences. A
//! method in an impl block has the path of its owner only (see
//! [`ImplMethod::owner`]), not of both its type and its trait. A C++
//! special name, such as `vtable for app::Gain`, also has the texts of the
//! class or function it is for. And a copy of a function, such as
//! `crc32.part.0`, also has the texts of the function. Each text is a
//! whole path or an end of a longer one ([`TextKind`]), and a pattern
//! that starts with `::` leaves out the ends. `commands::symbol::pattern`
//! says why each of these rules is as it is.
//!
//! Each word of a text is a word of the name, as for short names (see
//! [`for_each_pattern_text`]), so a quick check of the pattern's words
//! against the name, with [`may_have_words`], [`starts_a_word`] and
//! [`ends_a_word`], rules out most names before their texts are worked
//! out. `commands::symbol`'s `QuickCheck` makes it.

/// The readable name of a mangled Rust or C++ symbol, or `None` when
/// `symbol` isn't one. Rust names leave out the hashes that tell builds of a
/// crate apart.
pub(crate) fn demangle(symbol: &str) -> Option<String> {
    if let Ok(name) = rustc_demangle::try_demangle(symbol) {
        return Some(format!("{name:#}"));
    }
    demangle_cpp(symbol)
}

/// Whether `name` is a mangled Rust or C++ symbol, which elfq shows by its
/// readable name (see [`demangle`]): an item's alias can be one, with
/// `@plt` after it for a PLT stub's.
pub(crate) fn is_mangled(name: &str) -> bool {
    demangle(name.strip_suffix("@plt").unwrap_or(name)).is_some()
}

/// The readable name of a C++ symbol, mangled as GCC and Clang do on every
/// target elfq reads (the Itanium C++ ABI): `app::Gain::run(short*)` for
/// `_ZN3app4Gain3runEPs`. Return types are left out, so that every name
/// starts with its path.
fn demangle_cpp(symbol: &str) -> Option<String> {
    if !symbol.starts_with("_Z") {
        return None;
    }
    let parsed = cpp_demangle::Symbol::new(symbol).ok()?;
    let options = cpp_demangle::DemangleOptions::new().no_return_type();
    let name = parsed.demangle_with_options(&options).ok()?;
    Some(as_cxxfilt_writes(name))
}

/// A few names that cpp_demangle writes its own way, as c++filt and `nm -C`
/// write them, which is how C++ programmers know them: `{vtable(app::Gain)}`
/// becomes `vtable for app::Gain`.
fn as_cxxfilt_writes(name: String) -> String {
    let inside = |prefix: &str| name.strip_prefix(prefix)?.strip_suffix(")}");
    if let Some(class) = inside("{vtable(") {
        return format!("vtable for {class}");
    }
    if let Some(class) = inside("{vtt(") {
        return format!("VTT for {class}");
    }
    // A thunk adjusts `this` by one offset, or for a covariant return by
    // two, before it jumps to the function: `{virtual override
    // thunk({offset(-8)}, app::Sensor::name())}`.
    if let Some(mut rest) = inside("{virtual override thunk(") {
        let mut offsets = Vec::new();
        while rest.starts_with('{') {
            let Some(end) = rest.find("}, ") else {
                return name;
            };
            offsets.push(&rest[..end]);
            rest = &rest[end + "}, ".len()..];
        }
        let thunk = match offsets[..] {
            [offset] if offset.starts_with("{virtual") => "virtual thunk",
            [_] => "non-virtual thunk",
            _ => "covariant return thunk",
        };
        return format!("{thunk} to {rest}");
    }
    name
}

/// The shorter names that also mean `name`: the ends of its path, with and
/// without generic arguments, and for C++, with and without parameters. A
/// Rust method in an impl block can also be named through its type or its
/// trait, and a PLT stub through its import.
///
/// For `<app::Gain as app::Stage>::run` they are `run`, `Gain::run`,
/// `app::Gain::run`, `Stage::run` and `app::Stage::run`. For
/// `app::checksum::<u8>` they are `checksum`, `checksum::<u8>` and
/// `app::checksum`. For `app::log(unsigned int)` they are `log`, `app::log`
/// and `log(unsigned int)`. For `write@plt`, it is `write`.
pub fn short_names(name: &str) -> Vec<String> {
    short_names_ending(name, None)
}

/// The [`short_names`] of `name` that have the [`short_name_key`] `key`.
pub fn short_names_with_key(name: &str, key: &str) -> Vec<String> {
    short_names_ending(name, Some(key))
}

/// Whether `short_name` is one of `name`'s [`short_names`], without
/// working out the others with other keys or putting them in order.
pub(crate) fn has_short_name(name: &str, short_name: &str) -> bool {
    let Some(key) = short_name_key(short_name) else {
        return false;
    };
    let mut has = false;
    for_each_short_name_ending(name, Some(key), &mut |its, _| has |= its == short_name);
    has && short_name != name
}

/// The [`short_names`] of `name`, or with a key, only those that have it.
fn short_names_ending(name: &str, key: Option<&str>) -> Vec<String> {
    let mut short = Vec::with_capacity(16);
    for_each_short_name_ending(name, key, &mut |short_name, _| {
        short.push(short_name.to_string());
    });
    short.retain(|short_name| short_name != name);
    short.sort();
    short.dedup();
    short
}

/// Calls `visit` with each of `name`'s [`short_names`], and with where it
/// starts in `name` when it is a part of it, without the work of putting
/// them in order: some more than once, and perhaps `name` itself.
pub(crate) fn for_each_short_name(name: &str, mut visit: impl FnMut(&str, Option<usize>)) {
    for_each_short_name_ending(name, None, &mut visit);
}

/// [`for_each_short_name`], or with a key, for only the short names that
/// have it.
fn for_each_short_name_ending(
    name: &str,
    key: Option<&str>,
    visit: &mut dyn FnMut(&str, Option<usize>),
) {
    // `vtable for app::Gain` is also `vtable for Gain`, which has the key
    // that `Gain` has.
    if let Some(prefix) = special_prefix(name) {
        for_each_short_name_ending(&name[prefix.len()..], key, &mut |short_name, _| {
            visit(&[prefix, short_name].concat(), None);
        });
        return;
    }
    for_each_path(
        name,
        key,
        ImplPaths::TypeAndTrait,
        |path, parts, in_name| {
            let last = parts[parts.len() - 1];
            // A short name's key is the last part of the path it ends, or
            // when that part is the whole short name, the part without
            // special prefixes.
            if key.is_some_and(|key| last != key && without_special_prefixes(last) != key) {
                return;
            }
            for_each_end(path, parts, |end, start| {
                if let Some(key) = key {
                    let its_key = match start + 1 == parts.len() {
                        true => without_special_prefixes(last),
                        false => last,
                    };
                    if its_key != key {
                        return;
                    }
                }
                visit(end, in_name.then_some(path.len() - end.len()));
            });
        },
    );
}

/// Calls `visit` with each end of `path`, split into its `parts`, that a
/// short name can be, and the index of the part where it starts: `path`
/// itself, from part 0, and each end from a later part where a short name
/// can start (see [`can_start_short_name`]).
fn for_each_end<'p>(path: &'p str, parts: &[&str], mut visit: impl FnMut(&'p str, usize)) {
    // Where the part starts in `path`.
    let mut at = 0;
    for (index, part) in parts.iter().enumerate() {
        if can_start_short_name(index, part) {
            visit(&path[at..], index);
        }
        at += part.len() + "::".len();
    }
}

/// Calls `visit` with each text of `name` that a pattern can match all of,
/// and whether it is a whole path or an end of a longer one (see
/// [Texts for patterns](self#texts-for-patterns)): `name` itself, its
/// paths, as for short names but with a method in an impl block through
/// its owner only, and their ends. A name with a special C++ prefix also
/// has the texts of the rest, each with the prefix kept, and for class
/// data with `::` after it, as `app::Gain::` for `vtable for app::Gain`,
/// or else as it is, as `app::Sensor::name()` for `non-virtual thunk to
/// app::Sensor::name()`. A copy of a function, such as `crc32.part.0`,
/// also has the texts of the function. Some texts come more than once.
///
/// Every text joins pieces of `name`, each cut from it at places outside
/// its words, and adds nothing but `::`. So each word of a text is a word
/// of `name`, as for short names (see [`words`]), unless taking out
/// generic arguments joins two, which [`may_join_words`] tells. The quick
/// check that rules out names for a pattern, `commands::symbol`'s
/// `QuickCheck`, relies on this, so a new kind of text must keep it.
pub(crate) fn for_each_pattern_text(name: &str, mut visit: impl FnMut(&str, TextKind)) {
    for_each_text(name, &mut visit);
    let function = without_copy_suffixes(name);
    if function != name {
        for_each_text(function, &mut visit);
    }
}

/// Whether a text of a name, which [`for_each_pattern_text`] visits, is a
/// whole path or an end of a longer one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextKind {
    /// A whole path, from its first part, as opposed to an end: the name
    /// itself, or a path that means the same, as `app::Gain::run` for
    /// `app::Gain::run(short*) const` and for `<app::Gain as
    /// app::Stage>::run`.
    Path,
    /// The end of a longer path, from a later part: `Gain::run` or `run`.
    End,
}

/// [`for_each_pattern_text`] for `name` alone, not for the function that
/// it may be a copy of.
fn for_each_text(name: &str, visit: &mut dyn FnMut(&str, TextKind)) {
    visit(name, TextKind::Path);
    if let Some(prefix) = special_prefix(name) {
        let rest = &name[prefix.len()..];
        let class_data = is_class_data(prefix);
        for_each_text(rest, &mut |text, kind| {
            visit(&[prefix, text].concat(), kind);
            if class_data {
                visit(&[text, "::"].concat(), kind);
            } else {
                visit(text, kind);
            }
        });
        return;
    }
    for_each_path(name, None, ImplPaths::Owner, |path, parts, _| {
        for_each_end(path, parts, |end, start| {
            let kind = match start {
                0 => TextKind::Path,
                _ => TextKind::End,
            };
            visit(end, kind);
        });
    });
}

/// Whether `short_name` is one of `name`'s [`short_names`] because `name`
/// ends with it, from one of the parts of its path, or a method in an impl
/// block does through its type or trait: `Gain::run` of `app::Gain::run`
/// or of `<app::Gain as app::Stage>::run`. A quick check for a short name:
/// when it says no, `name` may still have it.
pub fn ends_with_short_name(name: &str, short_name: &str) -> bool {
    // A special name's short names keep its prefix.
    if special_prefix(name).is_some() || !can_be_shorter(name) {
        return false;
    }
    // `name` and the paths of a method through its type and trait are
    // among the paths that `for_each_path` visits, and each of their ends
    // from a part where a short name can start is a short name, but `name`
    // itself.
    let ends_with = |path: &str| {
        let Some(start) = path.len().checked_sub(short_name.len()) else {
            return false;
        };
        if !path.ends_with(short_name) || short_name == name {
            return false;
        }
        let Some(parts) = split_path(path) else {
            return false;
        };
        let mut at = 0;
        for (index, part) in parts.into_iter().enumerate() {
            if at == start {
                return can_start_short_name(index, part);
            }
            at += part.len() + "::".len();
        }
        false
    };
    ends_with(name)
        || impl_method_paths(name, ImplPaths::TypeAndTrait)
            .iter()
            .any(|path| ends_with(path))
}

/// Whether a short name can start at `part`, part `index` of a path: at
/// the first part, or at one that isn't empty and doesn't start with
/// generic arguments, like the `<u8>` in `checksum::<u8>`.
fn can_start_short_name(index: usize, part: &str) -> bool {
    index == 0 || !part.is_empty() && !part.starts_with('<')
}

/// The words of `text`: its runs of letters, digits, `_` and characters
/// outside ASCII. Short names are made of parts of a name cut where words
/// end, and `::`, so that each word of a short name is a word of the name,
/// unless taking out generic arguments joins two words, as `a<b>c` would
/// (see [`may_join_words`]). That finds the few names that may have a
/// short name without working out their short names.
pub fn words(text: &str) -> impl Iterator<Item = &str> {
    let bytes = text.as_bytes();
    let mut at = 0;
    std::iter::from_fn(move || {
        let start = at + bytes[at..].iter().position(|&byte| in_word(byte))?;
        at = bytes[start..]
            .iter()
            .position(|&byte| !in_word(byte))
            .map_or(bytes.len(), |length| start + length);
        Some(&text[start..at])
    })
}

/// Whether `name` can have a short name, or a text that a pattern matches
/// (see [`for_each_pattern_text`]), that holds each of `words`, which are
/// runs of word characters (see [`words`]). Each word of a short name or a
/// text is a word of the name, unless taking out generic arguments joins
/// two, so when this says no, `name` has none.
pub(crate) fn may_have_words<W: AsRef<str>>(name: &str, words: &[W]) -> bool {
    may_join_words(name) || words.iter().all(|word| name.contains(word.as_ref()))
}

/// Whether taking out generic arguments from `name` may join two of its
/// words into one: when a `>` comes right before a letter, digit or `_`.
pub fn may_join_words(name: &str) -> bool {
    name.split('>')
        .skip(1)
        .any(|after| after.bytes().next().is_some_and(in_word))
}

/// Whether `word`, a run of word characters (see [`words`]), starts a word
/// of `text`: some place where `word` is in `text` is at its start or right
/// after a character that isn't part of a word.
pub(crate) fn starts_a_word(text: &str, word: &str) -> bool {
    any_place(text, word, |at| {
        at == 0 || !in_word(text.as_bytes()[at - 1])
    })
}

/// Whether `word`, a run of word characters, ends a word of `text`: some
/// place where `word` is in `text` is at its end or right before a
/// character that isn't part of a word.
pub(crate) fn ends_a_word(text: &str, word: &str) -> bool {
    any_place(text, word, |at| {
        text.as_bytes()
            .get(at + word.len())
            .is_none_or(|&after| !in_word(after))
    })
}

/// Whether `holds` holds for any place where `word` is in `text`, as a
/// byte offset. The places can overlap: `77` is in `h20777` at 3, which
/// doesn't end a word, and at 4, which does.
fn any_place(text: &str, word: &str, holds: impl Fn(usize) -> bool) -> bool {
    let mut from = 0;
    while let Some(found) = text[from..].find(word) {
        let at = from + found;
        if holds(at) {
            return true;
        }
        // The next place can start at the next character.
        from = at + text[at..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// Whether `byte` is part of a word (see [`words`]).
fn in_word(byte: u8) -> bool {
    IN_WORD[usize::from(byte)]
}

/// Whether each byte is part of a word, by value: letters, digits, `_`,
/// and the bytes of characters outside ASCII.
const IN_WORD: [bool; 256] = {
    let mut table = [false; 256];
    let mut byte = 0;
    while byte < 256 {
        let c = byte as u8;
        table[byte] = c.is_ascii_alphanumeric() || c == b'_' || !c.is_ascii();
        byte += 1;
    }
    table
};

/// What a short name is filed under, to find the names that may have it
/// without working out every short name: the last part of its path,
/// without a special C++ prefix. `run(short*) const` for `Gain::run(short*)
/// const`, and `Gain` for `vtable for Gain`. `None` when the brackets
/// don't pair up, as in no short name.
pub fn short_name_key(short_name: &str) -> Option<&str> {
    split_path(without_special_prefixes(short_name))?.pop()
}

/// The [`short_name_key`] of every one of `name`'s [`short_names`], and
/// perhaps a few more, worked out without joining the parts of paths.
pub fn short_name_keys(name: &str) -> Vec<String> {
    let name = without_special_prefixes(name);
    let mut keys = Vec::new();
    if !can_be_shorter(name) {
        return keys;
    }
    let end = path_end(name);
    let mut add = |last: &str| {
        keys.push(last.to_string());
        // A short name of the last part alone may start with a special
        // prefix, which its key leaves out.
        let without_prefixes = without_special_prefixes(last);
        if without_prefixes.len() < last.len() {
            keys.push(without_prefixes.to_string());
        }
    };
    // Each short name ends with the last part of one of the paths that
    // `for_each_path` visits, and those of `name` end as those of its end
    // do, or as the end itself does when it has nothing to make shorter.
    if end.len() < name.len()
        && let Some(last) = split_path(end).and_then(|mut parts| parts.pop())
    {
        add(last);
    }
    for_each_path(end, None, ImplPaths::TypeAndTrait, |_, parts, _| {
        add(parts[parts.len() - 1]);
    });
    keys.sort();
    keys.dedup();
    keys
}

/// The end of `name`'s path that its shorter paths differ in: from its
/// last part that doesn't start with generic arguments, as
/// `checksum::<u8>` in `app::checksum::<u8>`. Each of the shorter paths
/// keeps the parts before the end, or a type or trait in place of the
/// first, and ends with the end or a shorter path of it, so that it ends
/// as one of those does. The whole name when a clone suffix starts before
/// the end, which cuts it there, or when a `)` in the end closes a `(`
/// before it, which takes parameters off there.
fn path_end(name: &str) -> &str {
    let Some(separators) = top_level_matches(name, "::") else {
        return name;
    };
    let start = separators
        .iter()
        .map(|at| at + "::".len())
        .rfind(|&start| !name[start..].starts_with('<'))
        .unwrap_or(0);
    let end = &name[start..];
    let clone_before = clone_suffix_at(name).is_some_and(|at| at < start);
    let mut depth = 0usize;
    let closes_before = end.chars().any(|c| match c {
        '(' => {
            depth += 1;
            false
        }
        ')' => match depth.checked_sub(1) {
            Some(rest) => {
                depth = rest;
                false
            }
            None => true,
        },
        _ => false,
    });
    if clone_before || closes_before {
        return name;
    }
    end
}

/// Calls `visit` with each path that `name`'s short names are ends of,
/// split into its parts: `name`'s own path, and the shorter paths that
/// mean the same, each with and without generic arguments. A method in an
/// impl block has the paths that `impl_paths` says. With each, it says
/// whether the path is the start of `name`, so that its ends are parts of
/// `name`. With `ending`, it leaves out paths that don't end with it.
fn for_each_path(
    name: &str,
    ending: Option<&str>,
    impl_paths: ImplPaths,
    mut visit: impl FnMut(&str, &[&str], bool),
) {
    if !can_be_shorter(name) {
        return;
    }

    // Each path adds the shorter paths that mean the same thing, until none
    // is left to add. `@plt`, a clone suffix and parameters come off the
    // end, so the paths without them start `name` as the path did.
    let mut paths = vec![(name.to_string(), true)];
    let mut next = 0;
    while next < paths.len() {
        let (path, in_name) = &paths[next];
        let in_name = *in_name;
        let shorter: Vec<(String, bool)> = [
            path.strip_suffix("@plt").map(str::to_string),
            without_clone_suffix(path),
            without_parameters(path),
        ]
        .into_iter()
        .flatten()
        .map(|path| (path, in_name))
        .chain(
            impl_method_paths(path, impl_paths)
                .into_iter()
                .map(|path| (path, false)),
        )
        .collect();
        for (path, in_name) in shorter {
            match paths.iter_mut().find(|(seen, _)| *seen == path) {
                Some((_, seen_in_name)) => *seen_in_name |= in_name,
                None => paths.push((path, in_name)),
            }
        }
        next += 1;
    }

    let wanted = |path: &str| ending.is_none_or(|ending| path.ends_with(ending));
    for (path, in_name) in &paths {
        let generic = path.contains('<');
        if !generic && !wanted(path) {
            continue;
        }
        let Some(parts) = split_path(path) else {
            continue;
        };
        if wanted(path) {
            visit(path, &parts, *in_name);
        }
        // A path without generic arguments is the same without them. When
        // it has them only at its start, it is the path itself.
        if generic {
            let without = without_generics(&parts);
            if without != *path
                && wanted(&without)
                && let Some(parts) = split_path(&without)
            {
                visit(&without, &parts, false);
            }
        }
    }
}

/// Whether `name` has any of what makes a name shorter: `::` in a path,
/// `@plt`, `(` for parameters, `<` for generic arguments and `[` for a
/// clone suffix. Most names, as in C, have none of them, and no short
/// names.
pub(crate) fn can_be_shorter(name: &str) -> bool {
    // Looking for each character on its own is quicker than for any of
    // them at once.
    [':', '(', '<', '['].into_iter().any(|c| name.contains(c)) || name.ends_with("@plt")
}

/// `name` without the special C++ prefixes that it starts with.
fn without_special_prefixes(mut name: &str) -> &str {
    while let Some(prefix) = special_prefix(name) {
        name = &name[prefix.len()..];
    }
    name
}

/// The special C++ prefix that `name` starts with, such as `vtable for `,
/// if it starts with one: one of [`CLASS_DATA_PREFIXES`] or
/// [`OTHER_SPECIAL_PREFIXES`]. These are the special names that C++
/// compilers give to the data and code they make for a class or a
/// function, and short names keep the prefix.
fn special_prefix(name: &str) -> Option<&'static str> {
    CLASS_DATA_PREFIXES
        .iter()
        .chain(&OTHER_SPECIAL_PREFIXES)
        .find(|prefix| name.starts_with(*prefix))
        .copied()
}

/// Whether `prefix`, a special C++ prefix, names data that the compiler
/// makes for a class, which patterns see as a member of the class.
fn is_class_data(prefix: &str) -> bool {
    CLASS_DATA_PREFIXES.contains(&prefix)
}

/// The special C++ prefixes of the data that a compiler makes for a class:
/// its table of virtual functions, its table of those tables, and its type
/// information.
const CLASS_DATA_PREFIXES: [&str; 5] = [
    "vtable for ",
    "VTT for ",
    "typeinfo for ",
    "typeinfo name for ",
    "typeinfo fn for ",
];

/// The other special C++ prefixes, of what a compiler makes for a function
/// or a variable: the guard of a static variable, the functions that set up
/// a thread-local one, and thunks, which adjust `this` and jump to a
/// virtual function.
const OTHER_SPECIAL_PREFIXES: [&str; 6] = [
    "guard variable for ",
    "TLS init function for ",
    "TLS wrapper function for ",
    "non-virtual thunk to ",
    "virtual thunk to ",
    "covariant return thunk to ",
];

/// A C++ function's name without the suffix that GCC adds to its copies of
/// a function: `app::log(unsigned int) [clone .constprop.0]` becomes
/// `app::log(unsigned int)`.
fn without_clone_suffix(path: &str) -> Option<String> {
    let at = clone_suffix_at(path)?;
    Some(path[..at].to_string())
}

/// The suffixes that compilers add to the name of a copy of a C function,
/// each with an optional number after it, as in `dev_read.constprop.0`.
const COPY_SUFFIXES: [&str; 8] = [
    "constprop",
    "isra",
    "part",
    "cold",
    "lto_priv",
    "localalias",
    "clone",
    "llvm",
];

/// A function's name without the suffixes that compilers add to their
/// copies of it, so that a copy and the function have one name:
/// `dev_read.constprop.0` and `dev_read.llvm.4123` become `dev_read`, and
/// `log(unsigned int) [clone .isra.0]` becomes `log(unsigned int)`. LLVM's
/// `.llvm.` number changes with every build.
pub(crate) fn without_copy_suffixes(name: &str) -> &str {
    let mut name = &name[..clone_suffix_at(name).unwrap_or(name.len())];
    loop {
        // The numbers after the suffix, as in `.isra.0.1`.
        let mut rest = name;
        while let Some((before, number)) = rest.rsplit_once('.')
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
        {
            rest = before;
        }
        match rest.rsplit_once('.') {
            Some((before, suffix)) if !before.is_empty() && COPY_SUFFIXES.contains(&suffix) => {
                name = before;
            }
            _ => return name,
        }
    }
}

/// Where the suffix that GCC adds to its copies of a function starts in
/// `name`, if it has one.
fn clone_suffix_at(name: &str) -> Option<usize> {
    // Most names have no `[`, which is quicker to look for.
    if !name.contains('[') {
        return None;
    }
    name.find(" [clone ")
}

/// A C++ function's name without its parameters and the qualifiers after
/// them: `app::Gain::run(short*) const` becomes `app::Gain::run`. `None`
/// for a name that doesn't end with parameters.
fn without_parameters(path: &str) -> Option<String> {
    let mut end = path;
    while let Some(rest) = [" const", " volatile", " &&", " &", " noexcept"]
        .iter()
        .find_map(|qualifier| end.strip_suffix(qualifier))
    {
        end = rest;
    }
    let inside = end.strip_suffix(')')?;
    let mut depth = 1;
    for (at, c) in inside.char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    let function = &inside[..at];
                    // The `()` of C++'s call operator is part of its name:
                    // `Op::operator()` has no shorter name.
                    let call_operator = function == "operator" || function.ends_with("::operator");
                    return (!function.is_empty() && !function.ends_with("::") && !call_operator)
                        .then(|| function.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Which paths a method in an impl block has besides its own, which
/// starts with `<Type as Trait>` or `<Type>`.
#[derive(Clone, Copy)]
enum ImplPaths {
    /// Its paths through its type and through its trait, as short names
    /// have them: `app::Gain::run` and `app::Stage::run` for
    /// `<app::Gain as app::Stage>::run`.
    TypeAndTrait,
    /// Its path through its owner only (see [`ImplMethod::owner`]), as
    /// patterns see it.
    Owner,
}

/// For a method in an impl block, written `<Type as Trait>::method` or
/// `<Type>::method`, the paths that `which` says: `Type::method` and
/// `Trait::method`, or the path through its owner alone.
fn impl_method_paths(path: &str, which: ImplPaths) -> Vec<String> {
    let Some(method) = ImplMethod::of(path) else {
        return Vec::new();
    };
    let path_through = |owner: &str| [owner, method.rest].concat();
    match which {
        ImplPaths::TypeAndTrait => std::iter::once(method.type_name)
            .chain(method.trait_name)
            .map(path_through)
            .collect(),
        ImplPaths::Owner => vec![path_through(method.owner())],
    }
}

/// A method in an impl block, as Rust names it: `<Type as Trait>::method`,
/// or `<Type>::method` for one in an impl block of the type alone. Each
/// part is a slice of the path, as written.
struct ImplMethod<'p> {
    /// The type, as `app::Gain` in `<app::Gain as app::Stage>::run`.
    type_name: &'p str,
    /// The trait, as `app::Stage`, or `None` for `<Type>::method`.
    trait_name: Option<&'p str>,
    /// The rest of the path, after the `<...>`: `::run`.
    rest: &'p str,
}

impl<'p> ImplMethod<'p> {
    /// `path` as a method in an impl block, if it is one.
    fn of(path: &'p str) -> Option<ImplMethod<'p>> {
        if !path.starts_with('<') {
            return None;
        }
        let parts = split_path(path)?;
        let inner = parts[0].strip_prefix('<')?.strip_suffix('>')?;
        if parts.len() < 2 {
            return None;
        }
        let rest = &path[parts[0].len()..];
        let (type_name, trait_name) = match top_level_matches(inner, " as ")?.as_slice() {
            [] => (inner, None),
            &[at] => (&inner[..at], Some(&inner[at + " as ".len()..])),
            _ => return None,
        };
        Some(ImplMethod {
            type_name,
            trait_name,
            rest,
        })
    }

    /// The one type or trait that the method belongs to, for patterns:
    /// its type, without `&`, `&mut`, `*const`, `*mut` or `dyn` before it,
    /// or its trait when the type has no path of its own, as `u32`, `str`,
    /// `[u8]` and `(u8, u16)` have none. So `<&mut bigfw::Uart as
    /// core::fmt::Write>::write_str` belongs to `bigfw::Uart`, and `<u32 as
    /// core::fmt::Display>::fmt` to `core::fmt::Display`.
    ///
    /// One path, not both, so that `core::fmt::*` doesn't take every
    /// type's `Debug` impl in the program; and the trait's for a type
    /// without a path, so that it still takes how numbers are formatted.
    fn owner(&self) -> &'p str {
        let mut type_name = self.type_name;
        while let Some(rest) = ["&mut ", "&", "*const ", "*mut ", "dyn "]
            .iter()
            .find_map(|before| type_name.strip_prefix(before))
        {
            type_name = rest;
        }
        let has_path = top_level_matches(type_name, "::").is_some_and(|at| !at.is_empty());
        match self.trait_name {
            Some(trait_name) if !has_path => trait_name,
            _ => type_name,
        }
    }
}

/// Splits `path` at each `::` outside brackets: `<a::B as c::D>::f` has two
/// parts, `<a::B as c::D>` and `f`. `None` when the brackets don't pair up.
fn split_path(path: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::with_capacity(8);
    let mut start = 0;
    let paired = for_each_top_level_match(path, "::", |at| {
        parts.push(&path[start..at]);
        start = at + "::".len();
    });
    parts.push(&path[start..]);
    paired.then_some(parts)
}

/// A path, split into its `parts`, without generic arguments:
/// `alloc::vec::Vec<u8>::drop` becomes `alloc::vec::Vec::drop` and
/// `app::checksum::<u8>` becomes `app::checksum`. A leading `<Type as
/// Trait>` stays.
fn without_generics(parts: &[&str]) -> String {
    let mut kept = String::new();
    let mut first = true;
    for (index, part) in parts.iter().enumerate() {
        let leading = index == 0 && part.starts_with('<');
        if !leading && part.starts_with('<') {
            continue;
        }
        if !first {
            kept += "::";
        }
        first = false;
        if leading {
            kept += part;
        } else {
            push_without_angle_brackets(part, &mut kept);
        }
    }
    kept
}

/// Adds `text` to `kept` without anything in angle brackets: `Vec<u8>`
/// adds `Vec`.
fn push_without_angle_brackets(text: &str, kept: &mut String) {
    if !text.contains('<') {
        *kept += text;
        return;
    }
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    // Where the text outside brackets that `kept` doesn't have yet starts.
    let mut outside = 0;
    let mut at = 0;
    while at < bytes.len() {
        // C++'s `operator<` is a name, not a bracket.
        if bytes[at] == b'o'
            && let Some(end) = operator_end(bytes, at)
        {
            at = end;
            continue;
        }
        match bytes[at] {
            b'<' => {
                if depth == 0 {
                    *kept += &text[outside..at];
                }
                depth += 1;
            }
            b'>' if depth > 0 && !is_arrow(bytes, at) => {
                depth -= 1;
                if depth == 0 {
                    outside = at + 1;
                }
            }
            _ => {}
        }
        at += 1;
    }
    if depth == 0 {
        *kept += &text[outside..];
    }
}

/// Where `pattern` occurs in `text` outside any brackets, as byte offsets.
/// `None` when the brackets don't pair up.
fn top_level_matches(text: &str, pattern: &str) -> Option<Vec<usize>> {
    let mut matches = Vec::new();
    for_each_top_level_match(text, pattern, |at| matches.push(at)).then_some(matches)
}

/// Calls `found` with where `pattern` occurs in `text` outside any
/// brackets, as a byte offset, and says whether the brackets pair up.
fn for_each_top_level_match(text: &str, pattern: &str, mut found: impl FnMut(usize)) -> bool {
    let bytes = text.as_bytes();
    let pattern = pattern.as_bytes();
    let mut depth = 0usize;
    let mut at = 0;
    let first = pattern.first().copied();
    while at < bytes.len() {
        // Most names are long and have few brackets: bytes that start
        // nothing are passed over first.
        let byte = bytes[at];
        if !BRACKET_OR_OPERATOR[usize::from(byte)] && Some(byte) != first {
            at += 1;
            continue;
        }
        if depth == 0 && Some(byte) == first && bytes[at..].starts_with(pattern) {
            found(at);
            at += pattern.len();
            continue;
        }
        match byte {
            // C++'s `operator<` is a name, not a bracket.
            b'o' => {
                if let Some(end) = operator_end(bytes, at) {
                    at = end;
                    continue;
                }
            }
            b'<' | b'(' | b'[' | b'{' => depth += 1,
            b'>' if is_arrow(bytes, at) => {}
            b'>' | b')' | b']' | b'}' => match depth.checked_sub(1) {
                Some(outer) => depth = outer,
                None => return false,
            },
            _ => {}
        }
        at += 1;
    }
    depth == 0
}

/// Whether the `>` at `at` in `bytes` ends `->`, as in `fn() -> u8`,
/// which closes no bracket.
fn is_arrow(bytes: &[u8], at: usize) -> bool {
    at > 0 && bytes[at - 1] == b'-'
}

/// The bytes that open or close a bracket, and the `o` that starts
/// `operator`, by value.
const BRACKET_OR_OPERATOR: [bool; 256] = {
    let mut table = [false; 256];
    let bytes = b"<>()[]{}o";
    let mut at = 0;
    while at < bytes.len() {
        table[bytes[at] as usize] = true;
        at += 1;
    }
    table
};

/// When a C++ operator's name starts at `at`, as in `operator<` or
/// `operator->`, where its symbol ends. Brackets in the symbol don't pair
/// up with others.
fn operator_end(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes.get(at) != Some(&b'o') {
        return None;
    }
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let after = at + "operator".len();
    if !bytes[at..].starts_with(b"operator")
        || at
            .checked_sub(1)
            .is_some_and(|before| is_word(bytes[before]))
        || bytes.get(after).is_some_and(|&byte| is_word(byte))
    {
        return None;
    }
    let symbol = bytes[after..]
        .iter()
        .take_while(|byte| b"<>=!+-*/%^&|~,".contains(byte))
        .count();
    Some(after + symbol)
}

#[cfg(test)]
mod tests;
