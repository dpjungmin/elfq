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

The items that keep the most of the file's 4021 bytes:

RETAINED   OWN  ITEM
    3635    38  _start
    3597   195  └─ main (through a pointer)
    2068   532     ├─ process_frame
    1083    59     │  ├─ crc32
    1024  1024     │  │  └─ crc_table
     453           │  └─ 4 more
    1334           └─ 19 more
     345        9 more

Nothing calls the functions marked \"through a pointer\" directly, so elfq counts them for the code or data that holds their address.

Try:
  elfq testdata/bin/c-x86_64.elf top
  elfq testdata/bin/c-x86_64.elf retained main
  elfq testdata/bin/c-x86_64.elf why crc_table
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

The items that keep the most of the file's 3577 bytes:

RETAINED   OWN  ITEM
    3111    76  Reset_Handler
    2529   132  ├─ main
    1684   376  │  ├─ process_frame
    1076    52  │  │  ├─ crc32
    1024  1024  │  │  │  └─ crc_table
     232        │  │  └─ 4 more
     713        │  └─ 12 more
     506        └─ 4 more
     466        7 more

Try:
  elfq testdata/bin/c-cortex-m4.elf top
  elfq testdata/bin/c-cortex-m4.elf retained main
  elfq testdata/bin/c-cortex-m4.elf why crc_table
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
fn top_shows_what_keeps_the_most_bytes() {
    assert_eq!(
        stdout(&[SAMPLE, "top"]),
        "\
The items that keep the most of the file's 4021 bytes:

RETAINED   OWN  ITEM
    3635    38  _start
    3597   195  └─ main (through a pointer)
    2068   532     ├─ process_frame
    1083    59     │  ├─ crc32
    1024  1024     │  │  └─ crc_table
     378   271     │  ├─ apply_filter
     107    75     │  │  └─ filter_tap
      32    32     │  │     └─ fir_coeffs
      54    54     │  ├─ decode_header
      21           │  └─ 2 more
     705   370     ├─ report
     162   162     │  ├─ format_u32
      80    16     │  ├─ root
      48    16     │  │  ├─ inner
      32           │  │  │  └─ 2 more
      16           │  │  └─ 1 more
      46    46     │  ├─ count_nodes
      47           │  └─ 6 more
     187    84     ├─ parse_value
     103   103     │  └─ parse_list
     103    51     ├─ log_msg
      36    20     │  ├─ platform_write
      16           │  │  └─ 1 more
      16           │  └─ 1 more
      57    51     ├─ app_tick
       6           │  └─ 1 more
      34   ~34     ├─ .rodata@0x205e
      33    33     ├─ dispatch_event
      32    32     ├─ stats
      31    19     ├─ on_button (through a pointer)
      12           │  └─ 2 more
      25    25     ├─ frame_a
     127           └─ 10 more
     152   ~64  __do_global_dtors_aux
      48   ~48  ├─ deregister_tm_clones
      40        └─ 2 more
      80   ~16  frame_dummy
      64   ~64  └─ register_tm_clones
      64    64  .plt
      32    32  __abi_tag
      17        5 more

Nothing reaches 3 items, 41 bytes in all, as far as elfq can tell: debug_dump, .rodata@0x2056 and _IO_stdin_used. They may be dead code, or used in a way elfq can't see.

Nothing calls the functions marked \"through a pointer\" directly, so elfq counts them for the code or data that holds their address.
"
    );
    // On Cortex-M the SysTick interrupt also reaches `log_msg`, so it isn't
    // under `main` but at the top, with what only it leads to.
    let firmware = stdout(&["testdata/bin/c-cortex-m4.elf", "top"]);
    assert!(
        firmware.contains("\n     276    26  log_msg\n     216   216  ├─ strlen\n"),
        "{firmware}"
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
fn why_shows_the_chains_that_keep_an_item() {
    assert_eq!(
        stdout(&[SAMPLE, "why", "log_msg"]),
        "\
log_msg is in the file because 4 functions use it. The shortest chain through each, from where the program starts:

HOW            ITEM
entry point    _start
address taken  └─ main
call              ├─ app_tick
call              │  └─ log_msg
address taken     ├─ on_button
tail call         │  └─ log_msg
call              ├─ process_frame
call              │  └─ log_msg
call              └─ report
call                 └─ log_msg

debug_dump uses log_msg too, but nothing reaches debug_dump.

Every chain to log_msg goes through main, so it counts toward main's retained size.
"
    );
}

#[test]
fn why_on_firmware_can_start_from_an_interrupt() {
    assert_eq!(
        stdout(&["testdata/bin/c-cortex-m4.elf", "why", "app_tick"]),
        "\
app_tick is in the file because 2 functions use it. The shortest chain through each, from where the program starts:

HOW                         ITEM
entry point                 Reset_Handler
call                        └─ main
call                           └─ app_tick
listed in the vector table  SysTick_Handler
tail call                   └─ app_tick

No single item is on every chain to app_tick, so it doesn't count toward any one item's retained size.
"
    );
}

#[test]
fn why_explains_where_the_program_starts() {
    let why = |file, symbol| stdout(&[file, "why", symbol]);
    assert_eq!(
        why(SAMPLE, "_start"),
        "_start is the entry point, where the program starts.\n"
    );
    assert_eq!(
        why(SAMPLE, "frame_dummy"),
        "frame_dummy is listed in .init_array, so the program runs it when it starts.\n"
    );
    assert_eq!(
        why("testdata/bin/c-cortex-m4.elf", "SysTick_Handler"),
        "SysTick_Handler is listed in the vector table, so the processor runs it for an \
         exception or an interrupt.\n"
    );
}

#[test]
fn why_says_when_nothing_reaches_an_item() {
    assert_eq!(
        stdout(&[SAMPLE, "why", "debug_dump"]),
        "Nothing reaches debug_dump from where the program starts, as far as elfq can tell. It \
         may be dead code, or used in a way elfq can't see.\n"
    );
    // The string that only `debug_dump` prints.
    assert_eq!(
        stdout(&[SAMPLE, "why", ".rodata@0x2056"]),
        "Nothing reaches .rodata@0x2056 from where the program starts, as far as elfq can tell. \
         It may be dead code, or used in a way elfq can't see.\n\
         debug_dump uses it, but nothing reaches debug_dump either.\n"
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
