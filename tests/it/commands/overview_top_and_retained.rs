//! The overview, `top` and `retained`: what keeps the most bytes.
//!
//! All three show the tree of retained sizes, for the whole file and with
//! `--range`, as text and as JSON. They share the tree and `--range`, so
//! many of these tests check more than one of them. The test that the
//! overview suggests `startup` only for firmware that startup reads is in
//! startup.rs, with the files that startup refuses.

use serde_json::{Value, json};

use crate::support::elf::ElfBytes;
use crate::support::{
    DEEP_SAMPLE, Exit, LIBRARY_SAMPLE, SAMPLE, TCM_SAMPLE, assert_output, elfq_json, names, run,
    samples, stderr, stdout, text_rows,
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

#[test]
fn dead_code_keeps_nothing() {
    assert_output(
        &stdout(&[SAMPLE, "retained", "debug_dump"]),
        "Nothing reaches debug_dump: the program never uses it, or elfq can't see how.\n",
    );
}

#[test]
fn the_overview_says_what_the_file_is() {
    let mut overview = elfq_json(&[SAMPLE]);
    // One block, from .interp to .bss.
    let memory = overview["memory"].take();
    assert_eq!(memory.as_array().unwrap().len(), 1);
    assert_eq!(memory[0]["start"], "0x318");
    assert_eq!(memory[0]["end"], "0x4088");
    assert_eq!(memory[0]["size"], 0x4088 - 0x318);
    assert_eq!(memory[0]["sections"][0], ".interp");
    assert_eq!(memory[0]["sections"][26], ".bss");
    assert_eq!(memory[0]["load_images"], json!([]));
    assert_eq!(
        overview,
        json!({
            "file": SAMPLE,
            "architecture": "x86-64",
            "kind": "position-independent executable",
            "has_symbols": true,
            "has_dwarf": true,
            "memory": null,
            "range": null,
            "functions": 32,
            "data_objects": 36,
            "bytes": 4021,
            "imports": 8,
            "references": 119,
            "indirect_calls": 4
        })
    );
}

#[test]
fn firmware_stores_load_images_in_flash() {
    // The starting values of .data, and the code that runs from ITCM, are
    // stored in flash, for the startup code to copy.
    let memory = elfq_json(&[TCM_SAMPLE])["memory"].take();
    assert_eq!(
        memory[3],
        json!({
            "start": "0x30000000",
            "end": "0x30000edc",
            "size": 0xedc,
            "sections": [".isr_vector", ".text", ".rodata", ".ARM.exidx"],
            "load_images": [".itcm", ".data"]
        })
    );
    assert_eq!(memory[0]["load_images"], json!([]));
}

#[test]
fn a_range_limits_the_counts_to_it() {
    let overview = elfq_json(&[TCM_SAMPLE, "--range", "0x0..0xe0"]);
    assert_eq!(overview["range"], json!({"start": "0x0", "end": "0xe0"}));
    assert_eq!(overview["functions"], 6);
    assert_eq!(overview["data_objects"], 0);
    assert_eq!(overview["bytes"], 224);
    assert_eq!(overview["imports"], Value::Null);
    assert_eq!(overview["references"], Value::Null);
    assert_eq!(overview["memory"].as_array().unwrap().len(), 4);

    let top = elfq_json(&[TCM_SAMPLE, "top", "--range", "0x0..0xe0"]);
    assert_eq!(top["range"], json!({"start": "0x0", "end": "0xe0"}));
    assert_eq!(top["bytes"], 224);
    // Only items that keep some of the range's bytes, each with its own
    // bytes in the range.
    assert_eq!(
        names(&top["items"]),
        [
            "Reset_Handler",
            "main",
            "process_frame",
            "__apply_filter_veneer",
            "apply_filter",
            "filter_tap",
            "__memcpy_veneer",
            "__crc32_veneer",
            "crc32",
            "SysTick_Handler",
            "__app_tick_veneer"
        ]
    );
    assert_eq!(top["items"][0]["retained"], 200);
    assert_eq!(top["items"][0]["own"], 0);
    assert_eq!(top["items"][4]["name"], "apply_filter");
    assert_eq!(top["items"][4]["own"], 92);
    assert_eq!(own_bytes(&top["items"]), 224);
    // An item partly in the range counts only its bytes in it, as does an
    // item that nothing reaches.
    let top = elfq_json(&[TCM_SAMPLE, "top", "--range", "0x0..0x80"]);
    let apply_filter = &top["items"][4];
    assert_eq!(apply_filter["name"], "apply_filter");
    assert_eq!(apply_filter["size"], 92);
    assert_eq!(apply_filter["own"], 12);
    assert_eq!(own_bytes(&top["items"]), 128);
    let top = elfq_json(&[SAMPLE, "top", "--range", "0x1a90+0x20"]);
    let debug_dump = &top["unreachable"][0];
    assert_eq!(debug_dump["name"], "debug_dump");
    assert_eq!(debug_dump["size"], 29);
    assert_eq!(debug_dump["own"], 16);
    assert_eq!(top["bytes"], 18);
    assert_eq!(
        own_bytes(&top["items"]) + own_bytes(&top["unreachable"]),
        18
    );

    let retained = elfq_json(&[
        TCM_SAMPLE,
        "retained",
        "crc32",
        "--range",
        "0x20000000+1104",
    ]);
    assert_eq!(
        retained["range"],
        json!({"start": "0x20000000", "end": "0x20000450"})
    );
    assert_eq!(retained["retained"], 1024);
    assert_eq!(names(&retained["items"]), ["crc_table"]);

    let retained = elfq_json(&[TCM_SAMPLE, "retained", "process_frame", "--range", "0+224"]);
    assert_eq!(retained["retained"], 200);
    assert_eq!(
        names(&retained["items"]),
        [
            "__apply_filter_veneer",
            "apply_filter",
            "filter_tap",
            "__memcpy_veneer",
            "__crc32_veneer",
            "crc32"
        ]
    );
}

#[test]
fn rows_say_which_bytes_are_a_load_image() {
    // Flash holds the load images of .itcm and .data, for the startup code
    // to copy.
    let retained = elfq_json(&[
        TCM_SAMPLE,
        "retained",
        "process_frame",
        "--range",
        "0x30000000+16M",
    ]);
    assert_eq!(retained["retained"], 1712);
    assert_eq!(
        (&retained["own"], &retained["load_image"]),
        (&json!(376), &json!(0))
    );
    let rows = retained["items"].as_array().unwrap();
    let row = |name: &str| rows.iter().find(|row| row["name"] == name).unwrap();
    let bytes = |name: &str| {
        let row = row(name);
        (
            row["section"].clone(),
            row["own"].clone(),
            row["load_image"].clone(),
        )
    };
    assert_eq!(
        bytes("crc_table"),
        (json!(".data"), json!(1024), json!(1024))
    );
    assert_eq!(bytes("crc32"), (json!(".itcm"), json!(48), json!(48)));
    assert_eq!(
        bytes("decode_header"),
        (json!(".text"), json!(40), json!(0))
    );
    // A range that holds both copies counts both.
    let retained = elfq_json(&[TCM_SAMPLE, "retained", "crc32", "--range", "0..0x40000000"]);
    assert_eq!(retained["items"][0]["name"], "crc_table");
    assert_eq!(retained["items"][0]["own"], 2048);
    assert_eq!(retained["items"][0]["load_image"], 1024);
    // SYMBOL's own bytes too.
    let retained = elfq_json(&[
        TCM_SAMPLE,
        "retained",
        "crc_table",
        "--range",
        "0..0x40000000",
    ]);
    assert_eq!(
        (&retained["own"], &retained["load_image"]),
        (&json!(2048), &json!(1024))
    );
    // Without a range, every item counts once, where the program uses it.
    let top = elfq_json(&[TCM_SAMPLE, "top"]);
    for list in ["items", "unreachable"] {
        assert!(
            top[list]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["load_image"] == 0)
        );
    }
}

/// The sum of the `own` bytes of the rows in a list.
fn own_bytes(rows: &Value) -> u64 {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|row| row["own"].as_u64().unwrap())
        .sum()
}

#[test]
fn top_has_every_item_in_the_file() {
    let top = elfq_json(&[SAMPLE, "top"]);
    // The rows and what nothing reaches add up to the file.
    assert_eq!(top["bytes"], 4021);
    assert_eq!(
        own_bytes(&top["items"]) + own_bytes(&top["unreachable"]),
        4021
    );
    let items = top["items"].as_array().unwrap();
    assert_eq!(items.len(), 65);
    // So do the top of the tree and what nothing reaches.
    let top_rows = items.iter().filter(|item| item["parent"].is_null());
    let kept: u64 = top_rows
        .map(|item| item["retained"].as_u64().unwrap())
        .sum();
    assert_eq!(kept + own_bytes(&top["unreachable"]), 4021);
    // The rows are numbered in order, and each comes after its parent's.
    for (id, item) in items.iter().enumerate() {
        assert_eq!(item["id"], id);
        assert!(
            item["parent"]
                .as_u64()
                .is_none_or(|parent| parent < id as u64)
        );
    }
    assert_eq!(items[0]["name"], "_start");
    assert_eq!(names(&top["items"]), text_rows(&[SAMPLE, "top"]));
    assert_eq!(
        names(&top["unreachable"]),
        ["debug_dump", ".rodata@0x2056", "_IO_stdin_used"]
    );
}

#[test]
fn retained_has_what_symbol_keeps_as_rows() {
    let retained = elfq_json(&[SAMPLE, "retained", "process_frame"]);
    assert_eq!(retained["reachable"], true);
    assert_eq!(retained["retained"], 2068);
    // process_frame's own bytes, and those of every row.
    assert_eq!(own_bytes(&retained["items"]), 2068 - 532);
    let crc32 = &retained["items"][0];
    assert_eq!(crc32["id"], 0);
    assert_eq!(crc32["name"], "crc32");
    assert_eq!(crc32["own"], 59);
    assert_eq!(crc32["retained"], 1083);
    assert_eq!(crc32["through_pointer"], false);
    // process_frame keeps crc32 itself, and crc32 keeps crc_table.
    assert_eq!(crc32["parent"], Value::Null);
    let crc_table = &retained["items"][1];
    assert_eq!(crc_table["name"], "crc_table");
    assert_eq!(crc_table["parent"], 0);
    assert_eq!(
        names(&retained["shares"]),
        [
            "__stack_chk_fail@plt",
            "dispatch_event",
            "frames_bad",
            "frames_ok",
            "log_msg",
            "stats"
        ]
    );
    // The rows hold every item that only process_frame leads to, in the
    // order of the text's rows, which start with process_frame's own.
    assert_eq!(
        names(&retained["items"]),
        text_rows(&[SAMPLE, "retained", "process_frame"])[1..]
    );
    assert_eq!(
        names(&retained["items"]),
        [
            "crc32",
            "crc_table",
            "apply_filter",
            "filter_tap",
            "fir_coeffs",
            "decode_header",
            ".rodata@0x2012",
            ".rodata@0x201e"
        ]
    );
    // A string literal without a symbol has an estimated size.
    let literal = &retained["items"][6];
    assert_eq!(literal["name"], ".rodata@0x2012");
    assert_eq!(literal["size_estimated"], true);
    assert_eq!(retained["indirect_calls"], 0);
    // The jump through the `handlers` table, in `dispatch_event`.
    assert_eq!(
        elfq_json(&[SAMPLE, "retained", "main"])["indirect_calls"],
        1
    );

    // Nothing is there only because of something that nothing reaches.
    let unreachable = elfq_json(&[SAMPLE, "retained", "debug_dump"]);
    assert_eq!(unreachable["reachable"], false);
    assert_eq!(unreachable["retained"], 29);
    assert_eq!(unreachable["items"], json!([]));
    assert_eq!(unreachable["shares"], json!([]));
}

#[test]
fn deep_trees_are_whole() {
    // `--tree` continues a tree deeper than 9 levels below it; JSON has all
    // of it, each row under the one before.
    let answer = elfq_json(&[DEEP_SAMPLE, "retained", "chain1"]);
    let items = answer["items"].as_array().unwrap();
    let expected: Vec<String> = (2..=20).map(|n| format!("chain{n}")).collect();
    assert_eq!(names(&answer["items"]), expected);
    assert_eq!(items[0]["parent"], Value::Null);
    for (id, item) in items.iter().enumerate().skip(1) {
        assert_eq!(item["parent"], id - 1);
    }
}

#[test]
fn a_function_of_unknown_size_is_named_where_its_load_image_is() {
    // The last function in .itcm, which flash stores from 0x30000118 for
    // the startup code to copy, without a size.
    let path = ElfBytes::sample("testdata/bin/c-cortex-m7-nodebug.elf")
        .set_symbol_size("__memcpy_veneer", 0)
        .save("memcpy-veneer-unsized.elf");
    assert_output(
        &stderr(&[&path, "top", "--range", "0x30000118+8"]),
        "error: elfq doesn't know where __memcpy_veneer ends, so it can't count the bytes in \
         0x30000118..0x30000120 (the load image of .itcm)\n",
    );
}

#[test]
fn the_overview_suggests_reach_for_a_handler_that_runs_in_normal_operation() {
    // The overview of c-cortex-m4-nodebug.elf with `handler` in the given
    // entries of the vector table, counted from the stack pointer's.
    let overview = |handler: &str, entries: &[usize]| {
        let mut data = ElfBytes::sample("testdata/bin/c-cortex-m4-nodebug.elf");
        let address = data.symbol_address(handler);
        for &entry in entries {
            data.set_vector_entry(entry, address | 1);
        }
        run(&[&data.save(&format!("{handler}-in-the-table.elf"))]).printed(0)
    };
    // main reaches more code than SysTick_Handler, but runs from these
    // entries only when something has gone wrong or while debugging: as
    // the HardFault handler, as in most firmware; as the handler of every
    // fault and of DebugMonitor, as in Zephyr's; and as the NMI handler.
    for entries in [&[3][..], &[3, 4, 5, 6, 12], &[2]] {
        let stdout = overview("main", entries);
        assert!(
            stdout.contains(" reach SysTick_Handler\n"),
            "{entries:?}: {stdout}"
        );
    }
    let stdout = overview("main", &[3, 15]);
    assert!(stdout.contains(" reach main\n"), "{stdout}");
    // Default_Handler, the SysTick handler now too, reaches nothing else.
    let stdout = overview("Default_Handler", &[15]);
    assert!(!stdout.contains(" reach "), "{stdout}");
}
