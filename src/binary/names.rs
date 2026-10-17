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
    // `vtable for app::Gain` is also `vtable for Gain`.
    if let Some(prefix) = CPP_SPECIAL_PREFIXES
        .iter()
        .find(|prefix| name.starts_with(*prefix))
    {
        let mut short: Vec<String> = short_names(&name[prefix.len()..])
            .into_iter()
            .map(|short_name| format!("{prefix}{short_name}"))
            .collect();
        short.sort();
        return short;
    }
    // Only these make a name shorter: `::` in a path, `@plt`, `(` for
    // parameters, `<` for generic arguments and `[` for a clone suffix. Most
    // names, as in C, have none of them, and nothing to try.
    if !name.contains([':', '(', '<', '[']) && !name.ends_with("@plt") {
        return Vec::new();
    }

    // Each path adds the shorter paths that mean the same thing, until none
    // is left to add.
    let mut paths = vec![name.to_string()];
    let mut next = 0;
    while next < paths.len() {
        let path = &paths[next];
        let shorter: Vec<String> = [
            path.strip_suffix("@plt").map(str::to_string),
            without_clone_suffix(path),
            without_parameters(path),
        ]
        .into_iter()
        .flatten()
        .chain(impl_method_paths(path))
        .collect();
        for path in shorter {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        next += 1;
    }

    let mut short = Vec::new();
    for path in &paths {
        for parts in [split_path(path), split_path(&without_generics(path))] {
            let Some(parts) = parts else { continue };
            // Every end of the path, except one that starts with generic
            // arguments, like the `<u8>` in `checksum::<u8>`.
            for start in 0..parts.len() {
                if start > 0 && (parts[start].is_empty() || parts[start].starts_with('<')) {
                    continue;
                }
                short.push(parts[start..].join("::"));
            }
        }
    }
    short.retain(|short_name| short_name != name);
    short.sort();
    short.dedup();
    short
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
    let at = path.find(" [clone ")?;
    Some(path[..at].to_string())
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
                    return (!function.is_empty() && !function.ends_with("::"))
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
    let method = parts[1..].join("::");
    let owners = match top_level_matches(inner, " as ").as_deref() {
        Some([]) => vec![inner],
        Some(&[at]) => vec![&inner[..at], &inner[at + " as ".len()..]],
        _ => return Vec::new(),
    };
    owners
        .into_iter()
        .map(|owner| format!("{owner}::{method}"))
        .collect()
}

/// Splits `path` at each `::` outside brackets: `<a::B as c::D>::f` has two
/// parts, `<a::B as c::D>` and `f`. `None` when the brackets don't pair up.
fn split_path(path: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut start = 0;
    for at in top_level_matches(path, "::")? {
        parts.push(&path[start..at]);
        start = at + "::".len();
    }
    parts.push(&path[start..]);
    Some(parts)
}

/// `path` without generic arguments: `alloc::vec::Vec<u8>::drop` becomes
/// `alloc::vec::Vec::drop` and `app::checksum::<u8>` becomes
/// `app::checksum`. A leading `<Type as Trait>` stays.
fn without_generics(path: &str) -> String {
    let Some(parts) = split_path(path) else {
        return path.to_string();
    };
    let mut kept: Vec<String> = Vec::new();
    for (index, part) in parts.into_iter().enumerate() {
        if index == 0 && part.starts_with('<') {
            kept.push(part.to_string());
        } else if !part.starts_with('<') {
            kept.push(remove_angle_brackets(part));
        }
    }
    kept.join("::")
}

/// `text` without anything in angle brackets: `Vec<u8>` becomes `Vec`.
fn remove_angle_brackets(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut kept = String::new();
    let mut at = 0;
    while at < bytes.len() {
        // C++'s `operator<` is a name, not a bracket.
        if let Some(end) = operator_end(bytes, at) {
            if depth == 0 {
                kept.push_str(&text[at..end]);
            }
            at = end;
            continue;
        }
        let c = text[at..].chars().next().unwrap_or_default();
        match c {
            '<' => depth += 1,
            // The `>` of `->`, as in `fn() -> u8`, closes nothing.
            '>' if !text[..at].ends_with('-') && depth > 0 => depth -= 1,
            _ if depth == 0 => kept.push(c),
            _ => {}
        }
        at += c.len_utf8();
    }
    kept
}

/// Where `pattern` occurs in `text` outside any brackets, as byte offsets.
/// `None` when the brackets don't pair up.
fn top_level_matches(text: &str, pattern: &str) -> Option<Vec<usize>> {
    let bytes = text.as_bytes();
    let mut matches = Vec::new();
    let mut depth = 0usize;
    let mut at = 0;
    while at < bytes.len() {
        if depth == 0 && bytes[at..].starts_with(pattern.as_bytes()) {
            matches.push(at);
            at += pattern.len();
            continue;
        }
        // C++'s `operator<` is a name, not a bracket.
        if let Some(end) = operator_end(bytes, at) {
            at = end;
            continue;
        }
        match bytes[at] {
            b'<' | b'(' | b'[' | b'{' => depth += 1,
            // The `>` of `->`, as in `fn() -> u8`, closes nothing.
            b'>' if at > 0 && bytes[at - 1] == b'-' => {}
            b'>' | b')' | b']' | b'}' => depth = depth.checked_sub(1)?,
            _ => {}
        }
        at += 1;
    }
    (depth == 0).then_some(matches)
}

/// When a C++ operator's name starts at `at`, as in `operator<` or
/// `operator->`, where its symbol ends. Brackets in the symbol don't pair
/// up with others.
fn operator_end(bytes: &[u8], at: usize) -> Option<usize> {
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
}
