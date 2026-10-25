//! Symbol names: making mangled names readable, and the shorter names that
//! people type for long ones.
//!
//! Rust and C++ compilers encode a function's full path into its symbol
//! ("mangling"): Rust's `app::process_frame` becomes something like
//! `_RNvCsjtrlrMDykj5_3app13process_frame`, and C++'s
//! `app::process_frame(app::Frame const&)` becomes
//! `_ZN3app13process_frameERKNS_5FrameE`. elfq shows the readable name and
//! keeps the symbol as an alias, so both find the item.

/// The readable name of a mangled Rust or C++ symbol, or `None` when
/// `symbol` isn't one. Rust names leave out the hashes that tell builds of a
/// crate apart.
pub fn demangle(symbol: &str) -> Option<String> {
    if let Ok(name) = rustc_demangle::try_demangle(symbol) {
        return Some(format!("{name:#}"));
    }
    demangle_cpp(symbol)
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
pub fn has_short_name(name: &str, short_name: &str) -> bool {
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
pub fn for_each_short_name(name: &str, mut visit: impl FnMut(&str, Option<usize>)) {
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
    if let Some(prefix) = CPP_SPECIAL_PREFIXES
        .iter()
        .find(|prefix| name.starts_with(*prefix))
    {
        for_each_short_name_ending(&name[prefix.len()..], key, &mut |short_name, _| {
            visit(&[prefix, short_name].concat(), None);
        });
        return;
    }
    for_each_path(name, key, |path, parts, in_name| {
        let last = parts[parts.len() - 1];
        // A short name's key is the last part of the path it ends, or
        // when that part is the whole short name, the part without
        // special prefixes.
        if key.is_some_and(|key| last != key && without_special_prefixes(last) != key) {
            return;
        }
        // Every end of the path, except one that starts with generic
        // arguments, like the `<u8>` in `checksum::<u8>`. Each is the
        // path from where its first part starts.
        let mut at = 0;
        for start in 0..parts.len() {
            let from = at;
            at += parts[start].len() + "::".len();
            if start > 0 && (parts[start].is_empty() || parts[start].starts_with('<')) {
                continue;
            }
            if let Some(key) = key {
                let its_key = match start + 1 == parts.len() {
                    true => without_special_prefixes(last),
                    false => last,
                };
                if its_key != key {
                    continue;
                }
            }
            visit(&path[from..], in_name.then_some(from));
        }
    });
}

/// Whether `short_name` is one of `name`'s [`short_names`] because `name`
/// ends with it, from one of the parts of its path, or a method in an impl
/// block does through its type or trait: `Gain::run` of `app::Gain::run`
/// or of `<app::Gain as app::Stage>::run`. A quick check for a short name:
/// when it says no, `name` may still have it.
pub fn ends_with_short_name(name: &str, short_name: &str) -> bool {
    // A special name's short names keep its prefix.
    if CPP_SPECIAL_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || !can_be_shorter(name)
    {
        return false;
    }
    // `name` and the paths of a method through its type and trait are
    // among the paths that `for_each_path` visits, and each of their ends
    // from a part that isn't empty and doesn't start with generic arguments
    // is a short name, but `name` itself.
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
                return index == 0 || !part.is_empty() && !part.starts_with('<');
            }
            at += part.len() + "::".len();
        }
        false
    };
    ends_with(name) || impl_method_paths(name).iter().any(|path| ends_with(path))
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

/// Whether taking out generic arguments from `name` may join two of its
/// words into one: when a `>` comes right before a letter, digit or `_`.
pub fn may_join_words(name: &str) -> bool {
    name.split('>')
        .skip(1)
        .any(|after| after.bytes().next().is_some_and(in_word))
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
    for_each_path(end, None, |_, parts, _| add(parts[parts.len() - 1]));
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
/// mean the same, each with and without generic arguments. With each, it
/// says whether the path is the start of `name`, so that its ends are
/// parts of `name`. With `ending`, it leaves out paths that don't end with
/// it.
fn for_each_path(name: &str, ending: Option<&str>, mut visit: impl FnMut(&str, &[&str], bool)) {
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
            impl_method_paths(path)
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
pub fn can_be_shorter(name: &str) -> bool {
    // Looking for each character on its own is quicker than for any of
    // them at once.
    [':', '(', '<', '['].into_iter().any(|c| name.contains(c)) || name.ends_with("@plt")
}

/// `name` without the special C++ prefixes that it starts with.
fn without_special_prefixes(mut name: &str) -> &str {
    while let Some(prefix) = CPP_SPECIAL_PREFIXES
        .iter()
        .find(|prefix| name.starts_with(*prefix))
    {
        name = &name[prefix.len()..];
    }
    name
}

/// The special names that C++ compilers give to the data and code they make
/// for a class or a function. Short names keep the prefix.
const CPP_SPECIAL_PREFIXES: &[&str] = &[
    "vtable for ",
    "VTT for ",
    "typeinfo for ",
    "typeinfo name for ",
    "typeinfo fn for ",
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

/// For a method in an impl block, written `<Type as Trait>::method` or
/// `<Type>::method`, the paths `Type::method` and `Trait::method`.
fn impl_method_paths(path: &str) -> Vec<String> {
    if !path.starts_with('<') {
        return Vec::new();
    }
    let Some(parts) = split_path(path) else {
        return Vec::new();
    };
    let Some(inner) = parts[0]
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
    else {
        return Vec::new();
    };
    if parts.len() < 2 {
        return Vec::new();
    }
    let method = &path[parts[0].len()..];
    let owners = match top_level_matches(inner, " as ").as_deref() {
        Some([]) => vec![inner],
        Some(&[at]) => vec![&inner[..at], &inner[at + " as ".len()..]],
        _ => return Vec::new(),
    };
    owners
        .into_iter()
        .map(|owner| [owner, method].concat())
        .collect()
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
            // The `>` of `->`, as in `fn() -> u8`, closes nothing.
            b'>' if depth > 0 && (at == 0 || bytes[at - 1] != b'-') => {
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
            // The `>` of `->`, as in `fn() -> u8`, closes nothing.
            b'>' if at > 0 && bytes[at - 1] == b'-' => {}
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
mod tests {
    use super::*;

    #[test]
    fn rust_symbols_are_demangled_without_hashes() {
        // Rust's v0 mangling, and the older one based on C++'s.
        assert_eq!(
            demangle("_RNvCsjtrlrMDykj5_11elfq_sample13process_frame").as_deref(),
            Some("elfq_sample::process_frame")
        );
        assert_eq!(
            demangle("_ZN4core3fmt5write17h0123456789abcdefE").as_deref(),
            Some("core::fmt::write")
        );
        assert_eq!(demangle("process_frame"), None);
    }

    #[test]
    fn cpp_symbols_are_demangled_without_return_types() {
        let demangled = |symbol| demangle(symbol).unwrap();
        assert_eq!(demangled("_ZN3foo3barEv"), "foo::bar()");
        assert_eq!(
            demangled("_ZN3app8checksumIhEEjPKT_m"),
            "app::checksum<unsigned char>(unsigned char const*, unsigned long)"
        );
        assert_eq!(
            demangled("_ZN3app3logEj.constprop.0"),
            "app::log(unsigned int) [clone .constprop.0]"
        );
        // Special names, written as c++filt writes them.
        assert_eq!(demangled("_ZTVN3app4GainE"), "vtable for app::Gain");
        assert_eq!(demangled("_ZTIN3app4GainE"), "typeinfo for app::Gain");
        assert_eq!(demangled("_ZTTN3app4BothE"), "VTT for app::Both");
        assert_eq!(
            demangled("_ZThn8_N3app6Sensor4nameEv"),
            "non-virtual thunk to app::Sensor::name()"
        );
        assert_eq!(
            demangled("_ZTv0_n24_N3app6Sensor4nameEv"),
            "virtual thunk to app::Sensor::name()"
        );
    }

    #[test]
    fn short_names_are_the_ends_of_a_path() {
        assert_eq!(short_names("elfq_sample::process_frame"), ["process_frame"]);
        assert_eq!(short_names("process_frame"), Vec::<String>::new());
        assert_eq!(short_names("write@plt"), ["write"]);
    }

    #[test]
    fn methods_can_be_named_through_their_type_or_trait() {
        assert_eq!(
            short_names("<app::Gain as app::Stage>::run"),
            [
                "Gain::run",
                "Stage::run",
                "app::Gain::run",
                "app::Stage::run",
                "run"
            ]
        );
        assert_eq!(
            short_names("<core::fmt::Formatter>::pad_integral"),
            [
                "Formatter::pad_integral",
                "core::fmt::Formatter::pad_integral",
                "fmt::Formatter::pad_integral",
                "pad_integral"
            ]
        );
    }

    #[test]
    fn generic_arguments_can_be_left_out() {
        assert_eq!(
            short_names("app::checksum::<u8>"),
            ["app::checksum", "checksum", "checksum::<u8>"]
        );
        assert!(
            short_names("<alloc::vec::Vec<u8> as core::ops::drop::Drop>::drop")
                .contains(&"Vec::drop".to_string())
        );
        assert!(
            short_names("<fn() -> u8 as core::ops::FnOnce<()>>::call_once")
                .contains(&"FnOnce::call_once".to_string())
        );
    }

    #[test]
    fn cpp_functions_can_be_named_without_their_parameters() {
        assert_eq!(
            short_names("app::Gain::run(short*, unsigned long) const"),
            [
                "Gain::run",
                "Gain::run(short*, unsigned long) const",
                "app::Gain::run",
                "run",
                "run(short*, unsigned long) const"
            ]
        );
        assert!(
            short_names("app::checksum<unsigned char>(unsigned char const*, unsigned long)")
                .contains(&"checksum".to_string())
        );
        assert!(
            short_names("app::log(unsigned int) [clone .constprop.0]").contains(&"log".to_string())
        );
        assert_eq!(
            short_names("operator new(unsigned long)@plt"),
            ["operator new", "operator new(unsigned long)"]
        );
    }

    #[test]
    fn cpp_operators_are_names_not_brackets() {
        assert_eq!(
            short_names("app::operator<(app::Reading const&, app::Reading const&)"),
            [
                "app::operator<",
                "operator<",
                "operator<(app::Reading const&, app::Reading const&)"
            ]
        );
        assert!(short_names("app::Stream::operator<<(int)").contains(&"operator<<".to_string()));
        assert!(
            short_names("app::Ptr::operator->() const").contains(&"Ptr::operator->".to_string())
        );
        assert!(short_names("app::Run::operator()(int)").contains(&"Run::operator()".to_string()));
        let call = short_names("Op::operator()(unsigned int) const");
        assert!(call.contains(&"Op::operator()".to_string()), "{call:?}");
        assert!(
            !call.iter().any(|name| name.ends_with("operator")),
            "{call:?}"
        );
    }

    #[test]
    fn cpp_special_names_keep_their_prefix() {
        assert_eq!(short_names("vtable for app::Gain"), ["vtable for Gain"]);
        assert_eq!(
            short_names("non-virtual thunk to app::Sensor::name()"),
            [
                "non-virtual thunk to Sensor::name",
                "non-virtual thunk to Sensor::name()",
                "non-virtual thunk to app::Sensor::name",
                "non-virtual thunk to name",
                "non-virtual thunk to name()"
            ]
        );
    }

    #[test]
    fn unpaired_brackets_give_no_short_names() {
        assert_eq!(short_names("app::Vec<u8::new"), Vec::<String>::new());
    }

    /// Names that reach each case of the keys, as clone suffixes and
    /// parameters do before the end of a path, which no sample has.
    const NAMES: [&str; 31] = [
        "app::log(unsigned int) [clone .constprop.0]",
        "app::f(int) [clone .constprop.0]::b",
        "a::f() [clone .cold]::g",
        "ns::f(void (*)(int))",
        "ns::f(a>::b<c)(int)",
        "(anonymous namespace)::a::(anonymous namespace)::b(void (*)(void (*)(int)), int)",
        "(anonymous namespace)::helper()",
        "operator<",
        "app::operator<(app::Reading const&, app::Reading const&)",
        "app::Stream::operator<<(int)",
        "app::Run::operator()(int)",
        "app::Ptr::operator->() const",
        "{lambda()#1}::operator()() const",
        "app::f()::{lambda(int)#1}::operator()(int) const",
        "vtable for app::Gain",
        "a::vtable for B",
        "typeinfo name for app::Gain",
        "guard variable for app::f()::x",
        "non-virtual thunk to app::Sensor::name()",
        "<app::Gain as app::Stage>::run",
        "<app::Gain>::run::<u8>",
        "app::checksum::<u8>",
        "app::main::{closure#0}",
        "<fn() -> u8 as core::ops::FnOnce<()>>::call_once",
        "<alloc::vec::Vec<u8> as core::ops::drop::Drop>::drop",
        "std::vector<int, std::allocator<int> >::push_back(int const&)",
        "write@plt",
        "app::log(unsigned int)@plt",
        "operator new(unsigned long)@plt",
        "app::Vec<u8::new",
        "process_frame",
    ];

    #[test]
    fn short_names_are_filed_under_keys_that_find_them() {
        for name in NAMES {
            let short = short_names(name);
            let keys = short_name_keys(name);
            for short_name in &short {
                let key = short_name_key(short_name);
                assert!(
                    key.is_some_and(|key| keys.iter().any(|k| k == key)),
                    "{short_name:?} of {name:?} has key {key:?}, not one of {keys:?}"
                );
            }
            for key in &keys {
                let with_key: Vec<&String> = short
                    .iter()
                    .filter(|short_name| short_name_key(short_name) == Some(key.as_str()))
                    .collect();
                assert_eq!(
                    short_names_with_key(name, key).iter().collect::<Vec<_>>(),
                    with_key,
                    "{name:?}, key {key:?}"
                );
            }
        }
    }

    #[test]
    fn every_short_name_is_visited() {
        for name in NAMES {
            let mut visited = Vec::new();
            for_each_short_name(name, |short_name, at| {
                if short_name != name {
                    visited.push(short_name.to_string());
                }
                if let Some(at) = at {
                    assert!(
                        name[at..].starts_with(short_name),
                        "{short_name:?} of {name:?}"
                    );
                }
            });
            visited.sort();
            visited.dedup();
            assert_eq!(visited, short_names(name), "{name:?}");
        }
    }

    #[test]
    fn the_words_of_a_short_name_are_words_of_the_name() {
        assert_eq!(
            words("<a::Vec<u8> as b>::f_1(é)").collect::<Vec<_>>(),
            ["a", "Vec", "u8", "as", "b", "f_1", "é"]
        );
        assert!(may_join_words("a<b>c"));
        assert!(!may_join_words("a<b>::c"));
        for name in NAMES {
            let of_name: Vec<&str> = words(name).collect();
            for short_name in short_names(name) {
                if !may_join_words(name) {
                    for word in words(&short_name) {
                        assert!(
                            of_name.contains(&word),
                            "{word:?} of {short_name:?} isn't a word of {name:?}"
                        );
                    }
                }
                if ends_with_short_name(name, &short_name) {
                    continue;
                }
                // Only the short names that a name ends with, through its
                // type or trait too, can be found without working them all
                // out.
                assert!(
                    !name.ends_with(short_name.as_str())
                        || CPP_SPECIAL_PREFIXES
                            .iter()
                            .any(|prefix| name.starts_with(prefix)),
                    "{short_name:?} of {name:?}"
                );
            }
            let not_short = format!("{name}x");
            assert!(!ends_with_short_name(name, &not_short), "{name:?}");
            assert!(!ends_with_short_name(name, name), "{name:?}");
        }
        assert!(ends_with_short_name(
            "<app::Gain as app::Stage>::run",
            "Stage::run"
        ));
        assert!(!ends_with_short_name("app::Gain::run", "ain::run"));
    }
}
