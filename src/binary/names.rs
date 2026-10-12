//! Symbol names: making mangled names readable, and the shorter names that
//! people type for long ones.
//!
//! Rust compilers encode a function's full path into its symbol
//! ("mangling"): `app::process_frame` becomes something like
//! `_RNvCsjtrlrMDykj5_3app13process_frame`. elfq shows the readable name and
//! keeps the symbol as an alias, so both find the item.

/// The readable name of a mangled Rust symbol, or `None` when `symbol` isn't
/// one. The name leaves out the hashes that tell builds of a crate apart.
pub fn demangle(symbol: &str) -> Option<String> {
    let name = rustc_demangle::try_demangle(symbol).ok()?;
    Some(format!("{name:#}"))
}

/// The shorter names that also mean `name`: the ends of its path, with and
/// without generic arguments. A method in an impl block can also be named
/// through its type or its trait, and a PLT stub through its import.
///
/// For `<app::Gain as app::Stage>::run` they are `run`, `Gain::run`,
/// `app::Gain::run`, `Stage::run` and `app::Stage::run`. For
/// `app::checksum::<u8>` they are `checksum`, `checksum::<u8>` and
/// `app::checksum`. For `write@plt`, it is `write`.
pub fn short_names(name: &str) -> Vec<String> {
    let mut paths = vec![name.to_string()];
    if let Some(import) = name.strip_suffix("@plt") {
        paths.push(import.to_string());
    }
    // Each path that is an impl block's method adds the paths through its
    // type and trait. Those are shorter, so this ends.
    let mut next = 0;
    while next < paths.len() {
        let through = impl_method_paths(&paths[next]);
        paths.extend(through);
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
    let mut depth = 0usize;
    let mut previous = None;
    let mut kept = String::new();
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            // The `>` of `->`, as in `fn() -> u8`, closes nothing.
            '>' if previous != Some('-') && depth > 0 => depth -= 1,
            _ if depth == 0 => kept.push(c),
            _ => {}
        }
        previous = Some(c);
    }
    kept
}

/// Where `pattern` occurs in `text` outside any brackets, as byte offsets.
/// `None` when the brackets don't pair up, as in C++'s `operator<`.
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
        assert_eq!(demangle("_ZN3foo3barEv"), None);
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
    fn unpaired_brackets_give_no_short_names() {
        // A C++ operator, which isn't demangled yet but could be.
        assert_eq!(short_names("ns::operator<"), Vec::<String>::new());
    }
}
