use super::*;

#[test]
fn copy_suffixes_come_off() {
    for (name, without) in [
        ("dev_read.constprop.0", "dev_read"),
        ("scale.llvm.123", "scale"),
        ("std::sort(int*) [clone .isra.0]", "std::sort(int*)"),
        ("f.constprop.0.isra.0", "f"),
        ("f.part.0", "f"),
        ("f.cold", "f"),
        // A static variable's number, alone, stays: blanks.1 and
        // blanks.2 are two variables.
        ("blanks.1", "blanks.1"),
        (".constprop.0", ".constprop.0"),
        ("main", "main"),
    ] {
        assert_eq!(without_copy_suffixes(name), without, "{name}");
    }
}

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
    assert!(short_names("app::Ptr::operator->() const").contains(&"Ptr::operator->".to_string()));
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
