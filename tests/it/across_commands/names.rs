//! The names that the commands print and take.
//!
//! The tests check what SYMBOL gave when it isn't the item's name, as a
//! pattern that matches one item, Rust and C++ names made readable and
//! found by their short forms, and the names of data that no symbol names.
//! Finding what SYMBOL means, in the library, is tested in
//! library/symbol.rs.

use crate::support::{SAMPLE, TCM_SAMPLE, assert_output, crossing, stderr, stdout};

#[test]
fn the_first_line_says_what_symbol_gave_when_it_isnt_the_name() {
    let first_line = |args: &[&str]| stdout(args).lines().next().unwrap().to_string();
    let m4 = "testdata/bin/c-cortex-m4.elf";
    assert_eq!(
        first_line(&[m4, "retained", "NMI_Handler"]),
        "Removing Default_Handler (also named NMI_Handler) would save only its own 2 bytes."
    );
    assert_eq!(
        first_line(&[m4, "why", "HardFault_Handler"]),
        "Default_Handler (also named HardFault_Handler) is in the vector table: the processor \
         runs it on an interrupt or exception."
    );
    assert_eq!(
        first_line(&[m4, "callees", "SVC_Handler"]),
        "Default_Handler (also named SVC_Handler) doesn't call or use anything."
    );
    assert_eq!(
        first_line(&[TCM_SAMPLE, "callers", "0x30000290"]),
        "log_msg (which holds 0x30000290) is used by 4 functions:"
    );
    assert_eq!(
        first_line(&[SAMPLE, "why", "0x1aa5"]),
        "Nothing reaches debug_dump (which holds 0x1aa5): the program never uses it, or elfq \
         can't see how."
    );
    // A name with a note can't take "'s".
    assert_eq!(
        first_line(&[SAMPLE, "inlined", "0x1710"]),
        "Of the 532 bytes of process_frame (which holds 0x1710), 238 are inlined copies of 3 \
         functions:"
    );
    assert_eq!(
        first_line(&[SAMPLE, "inlined", "0x1700"]),
        "Of process_frame's 532 bytes, 238 are inlined copies of 3 functions:"
    );
    // Among several SYMBOLs too.
    let first_line = |args: &[&str]| crossing(args).lines().next().unwrap().to_string();
    assert_eq!(
        first_line(&[TCM_SAMPLE, "callers", "NMI_Handler", "crc32"]),
        "Default_Handler (also named NMI_Handler) and crc32 are used by 2 items outside them:"
    );
    assert_eq!(
        first_line(&[TCM_SAMPLE, "callers", "0x14", "report"]),
        "crc32 (which holds 0x14) and report are used by 2 functions outside them:"
    );
}

#[test]
fn a_pattern_that_matches_one_item_answers_as_its_name() {
    // why, reach and inlined give the item's answer, whose first line says
    // what the pattern found, and its JSON byte for byte.
    let m4 = "testdata/bin/c-cortex-m4.elf";
    for (sample, command, pattern, name, first_line) in [
        (
            TCM_SAMPLE,
            "why",
            "on_b*",
            "on_button",
            "on_button (the one item named on_b*) is used by main, through this chain:",
        ),
        (
            TCM_SAMPLE,
            "reach",
            "on_b*",
            "on_button",
            "on_button (the one item named on_b*) and the 8 items it reaches take 362 bytes of \
             code and 4116 bytes of data:",
        ),
        (
            SAMPLE,
            "inlined",
            "process_f*",
            "process_frame",
            "Of the 532 bytes of process_frame (the one item named process_f*), 238 are inlined \
             copies of 3 functions:",
        ),
        // Unlike retained, they add no note on an item that the pattern
        // matched only through another of its names.
        (
            m4,
            "why",
            "HardF*",
            "Default_Handler",
            "Default_Handler (the one item named HardF*) is in the vector table: the processor \
             runs it on an interrupt or exception.",
        ),
    ] {
        let by_pattern = stdout(&[sample, command, pattern]);
        let by_name = stdout(&[sample, command, name]);
        let (first, rest) = by_pattern.split_once('\n').unwrap();
        assert_eq!(first, first_line);
        assert_output(rest, by_name.split_once('\n').unwrap().1);
        assert_output(
            &stdout(&[sample, command, pattern, "--json"]),
            &stdout(&[sample, command, name, "--json"]),
        );
    }
}

#[test]
fn rust_names_are_readable_and_short_ones_work() {
    assert_output(
        &stdout(&[
            "testdata/bin/rust-x86_64-nodebug.elf",
            "callers",
            "process_frame",
        ]),
        "\
elfq_sample::process_frame is used by 1 function:

HOW   AT                  CALLER
call  0x14f12 and 2 more  elfq_sample::app_main
",
    );
    // A row names the row it hangs under by the shortest form of its name
    // that finds it.
    let top = stdout(&["testdata/bin/rust-cortex-m4.elf", "top"]);
    assert!(
        top.contains("\n    5840   736  elfq_sample::process_frame, kept by app_main\n"),
        "{top}"
    );
    let why = stdout(&["testdata/bin/rust-cortex-m4.elf", "why", "log_msg"]);
    assert!(
        why.contains("\ncall          elfq_sample::process_frame, used by app_main\n"),
        "{why}"
    );
    // Two functions have this name, and only an address finds each one, so
    // rows use the whole name.
    let all = stdout(&["testdata/bin/rust-x86_64-nodebug.elf", "top", "--all"]);
    assert!(
        all.contains(", kept by core::ptr::drop_glue::<alloc::string::String>\n"),
        "{all}"
    );
    // `DebugSet::entry` is an alias of this function, and shorter, but no
    // row shows it.
    let retained = stdout(&[
        "testdata/bin/rust-x86_64-nodebug.elf",
        "retained",
        "DebugList::entry",
    ]);
    assert!(
        retained.contains("  .rodata@0x963e, kept by DebugList::entry\n"),
        "{retained}"
    );
}

#[test]
fn cpp_names_are_readable_and_short_ones_work() {
    const CPP: &str = "testdata/bin/cpp-x86_64.elf";
    // A virtual function is reached through its class's vtable.
    assert_output(
        &stdout(&[CPP, "why", "Gain::run"]),
        "\
app::Gain::run(short*, unsigned long) is used by vtable for app::Gain, through this chain:

HOW            ITEM
entry point    _start
address taken  main
uses           vtable for app::Gain
pointer        app::Gain::run(short*, unsigned long)

Removing vtable for app::Gain would remove app::Gain::run(short*, unsigned long) too.
",
    );
    // Overloads share a name, so their parameters tell them apart, and
    // 'log*' takes both.
    assert_output(
        &stderr(&[CPP, "callers", "log"]),
        "\
error: `log` could mean 2 functions; use one on the left instead, or 'log*' for all 2:
  'log(char const*)'   app::log(char const*)
  'log(unsigned int)'  app::log(unsigned int)
",
    );
    // An operator, and imports from the C++ runtime.
    assert_output(
        &stdout(&[CPP, "callers", "operator<"]),
        "\
app::operator<(app::Reading const&, app::Reading const&) is used by 1 function:

HOW   AT      CALLER
call  0x156f  app::sort_readings(app::Reading*, unsigned long)
",
    );
    assert_output(
        &stdout(&[CPP, "callees", "typeinfo for Gain"]),
        "\
typeinfo for app::Gain uses 3 items:

HOW      AT      CALLEE
pointer  0x3d78  vtable for __cxxabiv1::__si_class_type_info
pointer  0x3d80  typeinfo name for app::Gain
pointer  0x3d88  typeinfo for app::Stage
",
    );
    // Rows name the row they hang under by a short name, prefix and all.
    let top = stdout(&[CPP, "top"]);
    assert!(
        top.contains(" (through a pointer), kept by vtable for DcBlock\n"),
        "{top}"
    );
}

#[test]
fn data_without_a_symbol_is_named_by_section_and_address() {
    // The vtable through which `app_main` reaches `Gain::run`.
    assert_output(
        &stdout(&[
            "testdata/bin/rust-x86_64-nodebug.elf",
            "callers",
            "Gain::run"
        ]),
        "\
<elfq_sample::Gain as elfq_sample::Stage>::run is used by 1 data object:

HOW      AT       CALLER
pointer  0x52dc0  .data.rel.ro@0x52da8

Calls to <elfq_sample::Gain as elfq_sample::Stage>::run through a pointer aren't listed, since elfq can't see them.
"
    );
    let overview = stdout(&["testdata/bin/c-x86_64-stripped.elf"]);
    assert!(
        overview.contains(
            "Items without symbols are named by address, like fn_0x10a0 and .rodata@0x2004.\n"
        ),
        "{overview}"
    );
}
