//! Finding what the user means by SYMBOL.

use elfq::binary::{Binary, short_name_key, short_name_keys, short_names};
use elfq::commands::symbol;

use crate::support::{load_sample, samples};

/// The name of what `query` finds.
fn found(binary: &Binary, query: &str) -> String {
    symbol::find(binary, query)
        .unwrap_or_else(|err| panic!("{query}: {err}"))
        .node
        .name(binary)
        .to_string()
}

/// The name of what `query` finds as the answer's first line gives it.
fn lead_name(binary: &Binary, query: &str) -> String {
    symbol::find(binary, query)
        .unwrap_or_else(|err| panic!("{query}: {err}"))
        .lead_name(binary)
}

fn error(binary: &Binary, query: &str) -> String {
    symbol::find(binary, query).unwrap_err().to_string()
}

#[test]
fn names_addresses_and_imports() {
    let binary = load_sample("c-x86_64.elf");
    assert_eq!(found(&binary, "process_frame"), "process_frame");
    assert_eq!(found(&binary, "0x1700"), "process_frame");
    assert_eq!(found(&binary, "0x1705"), "process_frame");
    assert_eq!(found(&binary, "0X1705"), "process_frame");
    // An imported function means its stub, which is what code calls.
    assert_eq!(found(&binary, "write"), "write@plt");
    assert_eq!(found(&binary, "__libc_start_main"), "__libc_start_main");

    assert_eq!(
        error(&binary, "0x10"),
        "no function or data object contains address 0x10"
    );
}

#[test]
fn the_first_line_says_what_symbol_gave_when_it_isnt_the_name() {
    let binary = load_sample("c-x86_64.elf");
    assert_eq!(lead_name(&binary, "process_frame"), "process_frame");
    assert_eq!(lead_name(&binary, "0x1700"), "process_frame");
    assert_eq!(
        lead_name(&binary, "0x1705"),
        "process_frame (which holds 0x1705)"
    );
    // An import's name gives its stub, whose name says so.
    assert_eq!(lead_name(&binary, "write"), "write@plt");
    // A Thumb function's start, with bit 0 set as its symbol has it, is
    // its start.
    let binary = load_sample("c-cortex-m4.elf");
    assert_eq!(lead_name(&binary, "0x80002c9"), "process_frame");
    assert_eq!(
        lead_name(&binary, "0x80002ca"),
        "process_frame (which holds 0x80002ca)"
    );

    // A weak alias is another name.
    let binary = load_sample("c-cortex-m4.elf");
    assert_eq!(
        lead_name(&binary, "NMI_Handler"),
        "Default_Handler (also named NMI_Handler)"
    );
    let binary = load_sample("c-plt.so");
    assert_eq!(
        lead_name(&binary, "square_resolver@plt"),
        "square@plt (also named square_resolver@plt)"
    );

    // A mangled symbol and a short name are the item's own name.
    let binary = load_sample("cpp-x86_64.elf");
    let run = "app::Gain::run(short*, unsigned long)";
    assert_eq!(lead_name(&binary, "_ZN3app4Gain3runEPsm"), run);
    assert_eq!(lead_name(&binary, "Gain::run"), run);
}

#[test]
fn a_shared_librarys_function_is_not_its_stub() {
    let binary = load_sample("c-x86_64.so");
    assert_eq!(found(&binary, "log_msg"), "log_msg");
    assert_eq!(found(&binary, "log_msg@plt"), "log_msg@plt");
    // A typo suggests only the function, not its stub.
    assert_eq!(
        error(&binary, "log_mgs"),
        "no function or data object is named `log_mgs`; did you mean log_msg?"
    );
    assert_eq!(
        error(&binary, "log_ms"),
        "no function or data object is named `log_ms`; did you mean log_msg?"
    );

    // In C++ too, the end of a name means the function, while the end of
    // its stub's name, with `@plt`, means the stub.
    let binary = load_sample("cpp-x86_64.so");
    assert_eq!(
        found(&binary, "process_frame"),
        "app::process_frame(app::Frame const&, app::Stage&)"
    );
    assert_eq!(
        error(&binary, "log"),
        "`log` could mean 2 functions; use one on the left instead:
  'log(char const*)'   app::log(char const*)
  'log(unsigned int)'  app::log(unsigned int)"
    );
    assert_eq!(
        found(&binary, "log(unsigned int)@plt"),
        "app::log(unsigned int)@plt"
    );
}

#[test]
fn the_end_of_a_rust_path_is_enough() {
    let binary = load_sample("rust-x86_64-nodebug.elf");
    assert_eq!(
        found(&binary, "process_frame"),
        "elfq_sample::process_frame"
    );
    assert_eq!(
        found(&binary, "elfq_sample::process_frame"),
        "elfq_sample::process_frame"
    );
    // The mangled symbol still works, for names copied from other tools.
    assert_eq!(
        found(&binary, "_RNvCsjtrlrMDykj5_11elfq_sample13process_frame"),
        "elfq_sample::process_frame"
    );
    // A trait method, through its type.
    let gain_run = "<elfq_sample::Gain as elfq_sample::Stage>::run";
    assert_eq!(found(&binary, "Gain::run"), gain_run);
    assert_eq!(found(&binary, gain_run), gain_run);
    // One copy of a generic function.
    assert_eq!(
        found(&binary, "checksum::<u16>"),
        "elfq_sample::checksum::<u16>"
    );
}

#[test]
fn an_exact_name_beats_the_end_of_a_path() {
    let binary = load_sample("rust-x86_64-nodebug.elf");
    // The C `main` that the Rust runtime provides, not `elfq_sample::main`.
    assert_eq!(found(&binary, "main"), "main");
    // The `write` imported from the C library, not `core::fmt::write`.
    assert_eq!(found(&binary, "write"), "write");
}

#[test]
fn a_name_that_could_mean_several_items_lists_them() {
    let binary = load_sample("rust-x86_64-nodebug.elf");
    assert_eq!(
        error(&binary, "checksum"),
        "`checksum` could mean 3 functions; use one on the left instead:
  'checksum::<u8>'   elfq_sample::checksum::<u8>
  'checksum::<u32>'  elfq_sample::checksum::<u32>
  'checksum::<u16>'  elfq_sample::checksum::<u16>"
    );
    assert_eq!(
        error(&binary, "Stage::run"),
        "`Stage::run` could mean 2 functions; use one on the left instead:
  Gain::run     <elfq_sample::Gain as elfq_sample::Stage>::run
  DcBlock::run  <elfq_sample::DcBlock as elfq_sample::Stage>::run"
    );
    // Two functions with the same name, from two copies of the standard
    // library's formatting code: only an address tells them apart.
    let display = "<&str as core::fmt::Display>::fmt";
    let message = error(&binary, display);
    assert!(
        message
            .lines()
            .next()
            .unwrap()
            .ends_with("; use one on the left instead:"),
        "{message}"
    );
    let choices: Vec<&str> = message.lines().skip(1).collect();
    assert_eq!(choices.len(), 2);
    for choice in choices {
        let (address, name) = choice.trim().split_once("  ").unwrap();
        assert!(address.starts_with("0x"), "{choice}");
        assert_eq!(name, display);
        assert_eq!(found(&binary, address), display);
    }
}

#[test]
fn a_name_longer_than_40_characters_gives_way_to_an_address() {
    let binary = load_sample("rust-x86_64-nodebug.elf");
    let message = error(&binary, "drop_glue");
    let mut lines = message.lines();
    assert_eq!(
        lines.next(),
        Some("`drop_glue` could mean 50 functions; use one on the left instead:")
    );
    let choices: Vec<&str> = lines.take_while(|line| !line.ends_with(" more")).collect();
    assert_eq!(choices.len(), 10);
    for choice in choices {
        let (query, name) = choice.trim().split_once("  ").unwrap();
        let query = query.trim_matches('\'');
        assert!(query.chars().count() <= 40, "{choice}");
        assert_eq!(found(&binary, query), name.trim_start(), "{choice}");
    }
    // Names and addresses can share a list.
    let message = error(&binary, "fmt");
    let queries: Vec<&str> = message
        .lines()
        .skip(1)
        .filter_map(|line| line.trim().split_once("  "))
        .map(|(query, _)| query)
        .collect();
    assert!(
        queries.iter().any(|query| query.starts_with("0x")),
        "{message}"
    );
    assert!(
        queries.iter().any(|query| !query.starts_with("0x")),
        "{message}"
    );
}

#[test]
fn typos_get_suggestions() {
    let binary = load_sample("c-x86_64.elf");
    assert_eq!(
        error(&binary, "proces_frame"),
        "no function or data object is named `proces_frame`; did you mean process_frame?"
    );
    assert_eq!(
        error(&binary, "log_mgs"),
        "no function or data object is named `log_mgs`; did you mean log_msg?"
    );

    // Suggestions are what to type, not the full name.
    let binary = load_sample("rust-x86_64-nodebug.elf");
    assert_eq!(
        error(&binary, "proces_frame"),
        "no function or data object is named `proces_frame`; did you mean process_frame?"
    );
    assert!(
        error(&binary, "Gain")
            .starts_with("no function or data object is named `Gain`; did you mean Gain::run, ")
    );

    // Of an item's names, a typo away beats containing what was typed.
    let binary = load_sample("c-cortex-m4.elf");
    assert_eq!(
        error(&binary, "Dfault_Handler"),
        "no function or data object is named `Dfault_Handler`; did you mean Default_Handler?"
    );
    assert_eq!(
        error(&binary, "SV_Handler"),
        "no function or data object is named `SV_Handler`; did you mean SVC_Handler?"
    );

    // An alias can be what comes closest, though the item's shortest name
    // is another.
    let binary = load_sample("c-softfloat-nodebug.elf");
    assert_eq!(
        error(&binary, "aeabi_dad"),
        "no function or data object is named `aeabi_dad`; did you mean __aeabi_dadd?"
    );

    // Of the names that contain the query, those the fewest typos from it
    // are the ones to suggest: `app::checksum` is two typos from
    // `app::checks`, closer than the longer names of the three checksum
    // functions, but finds all three, so nothing is suggested. A name
    // whose length is more typos away than the query allows counts as one
    // typo further.
    let binary = load_sample("cpp-x86_64.elf");
    for query in ["app::checks", "app::l", "p::log", "p::checksum"] {
        assert_eq!(
            error(&binary, query),
            format!("no function or data object is named `{query}`")
        );
    }
    assert_eq!(
        error(&binary, "load_defaultsxy"),
        "no function or data object is named `load_defaultsxy`; did you mean load_defaults?"
    );
}

#[test]
fn every_short_name_has_one_of_its_names_keys() {
    // To find a short name, elfq works out the short names of only the
    // items that one of their names files under the short name's key.
    let mut checked = 0;
    for sample in samples() {
        let binary = load_sample(sample.trim_start_matches("testdata/bin/"));
        for name in binary.items.iter().flat_map(|item| item.names()) {
            let keys = short_name_keys(name);
            for short_name in short_names(name) {
                let key = short_name_key(&short_name);
                assert!(
                    key.is_some_and(|key| keys.iter().any(|k| k == key)),
                    "{sample}: {short_name:?} of {name:?} has key {key:?}, not one of {keys:?}"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 1000, "{checked}");
}
