use super::*;
use crate::commands::symbol::fixtures;

/// How `pattern` matches a function named `name` with `aliases`, or `None`
/// when it doesn't match.
fn matched(pattern: &str, name: &str, aliases: &[&str]) -> Option<Match> {
    let mut binary = fixtures::functions(&[name]);
    binary.items[0].aliases = aliases.iter().map(|alias| alias.to_string()).collect();
    let pattern = Pattern::parse(pattern).expect("a pattern");
    let mut matches = Names::new(&binary).matching(&pattern);
    assert!(matches.len() <= 1);
    matches.pop()
}

/// The match of the item at index `item`, by `matched_by` as `matched_as`.
fn found(item: usize, matched_by: MatchedBy, matched_as: &str) -> Match {
    Match {
        item,
        matched_by,
        matched_as: matched_as.to_string(),
    }
}

/// How a row of [`check`] expects a pattern to match a name, and the text
/// it matches as, or `None` when it doesn't.
type Expected = Option<(MatchedBy, &'static str)>;

/// Checks each of `rows`: a pattern, a name and how the pattern matches
/// it.
#[track_caller]
fn check(rows: &[(&str, &str, Expected)]) {
    for &(pattern, name, expected) in rows {
        let expected = expected.map(|(by, text)| found(0, by, text));
        assert_eq!(matched(pattern, name, &[]), expected, "{pattern} on {name}");
    }
}

use MatchedBy::{Alias, End, Name};

/// Names from real builds, each with the rule that decides whether a
/// pattern matches it: firmware in C, C++ and Rust, elfq itself, rustc's
/// driver library, and samples in testdata/bin.
#[test]
fn names_from_real_builds_match_by_the_rules() {
    const READ: &str = "drv::Uart::read(unsigned char*, unsigned int)";
    const WRITE_STR: &str = "<bigfw::Uart as core::fmt::Write>::write_str";
    const NEW_DEBUG: &str = "<std::backtrace_rs::symbolize::gimli::Mapping>::new_debug";
    const THUNK: &str =
        "virtual thunk to std::basic_ifstream<char, std::char_traits<char> >::~basic_ifstream()";
    check(&[
        ("on_*", "on_button", Some((Name, "on_button"))),
        // Case counts.
        ("On_*", "on_button", None),
        ("*_putc", "uart_putc", Some((Name, "uart_putc"))),
        // An end of the path, with the parameters.
        (
            "Uart::*",
            READ,
            Some((End, "Uart::read(unsigned char*, unsigned int)")),
        ),
        // The path without the parameters.
        ("*::read", READ, Some((Name, "drv::Uart::read"))),
        ("drv::Uart::read(*)", READ, Some((Name, READ))),
        // The first `*` is a pointer, part of the name.
        ("*(unsigned char*, *)", READ, Some((Name, READ))),
        // A `*` after a space stands for any text, and the name has no space
        // before its `*`.
        ("drv::Uart::read(unsigned char *, unsigned int)", READ, None),
        // A leading `::` matches only from the root.
        ("::Uart::*", READ, None),
        // A vtable is class data: a member of its class.
        (
            "drv::Uart::*",
            "vtable for drv::Uart",
            Some((Name, "drv::Uart::")),
        ),
        (
            "vtable for *",
            "vtable for drv::Uart",
            Some((Name, "vtable for drv::Uart")),
        ),
        // So is typeinfo.
        (
            "app::Gain::*",
            "typeinfo for app::Gain",
            Some((Name, "app::Gain::")),
        ),
        // A PLT stub is an item, with `@plt` in its name.
        ("write*", "write@plt", Some((Name, "write@plt"))),
        // And its path is without it.
        ("twi*e", "twice@plt", Some((Name, "twice"))),
        // A copy of a function has the function's texts.
        (
            "*_stdio_init",
            "global_stdio_init.part.0",
            Some((Name, "global_stdio_init")),
        ),
        ("*_l", "_strtol_l.isra.0", Some((Name, "_strtol_l"))),
        // A method in an impl block of the type alone is under the type.
        (
            "core::fmt::*",
            "<core::fmt::Formatter>::pad",
            Some((Name, "core::fmt::Formatter::pad")),
        ),
        // `u32` has no path, so its method is under its trait.
        (
            "core::fmt::*",
            "<u32 as core::fmt::Display>::fmt",
            Some((Name, "core::fmt::Display::fmt")),
        ),
        // `bigfw::Uart` has one, so its method is under it, not its trait.
        ("core::fmt::*", WRITE_STR, None),
        (
            "bigfw::Uart::*",
            WRITE_STR,
            Some((Name, "bigfw::Uart::write_str")),
        ),
        // The name as printed counts too.
        (
            "*as core::fmt::Write>::*",
            WRITE_STR,
            Some((Name, WRITE_STR)),
        ),
        // The name itself, with its generic arguments.
        (
            "*::<f64>",
            "core::fmt::float::float_to_decimal_common_exact::<f64>",
            Some((
                Name,
                "core::fmt::float::float_to_decimal_common_exact::<f64>",
            )),
        ),
        // An end of the path under the type.
        (
            "pad*",
            "<core::fmt::Formatter>::pad_integral::write_prefix",
            Some((End, "pad_integral::write_prefix")),
        ),
        (
            "gimli::*",
            NEW_DEBUG,
            Some((End, "gimli::Mapping::new_debug")),
        ),
        // Its path starts with `std::`.
        ("::gimli::*", NEW_DEBUG, None),
        // A thunk has the texts of the function it is for.
        (
            "std::basic_ifstream*",
            THUNK,
            Some((
                Name,
                "std::basic_ifstream<char, std::char_traits<char> >::~basic_ifstream()",
            )),
        ),
    ]);
    // Through other names: the longest text, and of those as long, the one
    // that sorts first, whatever the order of the names.
    let handlers = [
        "CAN1_TX_IRQHandler",
        "CAN1_SCE_IRQHandler",
        "CAN1_RX1_IRQHandler",
        "CAN1_RX0_IRQHandler",
    ];
    assert_eq!(
        matched("CAN1_*", "Default_Handler", &handlers),
        Some(found(0, Alias, "CAN1_RX0_IRQHandler"))
    );
    // Not through a mangled symbol, which elfq never prints.
    assert_eq!(matched("_ZN3drv*", READ, &["_ZN3drv4Uart4readEPhj"]), None);
    // Without `*`, it is a name.
    assert!(Pattern::parse("global_stdio_init").is_none());
}

#[test]
fn a_star_in_brackets_right_after_a_type_is_a_pointer() {
    for (typed, wildcards_at) in [
        ("read(unsigned char*, unsigned int)", vec![]),
        ("read(char**)", vec![]),
        ("read(std::vector<int>*)", vec![]),
        ("read(*)", vec![5]),
        ("read(char**, *)", vec![13]),
        ("read(int *)", vec![9]),
        // Outside brackets, every `*` stands for any text.
        ("operator*", vec![8]),
        ("*(char*)", vec![0]),
        ("a*::b", vec![1]),
        // A function pointer's `(*)`.
        ("f(void (*)(int))", vec![8]),
        // Each `(` opens a bracket that a `)` closes.
        ("f(g(x*))*", vec![8]),
        ("f(é*)", vec![]),
    ] {
        assert_eq!(wildcards(typed), wildcards_at, "{typed}");
    }
    assert!(Pattern::parse("read(char**)").is_none());
}

#[test]
fn a_pattern_matches_all_of_a_text() {
    let matches = |pattern: &str, text: &str| Pattern::parse(pattern).unwrap().matches(text);
    assert!(matches("*", ""));
    assert!(matches("a*b*c", "abc"));
    assert!(matches("a*b*c", "a-b-c"));
    assert!(matches("a*b*c", "abcabc"));
    assert!(!matches("a*b*c", "abcab"));
    assert!(!matches("a*b", "xab"));
    // The start and the end don't overlap.
    assert!(!matches("a*a", "a"));
    assert!(matches("*ab*ab", "abab"));
    assert!(!matches("*ab*ab", "ab"));
    assert!(matches("**", "anything"));
    // `*` crosses `::`, `<`, `(` and spaces.
    assert!(matches("a*z", "a::b<c>(d e)::z"));
}

#[test]
fn a_copy_of_a_function_has_the_functions_texts() {
    check(&[
        ("*_init", "x_init.part.0", Some((Name, "x_init"))),
        ("*_init", "x_init.isra.0", Some((Name, "x_init"))),
        (
            "*_init",
            "x_init.constprop.0.isra.0",
            Some((Name, "x_init")),
        ),
        ("*_init", "x_init.cold", Some((Name, "x_init"))),
        ("*_init", "x_init.lto_priv.0", Some((Name, "x_init"))),
        ("*_init", "x_init.llvm.4123", Some((Name, "x_init"))),
        ("*_init", "x_init.localalias", Some((Name, "x_init"))),
        (
            "*::log",
            "app::log(unsigned int) [clone .constprop.0]",
            Some((Name, "app::log")),
        ),
        (
            "log*)",
            "app::log(unsigned int) [clone .constprop.0]",
            Some((End, "log(unsigned int)")),
        ),
        // A static variable's number is part of its name.
        ("*s", "blanks.1", None),
    ]);
}

#[test]
fn class_data_is_a_member_of_its_class() {
    check(&[
        (
            "app::Gain::*",
            "vtable for app::Gain",
            Some((Name, "app::Gain::")),
        ),
        (
            "app::Both::*",
            "VTT for app::Both",
            Some((Name, "app::Both::")),
        ),
        (
            "app::Gain::*",
            "typeinfo name for app::Gain",
            Some((Name, "app::Gain::")),
        ),
        (
            "app::Gain::*",
            "typeinfo fn for app::Gain",
            Some((Name, "app::Gain::")),
        ),
        ("Gain::*", "vtable for app::Gain", Some((End, "Gain::"))),
        (
            "vtable for G*",
            "vtable for app::Gain",
            Some((End, "vtable for Gain")),
        ),
        // Class data is in the class, and isn't the class.
        ("app::G*n", "vtable for app::Gain", None),
        // The others have the texts of what they are for.
        (
            "app::Sensor::*",
            "non-virtual thunk to app::Sensor::name()",
            Some((Name, "app::Sensor::name()")),
        ),
        (
            "app::*::name",
            "non-virtual thunk to app::Sensor::name()",
            Some((Name, "app::Sensor::name")),
        ),
        // With the prefix kept.
        (
            "*::name",
            "non-virtual thunk to app::Sensor::name()",
            Some((Name, "non-virtual thunk to app::Sensor::name")),
        ),
        (
            "app::f*",
            "guard variable for app::f()::x",
            Some((Name, "app::f()::x")),
        ),
        (
            "app::*",
            "TLS wrapper function for app::counter",
            Some((Name, "app::counter")),
        ),
        (
            "app::*",
            "TLS init function for app::counter",
            Some((Name, "app::counter")),
        ),
        (
            "app::*",
            "covariant return thunk to app::Sensor::clone() const",
            Some((Name, "app::Sensor::clone() const")),
        ),
    ]);
}

#[test]
fn a_method_in_an_impl_block_counts_under_its_owner_only() {
    check(&[
        // The type, without what comes before it.
        (
            "app::Gain::*",
            "<app::Gain as app::Stage>::run",
            Some((Name, "app::Gain::run")),
        ),
        ("app::Stage::*", "<app::Gain as app::Stage>::run", None),
        (
            "bigfw::Uart::*",
            "<&bigfw::Uart as core::fmt::Write>::write_str",
            Some((Name, "bigfw::Uart::write_str")),
        ),
        (
            "bigfw::Uart::*",
            "<&mut bigfw::Uart as core::fmt::Write>::write_str",
            Some((Name, "bigfw::Uart::write_str")),
        ),
        (
            "app::Node::*",
            "<*const app::Node as core::fmt::Pointer>::fmt",
            Some((Name, "app::Node::fmt")),
        ),
        (
            "app::Node::*",
            "<*mut app::Node as core::fmt::Pointer>::fmt",
            Some((Name, "app::Node::fmt")),
        ),
        (
            "app::Stage::*",
            "<dyn app::Stage as core::fmt::Debug>::fmt",
            Some((Name, "app::Stage::fmt")),
        ),
        (
            "core::any::Any::*",
            "<dyn core::any::Any>::type_id",
            Some((Name, "core::any::Any::type_id")),
        ),
        // The trait, for a type with no path of its own.
        (
            "core::fmt::*",
            "<str as core::fmt::Display>::fmt",
            Some((Name, "core::fmt::Display::fmt")),
        ),
        (
            "core::fmt::*",
            "<[u8] as core::fmt::Debug>::fmt",
            Some((Name, "core::fmt::Debug::fmt")),
        ),
        (
            "core::fmt::*",
            "<(u8, u16) as core::fmt::Debug>::fmt",
            Some((Name, "core::fmt::Debug::fmt")),
        ),
        (
            "core::fmt::*",
            "<&str as core::fmt::Display>::fmt",
            Some((Name, "core::fmt::Display::fmt")),
        ),
        ("u32::*", "<u32 as core::fmt::Display>::fmt", None),
        // With and without generic arguments.
        (
            "alloc::vec::Vec::*",
            "<alloc::vec::Vec<u8> as core::ops::drop::Drop>::drop",
            Some((Name, "alloc::vec::Vec::drop")),
        ),
        (
            "core::ops::*",
            "<alloc::vec::Vec<u8> as core::ops::drop::Drop>::drop",
            None,
        ),
    ]);
}

#[test]
fn a_leading_double_colon_matches_only_from_the_root() {
    const READ: &str = "drv::Uart::read(unsigned char*, unsigned int)";
    check(&[
        ("::drv::Uart::*", READ, Some((Name, READ))),
        ("::*::read", READ, Some((Name, "drv::Uart::read"))),
        ("::Uart::*", READ, None),
        ("::*", READ, Some((Name, READ))),
        (
            "::app::Gain::*",
            "vtable for app::Gain",
            Some((Name, "app::Gain::")),
        ),
        ("::Gain::*", "vtable for app::Gain", None),
    ]);
    // Not through an end of another name either.
    assert_eq!(
        matched("::IRQ*", "Default_Handler", &["vectors::IRQ0"]),
        None
    );
    assert_eq!(
        matched("IRQ*", "Default_Handler", &["vectors::IRQ0"]),
        Some(found(0, Alias, "IRQ0"))
    );
    let pattern = Pattern::parse("::gimli::*").unwrap();
    assert_eq!(pattern.typed(), "::gimli::*");
}

#[test]
fn mangled_aliases_dont_count() {
    // Rust's two manglings, C++'s, and a PLT stub's.
    for (pattern, alias) in [
        ("_ZN4core*", "_ZN4core3fmt5write17h0123456789abcdefE"),
        ("_RNv*", "_RNvCsjtrlrMDykj5_11elfq_sample13process_frame"),
        ("_ZN3app*", "_ZN3app3logEj"),
        ("_ZN3app*", "_ZN3app3logEj@plt"),
    ] {
        assert_eq!(matched(pattern, "readable", &[alias]), None, "{alias}");
    }
    // A symbol that only starts as a mangled one does is a name.
    assert_eq!(
        matched("_ZERO*", "zero_bss", &["_ZERO_BSS"]),
        Some(found(0, Alias, "_ZERO_BSS"))
    );
}

#[test]
fn a_pattern_picks_items_with_plt_stubs_and_never_imports() {
    let mut binary = fixtures::functions(&["write@plt", "main", "twice", "twice@plt", "writer"]);
    binary.items[4].aliases.push("write_alias".to_string());
    binary.imports.push("write".to_string());
    binary.imports.push("twine".to_string());
    let names = Names::new(&binary);
    let matching = |pattern: &str| names.matching(&Pattern::parse(pattern).unwrap());
    // In address order.
    assert_eq!(
        matching("write*"),
        [found(0, Name, "write@plt"), found(4, Name, "writer")]
    );
    assert_eq!(
        matching("twi*e"),
        [found(2, Name, "twice"), found(3, Name, "twice")]
    );
    assert_eq!(matching("*@plt").len(), 2);
    assert_eq!(matching("*_alias"), [found(4, Alias, "write_alias")]);
}

#[test]
fn taking_out_generic_arguments_can_join_words() {
    check(&[(
        "*FrameRef::fmt",
        "app::Frame<u8>Ref::fmt",
        Some((Name, "app::FrameRef::fmt")),
    )]);
}

/// Names that reach each case of the texts and of the quick check.
const NAMES: [&str; 24] = [
    "h20777",
    "app::h20777",
    "a77::b",
    "x_init.part.0",
    "on_button",
    "write@plt",
    "drv::Uart::read(unsigned char*, unsigned int)",
    "app::log(unsigned int) [clone .constprop.0]",
    "vtable for app::Gain",
    "typeinfo name for app::Gain",
    "non-virtual thunk to app::Sensor::name()",
    "guard variable for app::f()::x",
    "<app::Gain as app::Stage>::run",
    "<&mut bigfw::Uart as core::fmt::Write>::write_str",
    "<u32 as core::fmt::Display>::fmt",
    "<dyn core::any::Any>::type_id",
    "<alloc::vec::Vec<u8> as core::ops::drop::Drop>::drop",
    "<core::fmt::Formatter>::pad_integral::write_prefix",
    "core::fmt::float::float_to_decimal_common_exact::<f64>",
    "app::checksum<unsigned char>(unsigned char const*, unsigned long)",
    "app::Frame<u8>Ref::fmt",
    "app::grüße::fmt",
    "operator new(unsigned long)@plt",
    "app::Run::operator()(int)",
];

#[test]
fn the_quick_check_passes_every_name_that_a_pattern_matches() {
    for name in NAMES {
        let mut texts = Vec::new();
        names::for_each_pattern_text(name, |text, _| texts.push(text.to_string()));
        for text in &texts {
            // Patterns that match `text`, made from it: with `*` put in at
            // each place, and around each short piece of it.
            let places: Vec<usize> = (0..=text.len())
                .filter(|&at| text.is_char_boundary(at))
                .collect();
            let mut patterns = Vec::new();
            for &at in &places {
                let (before, after) = text.split_at(at);
                patterns.extend([
                    format!("{before}*"),
                    format!("*{after}"),
                    format!("{before}*{after}"),
                ]);
                for &end in places.iter().filter(|&&end| end >= at && end <= at + 8) {
                    patterns.push(format!("*{}*", &text[at..end]));
                }
            }
            let mut matching = 0;
            for typed in &patterns {
                // A `*` put in may be a pointer, so that the pattern
                // doesn't match.
                let Some(pattern) = Pattern::parse(typed) else {
                    continue;
                };
                if pattern.matches(text) {
                    matching += 1;
                    assert!(
                        pattern.quick_check.may_match(name),
                        "{typed:?} matches {text:?} of {name:?}"
                    );
                }
            }
            // Most of the patterns made match, so that the loop above
            // checked something, even where some `*`s are pointers.
            assert!(matching > places.len(), "{text:?} of {name:?}");
        }
    }
}

#[test]
fn the_quick_check_rules_out_names_without_the_patterns_words() {
    let may_match =
        |pattern: &str, name: &str| Pattern::parse(pattern).unwrap().quick_check.may_match(name);
    // `77` ends a word of `h20777` at its second place.
    assert!(may_match("*77", "h20777"));
    assert!(!may_match("*77", "h2077x"));
    assert!(may_match("h2*", "a::h20777"));
    assert!(!may_match("20*", "h20777"));
    assert!(!may_match("on_*", "button_on"));
    assert!(!may_match("*uart*", "on_button"));
    assert!(may_match("*Uart::read*", "drv::Uart::read(unsigned char*)"));
    // A name whose words may join passes.
    assert!(may_match("*FrameRef*", "app::Frame<u8>Ref::fmt"));
}
