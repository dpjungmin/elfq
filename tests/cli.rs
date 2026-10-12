//! Runs the elfq program the way a user would.

use std::process::{Command, Output};

const SAMPLE: &str = "testdata/bin/c-x86_64.elf";

fn elfq(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_elfq"))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap()
}

/// Runs elfq, expecting success, and returns what it printed.
fn stdout(args: &[&str]) -> String {
    let output = elfq(args);
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// Runs elfq, expecting failure with `code`, and returns its error output.
fn stderr(args: &[&str], code: i32) -> String {
    let output = elfq(args);
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    assert!(output.stdout.is_empty());
    String::from_utf8(output.stderr).unwrap()
}

#[test]
fn a_file_alone_gets_an_overview() {
    assert_eq!(
        stdout(&[SAMPLE]),
        "\
testdata/bin/c-x86_64.elf: x86-64 position-independent executable, with symbols and DWARF debug info

32 functions, 36 data objects and 8 imports, with 119 references between them.
4 calls or jumps go through pointers set at run time, which elfq can't follow.

Try:
  elfq testdata/bin/c-x86_64.elf callers log_msg
  elfq testdata/bin/c-x86_64.elf retained main
"
    );
}

#[test]
fn firmware_gets_the_same_overview() {
    assert_eq!(
        stdout(&["testdata/bin/c-cortex-m4.elf"]),
        "\
testdata/bin/c-cortex-m4.elf: 32-bit Arm executable, with symbols and DWARF debug info

25 functions and 31 data objects, with 97 references between them.
1 call or jump goes through pointers set at run time, which elfq can't follow.

Try:
  elfq testdata/bin/c-cortex-m4.elf callers log_msg
  elfq testdata/bin/c-cortex-m4.elf retained main
"
    );
    assert_eq!(
        stdout(&["testdata/bin/c-cortex-m4.elf", "callers", "SysTick_Handler"]),
        "\
SysTick_Handler is used by 1 data object:

CALLER        HOW      AT
vector_table  pointer  0x800003c
"
    );
}

#[test]
fn retained_shows_what_a_function_keeps_as_a_tree() {
    assert_eq!(
        stdout(&[SAMPLE, "retained", "process_frame"]),
        "\
process_frame keeps 2068 bytes in the file: its own 532, and 1536 in 8 items that only it leads to.

RETAINED   OWN  ITEM
    2068   532  process_frame
    1083    59  ├─ crc32
    1024  1024  │  └─ crc_table
     378   271  ├─ apply_filter
     107    75  │  └─ filter_tap
      32    32  │     └─ fir_coeffs
      54    54  ├─ decode_header
      12   ~12  ├─ .rodata@0x2012
       9    ~9  └─ .rodata@0x201e

Also uses, but shares with other code: __stack_chk_fail@plt, dispatch_event, frames_bad, frames_ok, log_msg, stats.
"
    );
}

#[test]
fn a_large_tree_shows_its_largest_items() {
    let output = stdout(&[SAMPLE, "retained", "main"]);
    assert!(
        output.starts_with("main keeps 3597 bytes in the file"),
        "{output}"
    );
    // Smaller items are summed up under their parent.
    assert!(
        output.contains("      21        │  └─ 2 more\n"),
        "{output}"
    );
    assert!(
        output.contains(
            "The tree shows the largest 30 of the 50 items. To list them all:\n  \
             elfq testdata/bin/c-x86_64.elf exclusive main\n"
        ),
        "{output}"
    );
    // The handlers are only called through the `handlers` table.
    assert!(
        output.contains("├─ on_button (through a pointer)\n"),
        "{output}"
    );
    assert!(
        output.ends_with(
            "Nothing calls the functions marked \"through a pointer\" directly, so elfq counts \
             them for the code or data that holds their address.\n"
        ),
        "{output}"
    );
}

#[test]
fn exclusive_lists_what_only_a_function_leads_to() {
    assert_eq!(
        stdout(&[SAMPLE, "exclusive", "process_frame"]),
        "\
process_frame is the only way to reach 8 items, 1536 bytes in all:

SIZE  KIND      ITEM
1024  data      crc_table
 271  function  apply_filter
  75  function  filter_tap
  59  function  crc32
  54  function  decode_header
  32  data      fir_coeffs
 ~12  data      .rodata@0x2012
  ~9  data      .rodata@0x201e
"
    );
    assert_eq!(
        stdout(&[SAMPLE, "exclusive", "crc_table"]),
        "Nothing else is in the file only because of crc_table.\n"
    );
}

#[test]
fn dead_code_keeps_nothing() {
    assert_eq!(
        stdout(&[SAMPLE, "retained", "debug_dump"]),
        "Nothing reaches debug_dump from where the program starts, as far as elfq can tell, so \
         nothing is there only because of it. It may be dead code, or used in a way elfq can't \
         see.\n"
    );
}

#[test]
fn callers_lists_each_caller_and_how_it_calls() {
    assert_eq!(
        stdout(&[SAMPLE, "callers", "log_msg"]),
        "\
log_msg is used by 5 functions:

CALLER         HOW        AT
app_tick       call       0x16e7
debug_dump     call       0x1aaf
on_button      tail call  0x1661
process_frame  call       0x18ee, 0x1908
report         call       0x1a72
"
    );
}

#[test]
fn rust_names_are_readable_and_short_ones_work() {
    assert_eq!(
        stdout(&[
            "testdata/bin/rust-x86_64-nodebug.elf",
            "callers",
            "process_frame"
        ]),
        "\
elfq_sample::process_frame is used by 1 function:

CALLER                 HOW   AT
elfq_sample::app_main  call  0x14f12, 0x14f29, 0x14f40
"
    );
}

#[test]
fn data_without_a_symbol_is_named_by_section_and_address() {
    // The vtable through which `app_main` reaches `Gain::run`.
    assert_eq!(
        stdout(&[
            "testdata/bin/rust-x86_64-nodebug.elf",
            "callers",
            "Gain::run"
        ]),
        "\
<elfq_sample::Gain as elfq_sample::Stage>::run is used by 1 data object:

CALLER                HOW      AT
.data.rel.ro@0x52da8  pointer  0x52dc0
"
    );
    let overview = stdout(&["testdata/bin/c-x86_64-stripped.elf"]);
    assert!(
        overview.contains(
            "Functions and data without symbols are named by address, like fn_0x10a0 and \
             .rodata@0x2004.\n"
        ),
        "{overview}"
    );
}

#[test]
fn callees_also_reports_what_it_cannot_follow() {
    assert_eq!(
        stdout(&[SAMPLE, "callees", "dispatch_event"]),
        "\
dispatch_event uses 1 data object:

CALLEE    HOW            AT
handlers  address taken  0x169b

1 indirect jump at 0x16ad can go anywhere: the target is only known at run time.
"
    );
}

#[test]
fn debug_items_lists_each_item() {
    let output = stdout(&[SAMPLE, "debug", "items"]);
    let line = output
        .lines()
        .find(|line| line.ends_with(" process_frame"))
        .unwrap();
    let fields: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(
        fields,
        ["0x1700", "532", "function", "symtab", "process_frame"]
    );
}

#[test]
fn errors_go_to_stderr_with_a_failing_exit_code() {
    assert_eq!(
        stderr(&["Cargo.toml"], 1),
        "error: cannot load Cargo.toml: not an ELF file\n"
    );
    assert_eq!(
        stderr(&[SAMPLE, "callers", "proces_frame"], 1),
        "error: no function or data object is named `proces_frame`; did you mean process_frame?\n"
    );
}

#[test]
fn a_command_typed_before_the_file_gets_a_hint() {
    assert_eq!(
        stderr(&["callers", SAMPLE, "log_msg"], 2),
        "error: the file comes before the command, as in: \
         elfq testdata/bin/c-x86_64.elf callers log_msg\n"
    );
}

#[test]
fn no_arguments_prints_help() {
    let help = stderr(&[], 2);
    assert!(help.contains("Usage: elfq <FILE> [COMMAND]"), "{help}");
}
