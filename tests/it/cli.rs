//! Runs the elfq program the way a user would.

use std::process::Stdio;

use crate::support::elf::ElfBytes;
use crate::support::{
    DEEP_SAMPLE, Exit, LIBRARY_SAMPLE, Run, SAMPLE, TCM_SAMPLE, assert_output, command, crossing,
    run, samples, stderr, stdout,
};

/// Splits a command line into its arguments as a shell would, for the
/// quotes that elfq puts around a name such as `'log(char const*)'`.
fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => quoted = !quoted,
            '\\' if !quoted => word.extend(chars.next()),
            ' ' if !quoted => words.push(std::mem::take(&mut word)),
            _ => word.push(c),
        }
    }
    words.push(word);
    words
}

#[test]
fn a_file_alone_gets_an_overview() {
    assert_output(
        &stdout(&[SAMPLE]),
        "\
testdata/bin/c-x86_64.elf: x86-64 position-independent executable, with symbols and DWARF debug info
32 functions and 36 data objects, 4021 bytes of code and data, plus 8 imports

RETAINED   OWN  ITEM
    3635    38  _start
    3597   195  main (through a pointer), kept by _start
    2068   532  process_frame, kept by main
    1083    59  crc32, kept by process_frame
    1024  1024  crc_table, kept by crc32
     453        6 more, kept by process_frame
    1334        41 more, kept by main
     345        13 more

Try:
  elfq testdata/bin/c-x86_64.elf top
  elfq testdata/bin/c-x86_64.elf top --tree
  elfq testdata/bin/c-x86_64.elf retained main
  elfq testdata/bin/c-x86_64.elf why crc_table
  elfq testdata/bin/c-x86_64.elf inlined
",
    );
}

#[test]
fn firmware_gets_the_same_overview() {
    assert_output(
        &stdout(&["testdata/bin/c-cortex-m4.elf"]),
        "\
testdata/bin/c-cortex-m4.elf: 32-bit Arm executable, with symbols and DWARF debug info
25 functions and 31 data objects, 3577 bytes of code and data

MEMORY                  SIZE  SECTIONS
0x08000000..0x08000e0c  3596  .isr_vector .text .rodata .ARM.exidx, load image of .data
0x20000000..0x20000048    72  .data .bss

RETAINED   OWN  ITEM
    3111    76  Reset_Handler
    2565   132  main, kept by Reset_Handler
    1684   376  process_frame, kept by main
    1076    52  crc32, kept by process_frame
    1024  1024  crc_table, kept by crc32
     232        6 more, kept by process_frame
     749        32 more, kept by main
     470        2 more, kept by Reset_Handler
     466        11 more

Try:
  elfq testdata/bin/c-cortex-m4.elf --range 0x08000000..0x08000e0c
  elfq testdata/bin/c-cortex-m4.elf --range 0x20000000..0x20000048
  elfq testdata/bin/c-cortex-m4.elf startup
  elfq testdata/bin/c-cortex-m4.elf top
  elfq testdata/bin/c-cortex-m4.elf top --tree
  elfq testdata/bin/c-cortex-m4.elf retained main
  elfq testdata/bin/c-cortex-m4.elf reach SysTick_Handler
  elfq testdata/bin/c-cortex-m4.elf why crc_table
  elfq testdata/bin/c-cortex-m4.elf inlined
",
    );
    assert_output(
        &stdout(&["testdata/bin/c-cortex-m4.elf", "callers", "SysTick_Handler"]),
        "\
SysTick_Handler is used by 1 data object:

HOW      AT         CALLER
pointer  0x800003c  vector_table

SysTick_Handler is in the vector table: the processor runs it on an interrupt or exception.
",
    );
}

#[test]
fn stripped_firmware_is_followed_from_its_entry_point() {
    // Without symbols or an unwind table, elfq finds the functions by
    // following the code, and names them by address.
    assert_output(
        &stdout(&["testdata/bin/c-cortex-m4-stripped.elf"]),
        "\
testdata/bin/c-cortex-m4-stripped.elf: 32-bit Arm executable, stripped
25 functions and 27 data objects, 3586 bytes of code and data
Items without symbols are named by address, like fn_0x8000040 and .rodata@0x80008f0.

MEMORY                  SIZE  SECTIONS
0x08000000..0x08000e0c  3596  .isr_vector .text .rodata .ARM.exidx, load image of .data
0x20000000..0x20000048    72  .data .bss

RETAINED    OWN  ITEM
    3120    ~76  entry
    2574   ~132  fn_0x8000518, kept by entry
    1684   ~376  fn_0x80002c8, kept by fn_0x8000518
    1076    ~52  fn_0x8000060, kept by fn_0x80002c8
    1024  ~1024  .rodata@0x80009e4, kept by fn_0x8000060
     232         6 more, kept by fn_0x80002c8
     758         28 more, kept by fn_0x8000518
     470         2 more, kept by entry
     466         11 more

Try:
  elfq testdata/bin/c-cortex-m4-stripped.elf --range 0x08000000..0x08000e0c
  elfq testdata/bin/c-cortex-m4-stripped.elf --range 0x20000000..0x20000048
  elfq testdata/bin/c-cortex-m4-stripped.elf startup
  elfq testdata/bin/c-cortex-m4-stripped.elf top
  elfq testdata/bin/c-cortex-m4-stripped.elf top --tree
  elfq testdata/bin/c-cortex-m4-stripped.elf retained entry
  elfq testdata/bin/c-cortex-m4-stripped.elf reach fn_0x80005ec
  elfq testdata/bin/c-cortex-m4-stripped.elf why .rodata@0x80009e4
",
    );
}

#[test]
fn a_shared_library_gets_an_overview() {
    // elfq doesn't know yet which functions a library offers to other files,
    // so it shows no tree of what keeps what.
    assert_output(
        &stdout(&[LIBRARY_SAMPLE]),
        "\
testdata/bin/c-x86_64.so: x86-64 shared library, with symbols and DWARF debug info
38 functions and 34 data objects, 4197 bytes of code and data, plus 7 imports

Try:
  elfq testdata/bin/c-x86_64.so callers log_msg
  elfq testdata/bin/c-x86_64.so callees main
  elfq testdata/bin/c-x86_64.so inlined
",
    );
}

#[test]
fn commands_that_need_roots_refuse_a_shared_library() {
    for (args, what) in [
        (&["top"][..], "what keeps the most bytes"),
        (
            &["retained", "process_frame"],
            "what only process_frame leads to",
        ),
        (&["why", "log_msg"], "why log_msg is"),
        (&["--range", "0x1000+4K"], "what keeps the bytes of a range"),
    ] {
        let args = [&[LIBRARY_SAMPLE][..], args].concat();
        assert_output(
            &stderr(&args),
            &format!("error: elfq can't yet tell {what} in a shared library\n"),
        );
    }
}

#[test]
fn every_command_the_overview_suggests_works() {
    for sample in &samples() {
        let overview = stdout(&[sample]);
        let prefix = format!("  elfq {sample} ");
        for line in overview
            .lines()
            .filter_map(|line| line.strip_prefix(&prefix))
        {
            let mut args = vec![sample.clone()];
            args.extend(shell_words(line));
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let suggested = run(&args);
            // startup exits 1 for a problem it finds.
            match line {
                "startup" => suggested.printed(Exit::ZeroOrOne),
                _ => suggested.printed(0),
            };
        }
    }
}

#[test]
fn top_shows_what_keeps_the_most_bytes() {
    assert_output(
        &stdout(&[SAMPLE, "top"]),
        "\
4021 bytes of code and data, by what keeps them:

RETAINED   OWN  ITEM
    3635    38  _start
    3597   195  main (through a pointer), kept by _start
    2068   532  process_frame, kept by main
    1083    59  crc32, kept by process_frame
    1024  1024  crc_table, kept by crc32
     378   271  apply_filter, kept by process_frame
     107    75  filter_tap, kept by apply_filter
      32    32  fir_coeffs, kept by filter_tap
      54    54  decode_header, kept by process_frame
      21        2 more, kept by process_frame
     705   370  report, kept by main
     162   162  format_u32, kept by report
      80    16  root, kept by report
      48    16  inner, kept by root
      32        2 more, kept by inner
      16    16  leaf_a, kept by root
      46    46  count_nodes, kept by report
      47        6 more, kept by report
     187    84  parse_value, kept by main
     103   103  parse_list, kept by parse_value
     103    51  log_msg, kept by main
      36    20  platform_write, kept by log_msg
      16    16  write@plt, kept by platform_write
      16    16  strlen@plt, kept by log_msg
      57    51  app_tick, kept by main
       6    ~6  .rodata@0x200c, kept by app_tick
      34   ~34  .rodata@0x205e, kept by main
      33    33  dispatch_event, kept by main
      32    32  stats, kept by main
      31    19  on_button (through a pointer), kept by main
      12        2 more, kept by on_button
      25    25  frame_a, kept by main
     127        12 more, kept by main
     152   ~64  __do_global_dtors_aux
      48   ~48  deregister_tm_clones, kept by __do_global_dtors_aux
      40        2 more, kept by __do_global_dtors_aux
      80   ~16  frame_dummy
      64   ~64  register_tm_clones, kept by frame_dummy
      64    64  .plt
      32    32  __abi_tag
      17        5 more

Nothing reaches 3 items, 41 bytes in all: debug_dump, .rodata@0x2056 and _IO_stdin_used.
The program never uses them, or elfq can't see how.
",
    );
    // On Cortex-M the SysTick interrupt also reaches `log_msg`, so it isn't
    // under `main` but at the top, with what only it leads to.
    let firmware = stdout(&["testdata/bin/c-cortex-m4.elf", "top"]);
    assert!(
        firmware.contains("\n     276    26  log_msg\n     216   216  strlen, kept by log_msg\n"),
        "{firmware}"
    );
    // One item that nothing reaches.
    let cpp = stdout(&["testdata/bin/cpp-x86_64.elf", "top"]);
    assert!(
        cpp.ends_with(
            "\n\nNothing reaches _IO_stdin_used, 4 bytes: the program never uses it, or elfq \
             can't see how.\n"
        ),
        "{cpp}"
    );
}

#[test]
fn tree_draws_the_rows_as_a_tree() {
    let tree = stdout(&[SAMPLE, "top", "--tree"]);
    assert!(
        tree.contains(
            "\
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
"
        ),
        "{tree}"
    );
    // The same rows in the same order, each named the same way.
    let rows = stdout(&[SAMPLE, "top"]);
    let names = |text: &str| -> Vec<String> {
        text.lines()
            .map(|line| {
                let line = line.split(", kept by ").next().unwrap();
                line.replace(['│', '├', '└', '─'], " ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    };
    assert_eq!(names(&tree), names(&rows));

    // The overview draws its largest items the same way, for the whole
    // file and for a range.
    let tree = stdout(&[SAMPLE, "--tree"]);
    assert!(
        tree.contains(
            "\
RETAINED   OWN  ITEM
    3635    38  _start
    3597   195  └─ main (through a pointer)
    2068   532     ├─ process_frame
    1083    59     │  ├─ crc32
    1024  1024     │  │  └─ crc_table
     453           │  └─ 6 more
    1334           └─ 41 more
     345        13 more
"
        ),
        "{tree}"
    );
    let tree = stdout(&[TCM_SAMPLE, "--range", "0x0..0xe0", "--tree"]);
    assert!(tree.contains("     200    0  └─ main\n"), "{tree}");

    // why's chains too.
    let tree = stdout(&[SAMPLE, "why", "log_msg", "--tree"]);
    assert!(
        tree.contains(
            "\
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
"
        ),
        "{tree}"
    );
}

#[test]
fn only_trees_are_drawn() {
    for args in [
        &[SAMPLE, "top", "--tree", "--json"][..],
        &[SAMPLE, "--json", "--tree"],
    ] {
        assert_output(
            &stderr(args),
            "error: --tree draws trees in the text, so it doesn't go with --json\n",
        );
    }
    for args in [
        &[SAMPLE, "callers", "log_msg", "--tree"][..],
        &[SAMPLE, "callees", "--tree", "0x1000+4K"],
        &[SAMPLE, "inlined", "--tree"],
        &[SAMPLE, "debug", "items", "--tree"],
    ] {
        assert_output(
            &stderr(args),
            "error: only the overview, top, retained, why, reach SYMBOL and inlined SYMBOL take --tree\n",
        );
    }
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
0x30000000..0x30000edc  3804  .isr_vector .text .rodata .ARM.exidx, load images of .itcm .data
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
  elfq testdata/bin/c-cortex-m7.elf startup
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
    assert_output(
        &stdout(&[TCM_SAMPLE, "top", "--range", "0x0..0xe0"]),
        "\
224 bytes of code in 0x00000000..0x000000e0 (.itcm), by what keeps them:

RETAINED  OWN  ITEM
     200    0  Reset_Handler
     200    0  main, kept by Reset_Handler
     200    0  process_frame, kept by main
     152    0  __apply_filter_veneer, kept by process_frame
     152   92  apply_filter, kept by __apply_filter_veneer
      52   52  filter_tap, kept by apply_filter
       8    8  __memcpy_veneer, kept by apply_filter
      48    0  __crc32_veneer, kept by process_frame
      48   48  crc32, kept by __crc32_veneer
      24   16  SysTick_Handler
       8    8  __app_tick_veneer, kept by SysTick_Handler
",
    );
    // A row that sums up items counts only those with bytes in the range:
    // under main, it stands for six items in flash, and leaves out
    // config_sum, frames_bad and frames_ok, which are in .bss.
    let flash = stdout(&[TCM_SAMPLE, "top", "--range", "0x30000000..0x30000edc"]);
    assert!(
        flash.contains("      90        6 more, kept by main\n"),
        "{flash}"
    );
    // The same range as a start and a size, with a unit in any case.
    assert_output(
        &stdout(&[TCM_SAMPLE, "top", "--range", "0+224B"]),
        &stdout(&[TCM_SAMPLE, "top", "--range", "0x0..0xe0"]),
    );
    assert_output(
        &stdout(&[TCM_SAMPLE, "--range", "0x20240000+4k"]),
        "\
testdata/bin/c-cortex-m7.elf: 32-bit Arm executable, with symbols and DWARF debug info
1 data object in 0x20240000..0x20241000 (.ocram_bss), 4096 bytes of data

RETAINED   OWN  ITEM
    4096     0  log_msg
    4096     0  platform_write, kept by log_msg
    4096  4096  tx_buffer, kept by platform_write

Try:
  elfq testdata/bin/c-cortex-m7.elf top --range 0x20240000..0x20241000
  elfq testdata/bin/c-cortex-m7.elf why tx_buffer
",
    );
}

#[test]
fn a_range_counts_load_images_where_they_are_stored() {
    // The startup code copies .itcm and .data from flash, so flash holds
    // their load images: the code and data that process_frame alone uses
    // there take 1712 bytes of flash, not 488.
    assert_output(
        &stdout(&[
            TCM_SAMPLE,
            "retained",
            "process_frame",
            "--range",
            "0x30000000+16M",
        ]),
        "\
Removing process_frame would save 1712 bytes in 0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx, with the load images of .itcm and .data):

RETAINED   OWN  ITEM
    1712   376  process_frame
    1080     8  __crc32_veneer, kept by process_frame
    1072    48  crc32 (load image), kept by __crc32_veneer
    1024  1024  crc_table (load image), kept by crc32
     192     8  __apply_filter_veneer, kept by process_frame
     184    92  apply_filter (load image), kept by __apply_filter_veneer
      84    52  filter_tap (load image), kept by apply_filter
      32    32  fir_coeffs, kept by filter_tap
       8     8  __memcpy_veneer (load image), kept by apply_filter
      40    40  decode_header, kept by process_frame
      12   ~12  .rodata@0x30000df0, kept by process_frame
      12   ~12  .rodata@0x30000dfc, kept by process_frame

Shared with other code, so not counted: dispatch_event, log_msg, memcpy, stats.
",
    );
    let flash = stdout(&[TCM_SAMPLE, "top", "--range", "0x30000000..0x30000edc"]);
    assert!(
        flash.starts_with(
            "3691 bytes of code and data in 0x30000000..0x30000edc (.isr_vector, .text, .rodata \
             and .ARM.exidx, with the load images of .itcm and .data), by what keeps them:\n"
        ),
        "{flash}"
    );
    // A range of only a load image.
    assert_output(
        &stdout(&[TCM_SAMPLE, "top", "--range", "0x30000040+224"]),
        "\
224 bytes of code in 0x30000040..0x30000120 (the load image of .itcm), by what keeps them:

RETAINED  OWN  ITEM
     200    0  Reset_Handler
     200    0  main, kept by Reset_Handler
     200    0  process_frame, kept by main
     152    0  __apply_filter_veneer, kept by process_frame
     152   92  apply_filter (load image), kept by __apply_filter_veneer
      52   52  filter_tap (load image), kept by apply_filter
       8    8  __memcpy_veneer (load image), kept by apply_filter
      48    0  __crc32_veneer, kept by process_frame
      48   48  crc32 (load image), kept by __crc32_veneer
      24   16  SysTick_Handler (load image)
       8    8  __app_tick_veneer (load image), kept by SysTick_Handler
",
    );
    // A range that holds both copies counts both.
    assert_output(
        &stdout(&[TCM_SAMPLE, "retained", "crc32", "--range", "0x0..0x40000000"]),
        "\
Removing crc32 would save 2144 bytes in 0x00000000..0x40000000 (.isr_vector, .itcm, .text, .data, .rodata, .ARM.exidx, .bss and .ocram_bss, with the load images of .itcm and .data):

RETAINED   OWN  ITEM
    2144    96  crc32 (with its load image)
    2048  2048  crc_table (with its load image), kept by crc32
",
    );
    assert_output(
        &stdout(&[
            TCM_SAMPLE,
            "retained",
            "crc_table",
            "--range",
            "0x30000000+16M",
        ]),
        "Removing crc_table would save only its own 1024 bytes in 0x30000000..0x31000000 \
         (.isr_vector, .text, .rodata and .ARM.exidx, with the load images of .itcm and .data), \
         all in its load image.\n",
    );
    // A range that holds both copies counts both.
    assert_output(
        &stdout(&[
            TCM_SAMPLE,
            "retained",
            "crc_table",
            "--range",
            "0x0..0x40000000",
        ]),
        "Removing crc_table would save only its own 2048 bytes in 0x00000000..0x40000000 \
         (.isr_vector, .itcm, .text, .data, .rodata, .ARM.exidx, .bss and .ocram_bss, with the \
         load images of .itcm and .data), 1024 of them in its load image.\n",
    );
    // Range checks look only where the program uses its code and data.
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x30000040+224"]),
        "error: elfq found no functions or data in 0x30000040..0x30000120 (the load image of \
         .itcm); the program uses .itcm at 0x00000000..0x000000e0\n",
    );
}

#[test]
fn a_range_that_nothing_reaches_has_no_tree() {
    // `debug_dump` is all that is in this range.
    assert_output(
        &stdout(&[SAMPLE, "top", "--range", "0x1aa0+29"]),
        "Nothing reaches debug_dump, 29 bytes: the program never uses it, or elfq can't see \
         how.\n",
    );
    assert_output(
        &stdout(&[SAMPLE, "--range", "0x1aa0+29"]),
        "\
testdata/bin/c-x86_64.elf: x86-64 position-independent executable, with symbols and DWARF debug info
1 function in 0x00001aa0..0x00001abd (.text), 29 bytes of code

Try:
  elfq testdata/bin/c-x86_64.elf top --range 0x00001aa0..0x00001abd
",
    );
}

#[test]
fn retained_counts_only_the_range_too() {
    assert_output(
        &stdout(&[
            TCM_SAMPLE,
            "retained",
            "process_frame",
            "--range",
            "0x0..0xe0",
        ]),
        "\
Removing process_frame would save 200 bytes in 0x00000000..0x000000e0 (.itcm):

RETAINED  OWN  ITEM
     200    0  process_frame
     152    0  __apply_filter_veneer, kept by process_frame
     152   92  apply_filter, kept by __apply_filter_veneer
      52   52  filter_tap, kept by apply_filter
       8    8  __memcpy_veneer, kept by apply_filter
      48    0  __crc32_veneer, kept by process_frame
      48   48  crc32, kept by __crc32_veneer
",
    );
    assert_output(
        &stdout(&[TCM_SAMPLE, "retained", "crc32", "--range", "0x0..0xe0"]),
        "Removing crc32 would save only its own 48 bytes in 0x00000000..0x000000e0 (.itcm).\n",
    );
    assert_output(
        &stdout(&[TCM_SAMPLE, "retained", "log_msg", "--range", "0x0..0xe0"]),
        "Removing log_msg would save none of the bytes in 0x00000000..0x000000e0 (.itcm).\n",
    );
}

#[test]
fn a_range_must_hold_something() {
    assert_output(
        &stderr(&[TCM_SAMPLE, "top", "--range", "0x40000000+4K"]),
        "error: the file has nothing in 0x40000000..0x40001000; the overview lists its ranges\n",
    );
    // The range is written as the overview writes it, whatever its spelling.
    assert_output(
        &stderr(&[TCM_SAMPLE, "top", "--range", "0x4000000+16"]),
        "error: the file has nothing in 0x04000000..0x04000010; the overview lists its ranges\n",
    );
    // A file in one block of addresses has no list of them in its overview,
    // so the error names the block.
    assert_output(
        &stderr(&[SAMPLE, "top", "--range", "0x900000+4K"]),
        "error: the file has nothing in 0x00900000..0x00901000, only in 0x00000318..0x00004088\n",
    );
    // A section with no functions or data, such as the unwind table.
    assert_output(
        &stderr(&[SAMPLE, "--range", "0x25f0+0x2c8"]),
        "error: elfq found no functions or data in 0x000025f0..0x000028b8 (.eh_frame)\n",
    );
    // A function with no size in the symbol table, as in hand-written
    // assembly: glibc's `_init`.
    assert_output(
        &stderr(&[SAMPLE, "top", "--range", "0x1000+4"]),
        "error: elfq doesn't know where _init ends, so it can't count the bytes in \
         0x00001000..0x00001004 (.init)\n",
    );
    // The overview's JSON refuses such ranges as its text does.
    for (args, error) in [
        (
            &[TCM_SAMPLE, "--range", "0x40000000+4K"],
            "the file has nothing in",
        ),
        (
            &[SAMPLE, "--range", "0x25f0+0x2c8"],
            "found no functions or data in",
        ),
        (
            &[SAMPLE, "--range", "0x1000+4"],
            "doesn't know where _init ends",
        ),
    ] {
        let text = stderr(args);
        assert!(text.contains(error), "{text}");
        assert_output(&stderr(&[args, &["--json"][..]].concat()), &text);
    }
    assert!(
        stderr(&[TCM_SAMPLE, "--range", "0x20000000"])
            .contains("expected START..END or START+SIZE")
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "why", "crc32", "--range", "0+1K"]),
        "error: only the overview, top and retained take --range\n",
    );
}

#[test]
fn retained_shows_what_a_function_keeps() {
    assert_output(
        &stdout(&[SAMPLE, "retained", "process_frame"]),
        "\
Removing process_frame would save 2068 bytes:

RETAINED   OWN  ITEM
    2068   532  process_frame
    1083    59  crc32, kept by process_frame
    1024  1024  crc_table, kept by crc32
     378   271  apply_filter, kept by process_frame
     107    75  filter_tap, kept by apply_filter
      32    32  fir_coeffs, kept by filter_tap
      54    54  decode_header, kept by process_frame
      12   ~12  .rodata@0x2012, kept by process_frame
       9    ~9  .rodata@0x201e, kept by process_frame

Shared with other code, so not counted: __stack_chk_fail@plt, dispatch_event, frames_bad, frames_ok, log_msg, stats.
"
    );
    assert_output(
        &stdout(&[SAMPLE, "retained", "crc_table"]),
        "Removing crc_table would save only its own 1024 bytes.\n",
    );
    assert_output(
        &stdout(&[SAMPLE, "retained", "completed.0"]),
        "Removing completed.0 would save only its own 1 byte.\n",
    );
}

#[test]
fn entry_points_that_share_code_count_it_once() {
    // libgcc's __aeabi_dsub runs on into __adddf3, and __aeabi_ul2d jumps
    // into __aeabi_l2d, each of whose symbols runs to the end they share.
    let sample = "testdata/bin/c-softfloat-nodebug.elf";
    assert_output(
        &stdout(&[sample, "top"]),
        "\
1314 bytes of code and data, by what keeps them:

RETAINED  OWN  ITEM
    1064  116  Reset_Handler
     630  630  __adddf3, kept by Reset_Handler
     156   18  __aeabi_dcmplt, kept by Reset_Handler
     138   16  __aeabi_cdcmpeq, kept by __aeabi_dcmplt
     122  122  __cmpdf2, kept by __aeabi_cdcmpeq
     106   16  __aeabi_ul2d, kept by Reset_Handler
      90   90  __aeabi_l2d, kept by __aeabi_ul2d
       8    8  count, kept by Reset_Handler
       8    8  converted, kept by Reset_Handler
       8    8  difference, kept by Reset_Handler
       8    8  sum, kept by Reset_Handler
       8    8  y, kept by Reset_Handler
       8    8  x, kept by Reset_Handler
       4    4  __aeabi_dsub, kept by Reset_Handler
       4    4  less, kept by Reset_Handler
       8    8  vector_table

Nothing reaches 11 functions, 242 bytes in all: __aeabi_f2d, __aeabi_i2d, __aeabi_ui2d, __aeabi_dcmpeq, __aeabi_dcmple, __aeabi_dcmpge, __aeabi_dcmpgt, __aeabi_cdrcmple, __aeabi_drsub, __gedf2 and __ledf2.
The program never uses them, or elfq can't see how.
",
    );
    assert_output(
        &stdout(&[sample, "retained", "__aeabi_dsub"]),
        "\
Removing __aeabi_dsub would save only its own 4 bytes.

Shared with other code, so not counted: __adddf3.
",
    );
}

#[test]
fn a_large_tree_shows_its_largest_items() {
    let output = stdout(&[SAMPLE, "retained", "main"]);
    assert!(
        output.starts_with("Removing main would save 3597 bytes:\n"),
        "{output}"
    );
    // Smaller items are summed up under the item that keeps them.
    assert!(
        output.contains("\n      21        2 more, kept by process_frame\n"),
        "{output}"
    );
    // The handlers are only called through the `handlers` table.
    assert!(
        output.contains("  on_button (through a pointer), kept by main\n"),
        "{output}"
    );
    // The jump through the `handlers` table, in `dispatch_event`.
    assert!(
        output.ends_with(
            "dispatch_event also jumps through a pointer, so what that reaches may count \
             elsewhere.\n"
        ),
        "{output}"
    );
}

#[test]
fn no_line_sums_up_a_single_item() {
    // A row such as "12 more, kept by main" counts every item it sums up,
    // so one that would sum up a single item shows the item instead, and so
    // do lists of names.
    for sample in &samples() {
        let overview = stdout(&[sample]);
        let mut runs = vec![vec![sample.as_str()]];
        // `top` refuses shared libraries.
        if !sample.ends_with(".so") {
            runs.push(vec![sample, "top"]);
        }
        // The ranges in the overview's Try list.
        for (_, range) in overview
            .lines()
            .filter_map(|line| line.split_once(" --range "))
        {
            runs.push(vec![sample, "--range", range]);
            runs.push(vec![sample, "top", "--range", range]);
        }
        // What the item that the Try list asks about reaches.
        for line in overview.lines() {
            if let Some((_, symbol)) = line
                .split_once(" retained ")
                .or_else(|| line.split_once(" callees "))
            {
                runs.push(vec![sample, "reach", symbol]);
                runs.push(vec![sample, "reach", symbol, "--tree"]);
            }
        }
        for args in &runs {
            let text = stdout(args);
            assert!(
                !text.lines().any(|line| line.contains(" 1 more")),
                "{args:?}\n{text}"
            );
        }
    }
}

#[test]
fn long_lists_name_the_first_few() {
    let sample = "testdata/bin/rust-x86_64-nodebug.elf";
    let choices = stderr(&[sample, "callers", "fmt"]);
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
    assert_output(
        &stdout(&[DEEP_SAMPLE, "retained", "chain1", "--tree"]),
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
",
    );
}

#[test]
fn a_deep_tree_shows_as_rows_in_place() {
    assert_output(
        &stdout(&[DEEP_SAMPLE, "retained", "chain1"]),
        "\
Removing chain1 would save 402 bytes:

RETAINED  OWN  ITEM
     402   16  chain1
     386   17  chain2, kept by chain1
     369   17  chain3, kept by chain2
     352   20  chain4, kept by chain3
     332   17  chain5, kept by chain4
     315   20  chain6, kept by chain5
     295   27  chain7, kept by chain6
     268   20  chain8, kept by chain7
     248   17  chain9, kept by chain8
     231   20  chain10, kept by chain9
     211   20  chain11, kept by chain10
     191   23  chain12, kept by chain11
     168   20  chain13, kept by chain12
     148   19  chain14, kept by chain13
     129   23  chain15, kept by chain14
     106   19  chain16, kept by chain15
      87   22  chain17, kept by chain16
      65   20  chain18, kept by chain17
      45   20  chain19, kept by chain18
      25   25  chain20, kept by chain19

Shared with other code, so not counted: chain_table.
",
    );
}

#[test]
fn every_tree_is_at_most_nine_levels_deep() {
    // Each command, and the rows whose part of the tree is continued below.
    let trees = [
        (
            &[DEEP_SAMPLE, "top", "--tree"][..],
            &["stage7", "stage15"][..],
        ),
        (
            &[DEEP_SAMPLE, "retained", "stage1", "--tree"],
            &["stage9", "stage17"],
        ),
        (
            &[DEEP_SAMPLE, "why", "chain_table", "--tree"],
            &["chain7", "chain15"],
        ),
        (
            &[DEEP_SAMPLE, "why", "stage_table", "--tree"],
            &["stage7", "stage15"],
        ),
        (
            &[DEEP_SAMPLE, "inlined", "run_steps", "--tree"],
            &["step9", "step17"],
        ),
        (
            &[DEEP_SAMPLE, "reach", "chain1", "--tree"],
            &["chain9", "chain17"],
        ),
        (
            &[
                "testdata/bin/rust-x86_64-nodebug.elf",
                "why",
                "memset",
                "--tree",
            ],
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
             so what that reaches may count elsewhere.\n"
        ),
        "{retained}"
    );
    // Calls through pointers in what only `main` leads to, and in both
    // `process_frame` and what only it leads to: the note names the
    // functions that make them, SYMBOL first.
    assert!(
        stdout(&["testdata/bin/cpp-x86_64.elf", "retained", "main"]).ends_with(
            "app::process_frame(app::Frame const&, app::Stage&) and app::dispatch(unsigned int, \
             unsigned int) also call and jump through pointers, so what they reach may count \
             elsewhere.\n"
        )
    );
    assert!(
        stdout(&[
            "testdata/bin/rust-cortex-m4.elf",
            "retained",
            "elfq_sample::process_frame"
        ])
        .ends_with(
            "elfq_sample::process_frame, <core::fmt::Formatter>::pad_integral and \
             <core::fmt::Formatter>::pad_integral::write_prefix also call and jump through \
             pointers, so what they reach may count elsewhere.\n"
        )
    );
    // `main` takes the address of `on_button` to store it in a table.
    assert_output(
        &stdout(&[SAMPLE, "callers", "on_button"]),
        "\
on_button is used by 1 function:

HOW            AT      CALLER
address taken  0x10b6  main

Calls to on_button through a pointer aren't listed, since elfq can't see them.
",
    );
    // Where the program starts, nothing in the file calls it.
    assert_output(
        &stdout(&[SAMPLE, "callers", "_init"]),
        "\
Nothing in the file calls or uses _init.
_init is in .init, which the program uses when it starts.
",
    );
}

/// Whether `text` sums up what it leaves out: a row such as "6 more, kept by
/// main" or "70 more", or a list in a sentence such as "a, b and 48 more.".
/// The AT column keeps its cut, as in "0x3a249 and 3 more  .rodata@0x6044".
fn sums_up(text: &str) -> bool {
    text.lines()
        .any(|line| line.contains(" more") && !line.contains(" more  "))
}

#[test]
fn all_shows_every_item() {
    let sample = "testdata/bin/rust-x86_64-nodebug.elf";
    let firmware = "testdata/bin/rust-cortex-m4.elf";
    for args in [
        vec![sample, "top"],
        vec![sample, "retained", "main"],
        vec![firmware, "inlined"],
        vec![sample, "callers", "memcpy"],
        vec![sample, "callees", "std::rt::lang_start_internal"],
    ] {
        let largest = stdout(&args);
        let all = stdout(&[&args[..], &["--all"]].concat());
        assert!(sums_up(&largest), "{args:?}\n{largest}");
        assert!(!sums_up(&all), "{args:?}\n{all}");
        assert!(all.lines().count() > largest.lines().count());
    }
    // A range check lists every crossing either way, and --all names every
    // function in the sentence under the table.
    let range = [sample, "callers", "0x20000+0x10000"];
    let largest = crossing(&range);
    let all = crossing(&[&range[..], &["--all"]].concat());
    assert!(largest.contains(" and 9 more aren't listed"), "{largest}");
    assert!(!sums_up(&all), "{all}");
    assert_eq!(all.lines().count(), largest.lines().count());
    // why draws the chains through the nearest 10 users, or every one.
    let chains = |why: &str| why.lines().filter(|line| line.ends_with(" memcpy")).count();
    let why = stdout(&[sample, "why", "memcpy"]);
    assert!(
        why.starts_with(
            "memcpy is used by 57 functions; these are the chains through the nearest 10:\n"
        ),
        "{why}"
    );
    assert_eq!(chains(&why), 10, "{why}");
    let why = stdout(&[sample, "why", "memcpy", "--all"]);
    assert!(
        why.starts_with("memcpy is used by 57 functions, through these chains:\n"),
        "{why}"
    );
    assert_eq!(chains(&why), 57, "{why}");
    // A tree of fewer than 30 items is the same either way.
    assert_output(
        &stdout(&[SAMPLE, "retained", "process_frame", "--all"]),
        &stdout(&[SAMPLE, "retained", "process_frame"]),
    );
}

#[test]
fn dead_code_keeps_nothing() {
    assert_output(
        &stdout(&[SAMPLE, "retained", "debug_dump"]),
        "Nothing reaches debug_dump: the program never uses it, or elfq can't see how.\n",
    );
}

#[test]
fn why_shows_the_chains_that_keep_an_item() {
    assert_output(
        &stdout(&[SAMPLE, "why", "log_msg"]),
        "\
log_msg is used by 4 functions, through these chains:

HOW            ITEM
entry point    _start
address taken  main
call           app_tick
call           log_msg
address taken  on_button, used by main
tail call      log_msg
call           process_frame, used by main
call           log_msg
call           report, used by main
call           log_msg

debug_dump uses log_msg too, but nothing reaches debug_dump.

Removing main would remove log_msg too.
",
    );
    // parse_list calls parse_value back, so it doesn't keep it.
    let why = stdout(&[SAMPLE, "why", "parse_value"]);
    assert!(
        why.contains(
            "\nparse_list uses parse_value too, but only parse_value leads to parse_list.\n"
        ),
        "{why}"
    );
    // With exactly 11 users, every chain is shown, since a summary never
    // stands for a single item.
    let why = stdout(&["testdata/bin/far-gnu-m4-nodebug.elf", "why", "counter"]);
    assert!(
        why.starts_with("counter is used by 11 functions, through these chains:\n"),
        "{why}"
    );
    assert_eq!(
        why.lines()
            .filter(|line| line.ends_with(" counter"))
            .count(),
        11
    );
    // With more, it shows the chains through the nearest ten.
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
    assert_output(
        &stdout(&["testdata/bin/c-cortex-m4.elf", "why", "app_tick"]),
        "\
app_tick is used by 2 functions, through these chains:

HOW           ITEM
entry point   Reset_Handler
call          main
call          app_tick
vector table  SysTick_Handler
tail call     app_tick

Removing any single item wouldn't remove app_tick.
",
    );
}

#[test]
fn why_explains_where_the_program_starts() {
    // A root of each kind, and for startup code, when the program runs it.
    let rust = "testdata/bin/rust-x86_64-nodebug.elf";
    let firmware = "testdata/bin/c-cortex-m4.elf";
    for (file, symbol, sentence) in [
        (
            SAMPLE,
            "_start",
            "_start is the entry point, where the program starts.",
        ),
        (
            SAMPLE,
            "_init",
            "_init is in .init, which the program uses when it starts.",
        ),
        (
            SAMPLE,
            "_fini",
            "_fini is in .fini, which the program uses when it exits.",
        ),
        (
            SAMPLE,
            "frame_dummy",
            "frame_dummy is listed in .init_array, so the program runs it when it starts.",
        ),
        (
            SAMPLE,
            "__do_global_dtors_aux",
            "__do_global_dtors_aux is listed in .fini_array, so the program runs it when it \
             exits.",
        ),
        (
            SAMPLE,
            "__abi_tag",
            "__abi_tag is a note in .note.ABI-tag, which the system reads when it loads the \
             program.",
        ),
        (
            SAMPLE,
            ".plt",
            ".plt is the PLT header, which the dynamic loader uses to find imports.",
        ),
        (
            rust,
            "rust_eh_personality",
            "rust_eh_personality is a personality routine, which the unwinder calls on a panic \
             or exception.",
        ),
        (
            firmware,
            "vector_table",
            "vector_table holds the vector table, where the processor finds its handlers.",
        ),
        (
            firmware,
            "SysTick_Handler",
            "SysTick_Handler is in the vector table: the processor runs it on an interrupt or \
             exception.",
        ),
    ] {
        assert_output(&stdout(&[file, "why", symbol]), &format!("{sentence}\n"));
    }
}

#[test]
fn why_says_when_nothing_reaches_an_item() {
    assert_output(
        &stdout(&[SAMPLE, "why", "debug_dump"]),
        "Nothing reaches debug_dump: the program never uses it, or elfq can't see how.\n",
    );
    // The string that only `debug_dump` prints.
    assert_output(
        &stdout(&[SAMPLE, "why", ".rodata@0x2056"]),
        "Nothing reaches .rodata@0x2056: the program never uses it, or elfq can't see how.\n\
         debug_dump uses it, but nothing reaches debug_dump either.\n",
    );
}

#[test]
fn inlined_ranks_the_functions_the_compiler_copied() {
    assert_output(
        &stdout(&[SAMPLE, "inlined"]),
        "\
Inlined copies of 5 functions take 420 of the 2557 bytes of code:

BYTES  COPIES  FUNCTION
  164       1  update_stats
  150       6  append
   74       1  unpack_samples
   32       1  saturate
    0       1  load_le32

A copy of 0 bytes is one the compiler merged into the code around it.
",
    );
    assert_output(
        &stdout(&[SAMPLE, "inlined", "process_frame"]),
        "\
Of process_frame's 532 bytes, 238 are inlined copies of 3 functions:

BYTES  COPIES  FUNCTION
  164       1  update_stats
   74       1  unpack_samples
    0       1  load_le32

A copy of 0 bytes is one the compiler merged into the code around it.
",
    );
    // The six calls to `append` in `report` make one row.
    assert_output(
        &stdout(&[SAMPLE, "inlined", "report"]),
        "\
Of report's 370 bytes, 150 are inlined copies of 1 function:

BYTES  COPIES  FUNCTION
  150       6  append
",
    );
    assert_output(
        &stdout(&[SAMPLE, "inlined", "main"]),
        "The debug info lists no inlined code in main.\n",
    );
}

#[test]
fn inlined_shows_copies_inside_copies() {
    let rows = stdout(&["testdata/bin/rust-cortex-m4.elf", "inlined", "memcpy"]);
    assert!(
        rows.starts_with(
            "\
Of compiler_builtins::mem::memcpy's 730 bytes, 712 are inlined copies of 17 functions:

BYTES  COPIES  FUNCTION
  712       1  compiler_builtins::mem::impls::copy_forward
  392       1  compiler_builtins::mem::impls::copy_forward::copy_forward_misaligned_words, inlined into copy_forward
   74       1  compiler_builtins::mem::impls::load_aligned_partial, inlined into copy_forward_misaligned_words
"
        ),
        "{rows}"
    );
    // A function inlined in several places names where its copies are.
    let rows = stdout(&[
        "testdata/bin/rust-cortex-m4.elf",
        "inlined",
        "process_frame",
    ]);
    for row in [
        "   14       1  core::sync::atomic::atomic_add::<u32, u32>, inlined into fetch_add in \
         update_stats\n",
        "   42       3  core::sync::atomic::atomic_add::<u32, u32>, inlined into fetch_add in \
         process_frame\n",
    ] {
        assert!(rows.contains(row), "{rows}");
    }
    let tree = stdout(&[
        "testdata/bin/rust-cortex-m4.elf",
        "inlined",
        "memcpy",
        "--tree",
    ]);
    assert!(
        tree.contains(
            "\
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
    assert_output(
        &stderr(&["testdata/bin/c-x86_64-nodebug.elf", "inlined"]),
        "error: testdata/bin/c-x86_64-nodebug.elf has no DWARF debug info; build it with debug \
         info and don't strip it\n",
    );
}

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
    // Overloads share a name, so their parameters tell them apart.
    assert_output(
        &stderr(&[CPP, "callers", "log"]),
        "\
error: `log` could mean 2 functions; use one on the left instead:
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

#[test]
fn a_reader_that_stops_early_keeps_the_exit_status() {
    // As in `elfq ... | head`, in a shell with `set -o pipefail`. Each
    // answer is larger than a pipe holds, so elfq writes after the reader
    // has gone.
    for (args, code) in [
        (&["callees", "0x1000+0x40000"][..], 1),
        (&["callees", "0x1000+0x40000", "--json"], 1),
        (&["reach", "main", "--to", "0x1000+0x40000"], 1),
        (&["reach", "main", "--to", "0x1000+0x40000", "--json"], 1),
        (&["top", "--all"], 0),
    ] {
        let sample = "testdata/bin/rust-x86_64-nodebug.elf";
        let mut child = command()
            .arg(sample)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stdout.take());
        let output = child.wait_with_output().unwrap();
        let command = format!("elfq {sample} {}", args.join(" "));
        Run::new(&command, output).printed(code);
    }
}

#[test]
fn an_error_keeps_its_exit_status_when_nothing_reads_it() {
    // As in `elfq ... 2>&1 | head -0`: the error is written after the
    // reader has gone.
    for args in [
        &[SAMPLE, "callers", "nothing_has_this_name"][..],
        &["callers", SAMPLE, "log_msg"],
    ] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let status = command()
            .args(args)
            .stdout(Stdio::null())
            .stderr(writer)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn arguments_that_dont_go_together_are_refused_before_the_file_is_read() {
    // So that a mistake costs no time on a large file. This file doesn't
    // exist.
    for (args, error) in [
        (
            &["missing.elf", "why", "main", "--range", "0+1K"][..],
            "error: only the overview, top and retained take --range\n",
        ),
        (
            &["missing.elf", "callers"],
            "error: callers needs a SYMBOL, or ranges such as 0x0+256K\n",
        ),
        (
            &["missing.elf", "callers", "log_msg", "--to", "0x0+1K"],
            "error: --to goes with callees; callers takes --from to list only the callers in a \
             range\n",
        ),
        (
            &["missing.elf", "callees", "0x0+1K", "--from", "0x0+1K"],
            "error: --from goes with callers; callees takes --to to list only the callees in a \
             range\n",
        ),
        (
            &["missing.elf", "debug", "items", "--json"],
            "error: the debug commands print only text\n",
        ),
        (
            &["missing.elf", "browse", "--json"],
            "error: browse takes no --json, --tree or --range\n",
        ),
        (
            &["missing.elf", "browse", "main", "--tree"],
            "error: browse takes no --json, --tree or --range\n",
        ),
        (
            &["missing.elf", "browse", "--range", "0+1K"],
            "error: browse takes no --json, --tree or --range\n",
        ),
        (
            &["missing.elf", "top", "--tree", "--json"],
            "error: --tree draws trees in the text, so it doesn't go with --json\n",
        ),
        (
            &["missing.elf", "inlined", "--tree"],
            "error: only the overview, top, retained, why, reach SYMBOL and inlined SYMBOL take --tree\n",
        ),
        (
            &["missing.elf", "diff", "missing-too.elf", "--tree"],
            "error: only the overview, top, retained, why, reach SYMBOL and inlined SYMBOL take --tree\n",
        ),
        (
            &["missing.elf", "diff", "missing-too.elf", "--range", "0+1K"],
            "error: only the overview, top and retained take --range\n",
        ),
        (
            &["missing.elf", "reach", "main", "--tree", "--json"],
            "error: --tree draws trees in the text, so it doesn't go with --json\n",
        ),
        (
            &["missing.elf", "reach", "main", "--range", "0+1K"],
            "error: reach takes --to, not --range, to list only what it reaches in a range: elfq \
             missing.elf reach main --to 0x00000000..0x00000400\n",
        ),
        (
            &[
                "missing.elf",
                "reach",
                "main",
                "--to",
                "0x20000000+1K",
                "--range",
                "0+1K",
                "--json",
            ],
            "error: reach takes --to, not --range, to list only what it reaches in a range: elfq \
             missing.elf reach main --to 0x20000000..0x20000400 --to 0x00000000..0x00000400 \
             --json\n",
        ),
        (
            &[
                "missing.elf",
                "reach",
                "main",
                "--range",
                "0+1K",
                "--all",
                "--tree",
            ],
            "error: reach takes --to, not --range, to list only what it reaches in a range: elfq \
             missing.elf reach main --to 0x00000000..0x00000400 --all --tree\n",
        ),
        (
            &["missing.elf", "reach", "main", "--from", "0x0+1K"],
            "error: --from goes with callers; reach takes --to to list only what it reaches in a \
             range\n",
        ),
        // What a range uses is what callees lists for it.
        (
            &["missing.elf", "reach", "0x0+1K", "--json"],
            "error: reach takes one SYMBOL, not ranges; for what a range uses outside it: elfq \
             missing.elf callees 0x00000000..0x00000400 --json\n",
        ),
        (
            &["missing.elf", "reach", "0x0+1K", "--to", "0x20000000+1K"],
            "error: reach takes one SYMBOL, not ranges; for what a range uses in the --to ranges: \
             elfq missing.elf callees 0x00000000..0x00000400 --to 0x20000000..0x20000400\n",
        ),
        (
            &["missing.elf", "reach", "0x0..zz"],
            "error: reach takes one SYMBOL, not ranges\n",
        ),
    ] {
        assert_output(&stderr(args), error);
    }
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
fn debug_walk_lists_what_the_code_run_from_reset_does() {
    let output = stdout(&[TCM_SAMPLE, "debug", "walk"]);
    let mut lines = output.lines();
    assert_eq!(
        lines.next().unwrap(),
        "from Reset_Handler (0x300005f8), stack pointer 0x20040000, main 0x30000570"
    );
    // The calls of the reset handler's own code: the copies of .itcm and
    // .data, the zeroing of .bss, and main, with what memset left in the
    // registers: its first argument in r0.
    let calls: Vec<&str> = lines
        .clone()
        .filter(|line| line.contains(" 0  call "))
        .collect();
    assert_eq!(
        calls,
        [
            "0x3000060e      0  call memcpy (0x3000087c) (r0=0x0 r1=0x30000040 r2=0xe0 r3=0xdf)",
            "0x30000626      0  call memcpy (0x3000087c) (r0=0x20000000 r1=0x300009c0 r2=0x420 r3=0x41f)",
            "0x3000063e      0  call memset (0x300007d8) (r0=0x20000420 r1=0x0 r2=0x30 r3=0x2f)",
            "0x30000642      0  call main (0x30000570) (r0=0x20000420 r1=0x0 r2=? r3=?), left by a call: r0",
        ]
    );
    assert_eq!(
        lines.last().unwrap(),
        "0x30000642         ends: reaches main"
    );
    // Only Thumb code.
    assert_output(
        &stderr(&[SAMPLE, "debug", "walk"]),
        "error: elfq walks only Thumb code, which Cortex-M processors run\n",
    );
}

#[test]
fn a_file_whose_code_elfq_cant_read_says_so() {
    // The x86-64 sample, marked as AArch64 code, which elfq can't read yet.
    let path = ElfBytes::sample(SAMPLE)
        .mark_code_unreadable()
        .save("aarch64.elf");
    let path = path.as_str();

    assert_output(
        &stdout(&[path]),
        &format!(
            "\
{path}: Aarch64 position-independent executable, with symbols and DWARF debug info
30 functions and 23 data objects, 3873 bytes of code and data
elfq can't yet read Aarch64 code to tell what calls what.
"
        ),
    );
    // Even when SYMBOL names nothing, since no SYMBOL would help.
    for args in [
        &[path, "top"][..],
        &[path, "retained", "no_such_function"],
        &[path, "why", "no_such_function"],
        &[path, "callers", "no_such_function"],
        &[path, "callees", "no_such_function"],
        &[path, "reach", "no_such_function"],
        &[path, "reach"],
        &[
            path,
            "callers",
            "no_such_function",
            "--range",
            "0x1000+0x100",
        ],
        &[path, "callers", "--range", "0x1000+0x100"],
    ] {
        assert_output(
            &stderr(args),
            "error: elfq can't yet read Aarch64 code to tell what calls what\n",
        );
    }
}

#[test]
fn errors_go_to_stderr_with_exit_code_2() {
    assert_output(
        &stderr(&["Cargo.toml"]),
        "error: cannot load Cargo.toml: not an ELF file\n",
    );
    assert_output(
        &stderr(&[SAMPLE, "callers", "proces_frame"]),
        "error: no function or data object is named `proces_frame`; did you mean process_frame?\n",
    );
}

#[test]
fn errors_say_what_to_try() {
    // A range typed with `-` or `:` between its ends.
    let dash = "error: `0x0-0x40000` isn't a range: did you mean 0x0..0x40000?\n";
    assert_output(&stderr(&[TCM_SAMPLE, "callees", "0x0-0x40000"]), dash);
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x0..0x100", "0x0-0x40000"]),
        dash,
    );
    let error = stderr(&[TCM_SAMPLE, "top", "--range", "0x0:0x40000"]);
    assert!(
        error.starts_with(
            "error: invalid value '0x0:0x40000' for '--range <RANGE>': did you mean \
             0x0..0x40000?\n"
        ),
        "{error}"
    );
    // A second number smaller than the first is a size.
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x20000000:256K"]),
        "error: `0x20000000:256K` isn't a range: did you mean 0x20000000+256K?\n",
    );
    // Ends that make no range get the forms a range takes.
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x40000-0x0"]),
        "error: `0x40000-0x0` isn't a range: expected START..END or START+SIZE, such as \
         0x0+256K\n",
    );
    // Several ranges joined by commas.
    let error = stderr(&[TCM_SAMPLE, "top", "--range", "0x0..0x100,0x200..0x300"]);
    assert!(
        error.starts_with(
            "error: invalid value '0x0..0x100,0x200..0x300' for '--range <RANGE>': --range takes \
             one range, such as 0x0..0x100\n"
        ),
        "{error}"
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x0..0x100,0x200..0x300"]),
        "error: `0x0..0x100,0x200..0x300` isn't a range; give each range as its own argument: \
         0x0..0x100 0x200..0x300\n",
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x0..0x100,", "0x200..0x300"]),
        "error: `0x0..0x100,` isn't a range; leave out the comma\n",
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callers", "0x0..0x100,0x200"]),
        "error: `0x200` isn't a range: expected START..END or START+SIZE, such as 0x0+256K\n",
    );

    // A file without a symbol table names its items by address.
    assert_output(
        &stderr(&["testdata/bin/c-cortex-m4-stripped.elf", "callers", "main"]),
        "error: no function or data object is named `main`; the file has no symbol table, so \
         elfq names its functions and data by address, like fn_0x8000040 and .rodata@0x80008f0\n",
    );

    // A function that the compiler inlined everywhere has no item.
    assert_output(
        &stderr(&[SAMPLE, "callers", "append"]),
        "error: no function or data object is named `append`; the compiler inlined all 6 copies \
         of it into report, which `inlined report` lists\n",
    );
    assert_output(
        &stderr(&[
            "testdata/bin/rust-cortex-m4.elf",
            "inlined",
            "copy_forward_bytes",
        ]),
        "error: no function or data object is named `copy_forward_bytes`; the compiler inlined \
         both copies of it into memcpy, which `inlined memcpy` lists\n",
    );
    assert_output(
        &stderr(&[
            "testdata/bin/rust-cortex-m4.elf",
            "why",
            "atomic_add::<u32, u32>",
        ]),
        "error: no function or data object is named `atomic_add::<u32, u32>`; the compiler \
         inlined all 9 copies of it into process_frame, app_main, on_button, on_data, on_timer \
         and sys_tick; `inlined process_frame` lists those in process_frame\n",
    );
    // Suggestions still follow, as the name may not be the one meant.
    assert_output(
        &stderr(&["testdata/bin/c-deep.elf", "why", "printf"]),
        "error: no function or data object is named `printf`; the compiler inlined its one copy \
         into main, which `inlined main` lists; did you mean __printf_chk?\n",
    );
}

#[test]
fn a_command_typed_before_the_file_gets_a_hint() {
    assert_output(
        &stderr(&["callers", SAMPLE, "log_msg"]),
        "error: the file comes before the command, as in: \
         elfq testdata/bin/c-x86_64.elf callers log_msg\n",
    );
    // diff's two files go on either side of it.
    assert_output(
        &stderr(&["diff", "a.elf", "b.elf"]),
        "error: the file comes before the command, as in: elfq a.elf diff b.elf\n",
    );
}

#[test]
fn help_comes_before_a_command_too() {
    let help = stdout(&["help", "top"]);
    assert!(
        help.starts_with("Show the items that keep the most bytes in the file\n"),
        "{help}"
    );
    assert_output(&help, &stdout(&[SAMPLE, "top", "--help"]));
    assert_output(&stdout(&["help"]), &stdout(&["--help"]));
    assert_output(&stdout(&["help", "top", "--json"]), &help);
    // A misspelled command gets the same error as after a file.
    assert!(
        stderr(&["help", "retaind"]).contains("a similar subcommand exists: 'retained'"),
        "{}",
        stderr(&["help", "retaind"])
    );
}

#[test]
fn help_before_a_command_is_that_commands_help() {
    // Hidden commands too: `elfq help debug` is `elfq FILE debug --help`.
    for command in elfq::cli::command_names() {
        assert_output(
            &stdout(&["help", &command]),
            &stdout(&[SAMPLE, &command, "--help"]),
        );
    }
}

#[test]
#[cfg(unix)]
fn an_argument_that_is_not_utf8_is_an_error() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    for args in [[SAMPLE, "retained"], ["retained", SAMPLE]] {
        let output = command()
            .args(args)
            .arg(OsStr::from_bytes(b"\xff"))
            .output()
            .unwrap();
        let command = format!("elfq {} \\xff", args.join(" "));
        let error = Run::new(&command, output).error();
        assert!(error.starts_with("error: "), "{error}");
    }
}

#[test]
fn no_arguments_prints_help() {
    let help = stderr(&[]);
    assert!(
        help.contains("Usage: elfq [OPTIONS] <FILE> [COMMAND]"),
        "{help}"
    );
}
