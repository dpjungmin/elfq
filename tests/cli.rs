//! Runs the elfq program the way a user would.

use std::process::{Command, Output};

const SAMPLE: &str = "testdata/bin/c-x86_64.elf";
/// Firmware with code and data in several kinds of memory.
const TCM_SAMPLE: &str = "testdata/bin/c-cortex-m7.elf";
/// Calls and inlined code nested 20 deep.
const DEEP_SAMPLE: &str = "testdata/bin/c-deep.elf";

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
32 functions and 36 data objects, 4021 bytes of code and data, plus 8 imports

RETAINED   OWN  ITEM
    3635    38  _start
    3597   195  └─ main (through a pointer)
    2068   532     ├─ process_frame
    1083    59     │  ├─ crc32
    1024  1024     │  │  └─ crc_table
     453           │  └─ 6 more
    1334           └─ 41 more
     345        13 more

Try:
  elfq testdata/bin/c-x86_64.elf top
  elfq testdata/bin/c-x86_64.elf retained main
  elfq testdata/bin/c-x86_64.elf why crc_table
  elfq testdata/bin/c-x86_64.elf inlined
"
    );
}

#[test]
fn firmware_gets_the_same_overview() {
    assert_eq!(
        stdout(&["testdata/bin/c-cortex-m4.elf"]),
        "\
testdata/bin/c-cortex-m4.elf: 32-bit Arm executable, with symbols and DWARF debug info
25 functions and 31 data objects, 3577 bytes of code and data

MEMORY                  SIZE  SECTIONS
0x08000000..0x08000dec  3564  .isr_vector .text .rodata .ARM.exidx
0x20000000..0x20000048    72  .data .bss

RETAINED   OWN  ITEM
    3111    76  Reset_Handler
    2565   132  ├─ main
    1684   376  │  ├─ process_frame
    1076    52  │  │  ├─ crc32
    1024  1024  │  │  │  └─ crc_table
     232        │  │  └─ 6 more
     749        │  └─ 32 more
     470        └─ 2 more
     466        11 more

Try:
  elfq testdata/bin/c-cortex-m4.elf --range 0x08000000..0x08000dec
  elfq testdata/bin/c-cortex-m4.elf --range 0x20000000..0x20000048
  elfq testdata/bin/c-cortex-m4.elf top
  elfq testdata/bin/c-cortex-m4.elf retained main
  elfq testdata/bin/c-cortex-m4.elf why crc_table
  elfq testdata/bin/c-cortex-m4.elf inlined
"
    );
    assert_eq!(
        stdout(&["testdata/bin/c-cortex-m4.elf", "callers", "SysTick_Handler"]),
        "\
SysTick_Handler is used by 1 data object:

CALLER        HOW      AT
vector_table  pointer  0x800003c

SysTick_Handler is in the vector table: the processor runs it on an interrupt or exception.
"
    );
}

#[test]
fn stripped_firmware_has_only_its_entry_point() {
    // Without symbols or an unwind table, elfq finds only the reset
    // handler, whose size it doesn't know.
    let overview = stdout(&["testdata/bin/c-cortex-m4-stripped.elf"]);
    assert!(
        overview.starts_with(
            "\
testdata/bin/c-cortex-m4-stripped.elf: 32-bit Arm executable, stripped
1 function
The file has no symbols or unwind table, so elfq can't find its functions yet.
"
        ),
        "{overview}"
    );
}

#[test]
fn top_shows_what_keeps_the_most_bytes() {
    assert_eq!(
        stdout(&[SAMPLE, "top"]),
        "\
4021 bytes of code and data, by what keeps them:

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
      16    16     │  │  └─ leaf_a
      46    46     │  ├─ count_nodes
      47           │  └─ 6 more
     187    84     ├─ parse_value
     103   103     │  └─ parse_list
     103    51     ├─ log_msg
      36    20     │  ├─ platform_write
      16    16     │  │  └─ write@plt
      16    16     │  └─ strlen@plt
      57    51     ├─ app_tick
       6    ~6     │  └─ .rodata@0x200c
      34   ~34     ├─ .rodata@0x205e
      33    33     ├─ dispatch_event
      32    32     ├─ stats
      31    19     ├─ on_button (through a pointer)
      12           │  └─ 2 more
      25    25     ├─ frame_a
     127           └─ 12 more
     152   ~64  __do_global_dtors_aux
      48   ~48  ├─ deregister_tm_clones
      40        └─ 2 more
      80   ~16  frame_dummy
      64   ~64  └─ register_tm_clones
      64    64  .plt
      32    32  __abi_tag
      17        5 more

Nothing reaches 3 items, 41 bytes in all: debug_dump, .rodata@0x2056 and _IO_stdin_used. They are dead code, or used in a way elfq can't see.
"
    );
    // On Cortex-M the SysTick interrupt also reaches `log_msg`, so it isn't
    // under `main` but at the top, with what only it leads to.
    let firmware = stdout(&["testdata/bin/c-cortex-m4.elf", "top"]);
    assert!(
        firmware.contains("\n     276    26  log_msg\n     216   216  ├─ strlen\n"),
        "{firmware}"
    );
    // One item that nothing reaches.
    let cpp = stdout(&["testdata/bin/cpp-x86_64.elf", "top"]);
    assert!(
        cpp.ends_with(
            "\n\nNothing reaches _IO_stdin_used, 4 bytes: it is dead code, or used in a way elfq \
             can't see.\n"
        ),
        "{cpp}"
    );
}

#[test]
fn the_overview_lists_where_firmware_is_in_memory() {
    let overview = stdout(&[TCM_SAMPLE]);
    assert!(
        overview.contains(
            "\
MEMORY                  SIZE  SECTIONS
0x00000000..0x000000e0   224  .itcm
0x20000000..0x20000450  1104  .data .bss
0x20240000..0x20241000  4096  .ocram_bss
0x30000000..0x30000edc  3804  .isr_vector .text .rodata .ARM.exidx
"
        ),
        "{overview}"
    );
    assert!(
        overview.contains(
            "\
Try:
  elfq testdata/bin/c-cortex-m7.elf --range 0x00000000..0x000000e0
  elfq testdata/bin/c-cortex-m7.elf --range 0x20000000..0x20000450
  elfq testdata/bin/c-cortex-m7.elf --range 0x20240000..0x20241000
  elfq testdata/bin/c-cortex-m7.elf --range 0x30000000..0x30000edc
  elfq testdata/bin/c-cortex-m7.elf top
"
        ),
        "{overview}"
    );
    // One block of addresses needs no table.
    assert!(!stdout(&[SAMPLE]).contains("MEMORY"));
}

#[test]
fn a_range_counts_only_the_bytes_in_it() {
    // Functions in flash keep what they alone call in ITCM, through the
    // linker's veneers, so they show with none of their own bytes.
    assert_eq!(
        stdout(&[TCM_SAMPLE, "top", "--range", "0x0..0xe0"]),
        "\
224 bytes of code and data in 0x00000000..0x000000e0 (.itcm), by what keeps them:

RETAINED  OWN  ITEM
     200    0  Reset_Handler
     200    0  └─ main
     200    0     └─ process_frame
     152    0        ├─ __apply_filter_veneer
     152   92        │  └─ apply_filter
      52   52        │     ├─ filter_tap
       8    8        │     └─ __memcpy_veneer
      48    0        └─ __crc32_veneer
      48   48           └─ crc32
      24   16  SysTick_Handler
       8    8  └─ __app_tick_veneer
"
    );
    // A line that sums up items counts only those with bytes in the range:
    // under process_frame, crc32 and crc_table are in ITCM and DTCM, and
    // so the line stands for two .rodata items and __crc32_veneer.
    let flash = stdout(&[TCM_SAMPLE, "top", "--range", "0x30000000..0x30000edc"]);
    assert!(
        flash.contains("      32       │  │  └─ 3 more\n"),
        "{flash}"
    );
    // The same range as a start and a size, with a unit in any case.
    assert_eq!(
        stdout(&[TCM_SAMPLE, "top", "--range", "0+224B"]),
        stdout(&[TCM_SAMPLE, "top", "--range", "0x0..0xe0"])
    );
    assert_eq!(
        stdout(&[TCM_SAMPLE, "--range", "0x20240000+4k"]),
        "\
testdata/bin/c-cortex-m7.elf: 32-bit Arm executable, with symbols and DWARF debug info
1 data object in 0x20240000..0x20241000 (.ocram_bss), 4096 bytes of code and data

RETAINED   OWN  ITEM
    4096     0  log_msg
    4096     0  └─ platform_write
    4096  4096     └─ tx_buffer

Try:
  elfq testdata/bin/c-cortex-m7.elf top --range 0x20240000..0x20241000
  elfq testdata/bin/c-cortex-m7.elf why tx_buffer
"
    );
}

#[test]
fn retained_counts_only_the_range_too() {
    assert_eq!(
        stdout(&[
            TCM_SAMPLE,
            "retained",
            "process_frame",
            "--range",
            "0x0..0xe0"
        ]),
        "\
Removing process_frame would save 200 bytes in 0x00000000..0x000000e0 (.itcm):

RETAINED  OWN  ITEM
     200    0  process_frame
     152    0  ├─ __apply_filter_veneer
     152   92  │  └─ apply_filter
      52   52  │     ├─ filter_tap
       8    8  │     └─ __memcpy_veneer
      48    0  └─ __crc32_veneer
      48   48     └─ crc32
"
    );
    assert_eq!(
        stdout(&[TCM_SAMPLE, "retained", "crc32", "--range", "0x0..0xe0"]),
        "Removing crc32 would save its own 48 bytes in 0x00000000..0x000000e0 (.itcm), and \
         nothing else.\n"
    );
    assert_eq!(
        stdout(&[TCM_SAMPLE, "retained", "log_msg", "--range", "0x0..0xe0"]),
        "Removing log_msg would save none of the bytes in 0x00000000..0x000000e0 (.itcm).\n"
    );
}

#[test]
fn a_range_must_hold_something() {
    assert_eq!(
        stderr(&[TCM_SAMPLE, "top", "--range", "0x40000000+4K"], 1),
        "error: nothing in the file is in 0x40000000..0x40001000; the overview lists the ranges \
         that the file's sections occupy\n"
    );
    assert!(
        stderr(&[TCM_SAMPLE, "--range", "0x20000000"], 2)
            .contains("expected START..END or START+SIZE")
    );
    assert_eq!(
        stderr(&[TCM_SAMPLE, "callers", "crc32", "--range", "0+1K"], 1),
        "error: --range limits sizes, so only the overview, top and retained take it\n"
    );
}

#[test]
fn retained_shows_what_a_function_keeps_as_a_tree() {
    assert_eq!(
        stdout(&[SAMPLE, "retained", "process_frame"]),
        "\
Removing process_frame would save 2068 bytes:

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

Shared with other code, so not counted: __stack_chk_fail@plt, dispatch_event, frames_bad, frames_ok, log_msg, stats.
"
    );
    assert_eq!(
        stdout(&[SAMPLE, "retained", "crc_table"]),
        "Removing crc_table would save its own 1024 bytes, and nothing else.\n"
    );
    assert_eq!(
        stdout(&[SAMPLE, "retained", "completed.0"]),
        "Removing completed.0 would save its own 1 byte, and nothing else.\n"
    );
}

#[test]
fn a_large_tree_shows_its_largest_items() {
    let output = stdout(&[SAMPLE, "retained", "main"]);
    assert!(
        output.starts_with("Removing main would save 3597 bytes:\n"),
        "{output}"
    );
    // Smaller items are summed up under their parent.
    assert!(
        output.contains("      21        │  └─ 2 more\n"),
        "{output}"
    );
    // The handlers are only called through the `handlers` table.
    assert!(
        output.contains("├─ on_button (through a pointer)\n"),
        "{output}"
    );
    // The jump through the `handlers` table, in `dispatch_event`.
    assert!(
        output.ends_with(
            "Code that only main leads to also jumps through a pointer, which elfq can't \
             follow, so what that reaches may count elsewhere.\n"
        ),
        "{output}"
    );
}

#[test]
fn no_line_sums_up_a_single_item() {
    // A line such as "└─ 12 more" counts every item it sums up, so one
    // that would sum up a single item shows the item instead, and so do
    // lists of names.
    let bin = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/bin");
    let mut samples: Vec<String> = std::fs::read_dir(bin)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".elf"))
        .map(|name| format!("testdata/bin/{name}"))
        .collect();
    samples.sort();
    assert!(samples.len() > 10);
    for sample in &samples {
        let overview = stdout(&[sample]);
        let mut runs = vec![vec![sample.as_str()], vec![sample, "top"]];
        // The ranges in the overview's Try list.
        for (_, range) in overview
            .lines()
            .filter_map(|line| line.split_once(" --range "))
        {
            runs.push(vec![sample, "--range", range]);
            runs.push(vec![sample, "top", "--range", range]);
        }
        for args in &runs {
            // A stripped file's ranges hold no items yet, so elfq fails on
            // them; what it prints before that is still checked.
            let text = String::from_utf8(elfq(args).stdout).unwrap();
            assert!(
                !text
                    .lines()
                    .any(|line| line.ends_with(" 1 more") || line.contains(" and 1 more")),
                "{args:?}\n{text}"
            );
        }
    }
}

#[test]
fn long_lists_name_the_first_few() {
    let sample = "testdata/bin/rust-x86_64-nodebug.elf";
    let choices = stderr(&[sample, "callers", "fmt"], 1);
    assert!(
        choices.starts_with("error: `fmt` could mean 53 functions;"),
        "{choices}"
    );
    assert_eq!(choices.lines().count(), 12, "{choices}");
    assert!(choices.ends_with("\n  and 43 more\n"), "{choices}");
    let retained = stdout(&[sample, "retained", "main"]);
    assert!(retained.contains(" and 48 more.\n"), "{retained}");
    let inlined = stdout(&["testdata/bin/rust-cortex-m4.elf", "inlined"]);
    assert!(inlined.contains("     231  133 more\n"), "{inlined}");
}

#[test]
fn a_deep_tree_continues_below() {
    assert_eq!(
        stdout(&[DEEP_SAMPLE, "retained", "chain1"]),
        "\
Removing chain1 would save 402 bytes:

RETAINED  OWN  ITEM
     402   16  chain1
     386   17  └─ chain2
     369   17     └─ chain3
     352   20        └─ chain4
     332   17           └─ chain5
     315   20              └─ chain6
     295   27                 └─ chain7
     268   20                    └─ chain8
     248   17                       └─ chain9 (continued below)

               chain9 (continued)
     231   20  └─ chain10
     211   20     └─ chain11
     191   23        └─ chain12
     168   20           └─ chain13
     148   19              └─ chain14
     129   23                 └─ chain15
     106   19                    └─ chain16
      87   22                       └─ chain17 (continued below)

               chain17 (continued)
      65   20  └─ chain18
      45   20     └─ chain19
      25   25        └─ chain20

Shared with other code, so not counted: chain_table.
"
    );
}

#[test]
fn every_tree_is_at_most_nine_levels_deep() {
    // Each command, and the rows whose part of the tree is continued below.
    let trees = [
        (&[DEEP_SAMPLE, "top"][..], &["stage7", "stage15"][..]),
        (&[DEEP_SAMPLE, "retained", "stage1"], &["stage9", "stage17"]),
        (&[DEEP_SAMPLE, "why", "chain_table"], &["chain7", "chain15"]),
        (&[DEEP_SAMPLE, "why", "stage_table"], &["stage7", "stage15"]),
        (
            &[DEEP_SAMPLE, "inlined", "--in", "run_steps"],
            &["step9", "step17"],
        ),
        (
            &["testdata/bin/rust-x86_64-nodebug.elf", "why", "memset"],
            &[
                "std::panicking::default_hook",
                "rustc_demangle::try_demangle",
                "__rustc::__rust_alloc_zeroed",
            ],
        ),
    ];
    for (args, cuts) in trees {
        let text = stdout(args);
        // The tree is in the last column, which starts where its header
        // does. Each level is 3 columns.
        let header = text
            .lines()
            .find(|line| line.ends_with("ITEM") || line.ends_with("FUNCTION"));
        let column = header.unwrap().rfind(' ').unwrap() + 1;
        let deepest = text
            .lines()
            .filter(|line| line.contains("─ "))
            .map(|line| {
                line.chars()
                    .skip(column)
                    .take_while(|c| "│├└─ ".contains(*c))
                    .count()
            })
            .max();
        assert!(deepest.unwrap() <= 9 * 3, "{args:?}\n{text}");
        // Each part cut from the tree is continued below, in order.
        let cut: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_suffix(" (continued below)"))
            .map(|line| line.rsplit("─ ").next().unwrap())
            .collect();
        let continued: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_suffix(" (continued)"))
            .map(str::trim)
            .collect();
        assert_eq!(cut, cuts, "{args:?}\n{text}");
        assert_eq!(continued, cuts, "{args:?}\n{text}");
    }
}

#[test]
fn calls_through_pointers_are_pointed_out() {
    // A virtual call: what it reaches counts for the vtables, which `main`
    // keeps.
    let retained = stdout(&["testdata/bin/cpp-x86_64.elf", "retained", "process_frame"]);
    assert!(
        retained.ends_with(
            "app::process_frame(app::Frame const&, app::Stage&) also calls through a pointer, \
             which elfq can't follow, so what that reaches may count elsewhere.\n"
        ),
        "{retained}"
    );
    // Calls through pointers in what only `main` leads to, and in both
    // `process_frame` and what only it leads to.
    assert!(
        stdout(&["testdata/bin/cpp-x86_64.elf", "retained", "main"]).ends_with(
            "Code that only main leads to also calls and jumps through pointers, which elfq \
             can't follow, so what they reach may count elsewhere.\n"
        )
    );
    assert!(
        stdout(&[
            "testdata/bin/rust-cortex-m4.elf",
            "retained",
            "elfq_sample::process_frame"
        ])
        .ends_with(
            "elfq_sample::process_frame and the code that only it leads to also call and jump \
             through pointers, which elfq can't follow, so what they reach may count \
             elsewhere.\n"
        )
    );
    // `main` takes the address of `on_button` to store it in a table.
    assert_eq!(
        stdout(&[SAMPLE, "callers", "on_button"]),
        "\
on_button is used by 1 function:

CALLER  HOW            AT
main    address taken  0x10b6

Calls to on_button through a pointer aren't listed, since elfq can't see them.
"
    );
    // Where the program starts, nothing in the file calls it.
    assert_eq!(
        stdout(&[SAMPLE, "callers", "_init"]),
        "\
Nothing in the file calls or uses _init.
_init is in .init, which the program uses when it starts.
"
    );
}

#[test]
fn all_shows_every_item() {
    let sample = "testdata/bin/rust-x86_64-nodebug.elf";
    let firmware = "testdata/bin/rust-cortex-m4.elf";
    for args in [
        vec![sample, "top"],
        vec![sample, "retained", "main"],
        vec![firmware, "inlined"],
    ] {
        let largest = stdout(&args);
        let all = stdout(&[&args[..], &["--all"]].concat());
        let sums_up = |text: &str| text.lines().any(|line| line.ends_with(" more"));
        assert!(sums_up(&largest), "{args:?}\n{largest}");
        assert!(!sums_up(&all), "{args:?}\n{all}");
        assert!(all.lines().count() > largest.lines().count());
    }
    // A tree of fewer than 30 items is the same either way.
    assert_eq!(
        stdout(&[SAMPLE, "retained", "process_frame", "--all"]),
        stdout(&[SAMPLE, "retained", "process_frame"])
    );
}

#[test]
fn dead_code_keeps_nothing() {
    assert_eq!(
        stdout(&[SAMPLE, "retained", "debug_dump"]),
        "Nothing reaches debug_dump from where the program starts: it is dead code, or used in \
         a way elfq can't see.\n"
    );
}

#[test]
fn why_shows_the_chains_that_keep_an_item() {
    assert_eq!(
        stdout(&[SAMPLE, "why", "log_msg"]),
        "\
log_msg is used by 4 functions, through these chains:

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

Removing main would remove log_msg too.
"
    );
    // parse_list calls parse_value back, so it doesn't keep it.
    let why = stdout(&[SAMPLE, "why", "parse_value"]);
    assert!(
        why.contains(
            "\nparse_list uses parse_value too, but only parse_value leads to parse_list.\n"
        ),
        "{why}"
    );
    // With more than ten users, the tree shows the chains through the
    // nearest ten.
    let why = stdout(&[
        "testdata/bin/rust-x86_64-nodebug.elf",
        "why",
        ".rodata@0x5633",
    ]);
    assert!(
        why.starts_with(
            ".rodata@0x5633 is used by 36 data objects; these are the chains through the \
             nearest 10:\n"
        ),
        "{why}"
    );
}

#[test]
fn why_on_firmware_can_start_from_an_interrupt() {
    assert_eq!(
        stdout(&["testdata/bin/c-cortex-m4.elf", "why", "app_tick"]),
        "\
app_tick is used by 2 functions, through these chains:

HOW           ITEM
entry point   Reset_Handler
call          └─ main
call             └─ app_tick
vector table  SysTick_Handler
tail call     └─ app_tick

Removing any single item wouldn't remove app_tick.
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
        "SysTick_Handler is in the vector table: the processor runs it on an interrupt or \
         exception.\n"
    );
}

#[test]
fn why_says_when_nothing_reaches_an_item() {
    assert_eq!(
        stdout(&[SAMPLE, "why", "debug_dump"]),
        "Nothing reaches debug_dump from where the program starts: it is dead code, or used in a \
         way elfq can't see.\n"
    );
    // The string that only `debug_dump` prints.
    assert_eq!(
        stdout(&[SAMPLE, "why", ".rodata@0x2056"]),
        "Nothing reaches .rodata@0x2056 from where the program starts: it is dead code, or used \
         in a way elfq can't see.\n\
         debug_dump uses it, but nothing reaches debug_dump either.\n"
    );
}

#[test]
fn inlined_ranks_the_functions_the_compiler_copied() {
    assert_eq!(
        stdout(&[SAMPLE, "inlined"]),
        "\
Inlined copies of 5 functions take 420 of the 2557 bytes of code:

BYTES  COPIES  FUNCTION
  164       1  update_stats
  150       6  append
   74       1  unpack_samples
   32       1  saturate
    0       1  load_le32

A copy of 0 bytes is one the compiler merged into the code around it, so the debug info gives it no bytes of its own.
"
    );
    assert_eq!(
        stdout(&[SAMPLE, "inlined", "--in", "process_frame"]),
        "\
Of process_frame's 532 bytes, 238 are inlined copies of 3 functions:

BYTES  COPIES  FUNCTION
  164       1  update_stats
   74       1  unpack_samples
    0       1  load_le32

A copy of 0 bytes is one the compiler merged into the code around it, so the debug info gives it no bytes of its own.
"
    );
    // The six calls to `append` in `report` make one row.
    assert_eq!(
        stdout(&[SAMPLE, "inlined", "--in", "report"]),
        "\
Of report's 370 bytes, 150 are inlined copies of 1 function:

BYTES  COPIES  FUNCTION
  150       6  append
"
    );
    assert_eq!(
        stdout(&[SAMPLE, "inlined", "--in", "main"]),
        "The debug info lists no inlined code in main.\n"
    );
}

#[test]
fn inlined_shows_copies_inside_copies_as_a_tree() {
    let tree = stdout(&[
        "testdata/bin/rust-cortex-m4.elf",
        "inlined",
        "--in",
        "memcpy",
    ]);
    assert!(
        tree.starts_with(
            "\
Of compiler_builtins::mem::memcpy's 730 bytes, 712 are inlined copies of 17 functions:

BYTES  COPIES  FUNCTION
  712       1  compiler_builtins::mem::impls::copy_forward
  392       1  ├─ compiler_builtins::mem::impls::copy_forward::copy_forward_misaligned_words
   74       1  │  ├─ compiler_builtins::mem::impls::load_aligned_partial
"
        ),
        "{tree}"
    );
}

#[test]
fn inlined_needs_dwarf() {
    assert_eq!(
        stderr(&["testdata/bin/c-x86_64-nodebug.elf", "inlined"], 1),
        "error: testdata/bin/c-x86_64-nodebug.elf has no DWARF debug info, which elfq needs to \
         tell what the compiler inlined; build it with debug info (such as -g) and don't strip \
         it\n"
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
fn cpp_names_are_readable_and_short_ones_work() {
    const CPP: &str = "testdata/bin/cpp-x86_64.elf";
    // A virtual function is reached through its class's vtable.
    assert_eq!(
        stdout(&[CPP, "why", "Gain::run"]),
        "\
app::Gain::run(short*, unsigned long) is used by vtable for app::Gain, through this chain:

HOW            ITEM
entry point    _start
address taken  └─ main
uses              └─ vtable for app::Gain
pointer              └─ app::Gain::run(short*, unsigned long)

Removing vtable for app::Gain would remove app::Gain::run(short*, unsigned long) too.
"
    );
    // Overloads share a name, so their parameters tell them apart.
    assert_eq!(
        stderr(&[CPP, "callers", "log"], 1),
        "\
error: `log` could mean 2 functions; use one of the names on the left instead:
  'log(char const*)'   app::log(char const*)
  'log(unsigned int)'  app::log(unsigned int)
"
    );
    // An operator, and imports from the C++ runtime.
    assert_eq!(
        stdout(&[CPP, "callers", "operator<"]),
        "\
app::operator<(app::Reading const&, app::Reading const&) is used by 1 function:

CALLER                                            HOW   AT
app::sort_readings(app::Reading*, unsigned long)  call  0x156f
"
    );
    assert_eq!(
        stdout(&[CPP, "callees", "typeinfo for Gain"]),
        "\
typeinfo for app::Gain uses 3 items:

CALLEE                                       HOW      AT
vtable for __cxxabiv1::__si_class_type_info  pointer  0x3d78
typeinfo name for app::Gain                  pointer  0x3d80
typeinfo for app::Stage                      pointer  0x3d88
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

Calls to <elfq_sample::Gain as elfq_sample::Stage>::run through a pointer aren't listed, since elfq can't see them.
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

CALLEE    HOW   AT
handlers  uses  0x169b

Also 1 jump through a pointer, at 0x16ad, which elfq can't follow.
"
    );
    const RUST_M4: &str = "testdata/bin/rust-cortex-m4.elf";
    assert!(
        stdout(&[RUST_M4, "callees", "<core::fmt::Formatter>::pad_integral"]).ends_with(
            "\nAlso 5 calls and 1 jump through pointers, at 0x8001004, 0x80010c4, 0x80010d6 \
             and 3 more, which elfq can't follow.\n"
        )
    );
    // With nothing else to list, the sentence leads.
    assert_eq!(
        stdout(&[
            RUST_M4,
            "callees",
            "<core::fmt::Formatter>::pad_integral::write_prefix"
        ]),
        "<core::fmt::Formatter>::pad_integral::write_prefix makes 1 call and 1 jump through \
         pointers, at 0x8001744 and 0x8001762, which elfq can't follow.\n"
    );
}

#[test]
fn code_uses_data_whether_it_reads_it_or_loads_its_address() {
    // lang_start_internal reads CLEANUP in one place and loads its address
    // in another: both are one row.
    assert_eq!(
        stdout(&[
            "testdata/bin/rust-x86_64-nodebug.elf",
            "callers",
            "std::rt::cleanup::CLEANUP"
        ]),
        "\
std::rt::cleanup::CLEANUP is used by 1 function:

CALLER                        HOW   AT
std::rt::lang_start_internal  uses  0x3a60a, 0x3a66d
"
    );
    // Arm code loads a variable's address before it reads or writes it,
    // and that is "uses" too.
    let stats = stdout(&["testdata/bin/c-cortex-m4.elf", "callers", "stats"]);
    assert!(
        stats.contains("\nprocess_frame  uses  0x8000332\n"),
        "{stats}"
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
    assert!(
        help.contains("Usage: elfq [OPTIONS] <FILE> [COMMAND]"),
        "{help}"
    );
}
