//! Finding what the user means by SYMBOL.

use elfq::binary::Binary;
use elfq::commands::symbol;

use crate::support::load_sample;

/// The name of what `query` finds.
fn found(binary: &Binary, query: &str) -> String {
    symbol::find(binary, query)
        .unwrap_or_else(|err| panic!("{query}: {err}"))
        .name(binary)
        .to_string()
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
    // An imported function means its stub, which is what code calls.
    assert_eq!(found(&binary, "write"), "write@plt");
    assert_eq!(found(&binary, "__libc_start_main"), "__libc_start_main");

    assert_eq!(
        error(&binary, "0x10"),
        "no function or data object contains address 0x10"
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
        "`checksum` could mean 3 functions; use one of the names on the left instead:
  'checksum::<u8>'   elfq_sample::checksum::<u8>
  'checksum::<u32>'  elfq_sample::checksum::<u32>
  'checksum::<u16>'  elfq_sample::checksum::<u16>"
    );
    assert_eq!(
        error(&binary, "Stage::run"),
        "`Stage::run` could mean 2 functions; use one of the names on the left instead:
  Gain::run     <elfq_sample::Gain as elfq_sample::Stage>::run
  DcBlock::run  <elfq_sample::DcBlock as elfq_sample::Stage>::run"
    );
    // Two functions with the same name, from two copies of the standard
    // library's formatting code: only an address tells them apart.
    let display = "<&str as core::fmt::Display>::fmt";
    let message = error(&binary, display);
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
}
