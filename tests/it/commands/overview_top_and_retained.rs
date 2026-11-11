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
    samples, shell_words, stderr, stdout, text_rows,
};

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
fn retained_of_several_counts_what_goes_when_they_all_go() {
    // The rows that the two keep together, and the notes.
    let together = "      32    32  stats, kept by process_frame and report together
      16    16  __stack_chk_fail@plt, kept by process_frame and report together
       4     4  frames_bad, kept by process_frame and report together
       4     4  frames_ok, kept by process_frame and report together

56 of these bytes go only when both go.

Shared with other code, so not counted: config_sum, dispatch_event, log_msg.
";
    assert_output(
        &stdout(&[SAMPLE, "retained", "process_frame", "report"]),
        &format!(
            "\
Removing process_frame and report would save 2829 bytes:

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
     705   370  report
     162   162  format_u32, kept by report
      80    16  root, kept by report
      48    16  inner, kept by root
      16    16  leaf_c, kept by inner
      16    16  leaf_b, kept by inner
      16    16  leaf_a, kept by root
      46    46  count_nodes, kept by report
      10   ~10  .rodata@0x2031, kept by report
      10   ~10  .rodata@0x203b, kept by report
       9    ~9  .rodata@0x2045, kept by report
       8    ~8  .rodata@0x204e, kept by report
       6    ~6  .rodata@0x202b, kept by report
       4    ~4  .rodata@0x2027, kept by report
{together}"
        ),
    );
    // The tree draws each SYMBOL's own branch, and names the keepers of a
    // row that none of them keeps alone in the same words.
    let tree = stdout(&[SAMPLE, "retained", "process_frame", "report", "--tree"]);
    assert!(
        tree.contains("    2068   532  process_frame\n    1083    59  ├─ crc32\n"),
        "{tree}"
    );
    assert!(
        tree.contains("       4    ~4  └─ .rodata@0x2027\n"),
        "{tree}"
    );
    assert!(tree.ends_with(together), "{tree}");
}

#[test]
fn the_note_counts_the_bytes_that_go_only_together() {
    // Of the 4 that the first line counts, debug_dump and _IO_stdin_used,
    // which nothing reaches, keep nothing together.
    let output = stdout(&[
        SAMPLE,
        "retained",
        "process_frame",
        "debug_dump",
        "_IO_stdin_used",
        "report",
    ]);
    assert!(
        output.starts_with("Removing these 4 items would save 2862 bytes:\n"),
        "{output}"
    );
    assert!(
        output.contains("\n56 of these bytes go only when two or more of the 4 go.\n"),
        "{output}"
    );
    let output = stdout(&[
        SAMPLE,
        "retained",
        "register_tm_clones",
        "__do_global_dtors_aux",
    ]);
    assert!(
        output.contains(
            "  completed.0, kept by register_tm_clones and __do_global_dtors_aux together\n\n\
             1 of these bytes goes only when both go.\n"
        ),
        "{output}"
    );
}

#[test]
fn a_symbol_that_nothing_reaches_counts_its_own_bytes() {
    let output = stdout(&[SAMPLE, "retained", "debug_dump", "process_frame"]);
    assert!(
        output.starts_with("Removing debug_dump and process_frame would save 2097 bytes:\n"),
        "{output}"
    );
    assert!(
        output.contains(
            "\nNothing reaches debug_dump: the program never uses it, or elfq can't see how. \
             The total counts its own 29 bytes.\n"
        ),
        "{output}"
    );
    // counts, which nothing reaches, has the only bytes in the range, so
    // there are no rows to show.
    assert_output(
        &stdout(&[
            "testdata/bin/c-cortex-m0-pico.elf",
            "retained",
            "_reset_handler",
            "counts",
            "--range",
            "0x20000000..0x20000034",
        ]),
        "\
Removing _reset_handler and counts would save 32 bytes in 0x20000000..0x20000034 (.data and .bss).

Nothing reaches counts: the program never uses it, or elfq can't see how. The total counts its own 32 bytes.

_reset_handler also calls through pointers, so what they reach may count elsewhere.
",
    );
    let unreachable = [SAMPLE, "retained", "debug_dump", "_IO_stdin_used"];
    assert_output(
        &stdout(&unreachable),
        "Nothing reaches debug_dump and _IO_stdin_used: the program never uses them, or elfq \
         can't see how. Removing them would save their own 33 bytes.\n",
    );
    assert_output(
        &stdout(&[&unreachable[..], &["--range", "0x1000+0x1000"]].concat()),
        "Nothing reaches debug_dump and _IO_stdin_used: the program never uses them, or elfq \
         can't see how. Removing them would save their own 29 bytes in 0x00001000..0x00002000 \
         (.init, .plt, .plt.got, .plt.sec, .text and .fini).\n",
    );
    assert_output(
        &stdout(&[&unreachable[..], &["--range", "0x0+0x10"]].concat()),
        "Nothing reaches debug_dump and _IO_stdin_used: the program never uses them, or elfq \
         can't see how. Removing them would save none of the bytes in 0x00000000..0x00000010.\n",
    );
}

#[test]
fn a_range_that_the_symbols_keep_nothing_in_saves_none() {
    assert_output(
        &stdout(&[
            SAMPLE,
            "retained",
            "debug_dump",
            "process_frame",
            "--range",
            "0x4000+0x100",
        ]),
        "\
Removing debug_dump and process_frame would save none of the bytes in 0x00004000..0x00004100 (.data and .bss).

Nothing reaches debug_dump: the program never uses it, or elfq can't see how.

Shared with other code, so not counted: frames_bad, frames_ok, stats.
",
    );
}

#[test]
fn one_symbol_that_keeps_the_others_saves_the_same_alone() {
    let output = stdout(&[SAMPLE, "retained", "process_frame", "crc32"]);
    assert!(
        output.contains("\nprocess_frame keeps crc32, so removing it alone saves the same.\n"),
        "{output}"
    );
    let output = stdout(&[
        "testdata/bin/cpp-x86_64.elf",
        "retained",
        "vtable for app::Gain",
        "app::Gain::run",
        "typeinfo for app::Gain",
        "typeinfo name for app::Gain",
    ]);
    assert!(
        output.contains(
            "\nvtable for app::Gain keeps the other 3, so removing it alone saves the same.\n"
        ),
        "{output}"
    );
}

#[test]
fn a_symbol_that_another_keeps_adds_nothing() {
    // Code that moves to ITCM, with the veneers that reach it.
    let itcm = [
        TCM_SAMPLE,
        "retained",
        "crc32",
        "apply_filter",
        "filter_tap",
        "__memcpy_veneer",
        "SysTick_Handler",
        "__app_tick_veneer",
    ];
    assert_output(
        &stdout(&itcm),
        "\
Removing these 6 items would save 1284 bytes:

RETAINED   OWN  ITEM
    1072    48  crc32
    1024  1024  crc_table, kept by crc32
     184    92  apply_filter
      84    52  filter_tap, kept by apply_filter
      32    32  fir_coeffs, kept by filter_tap
       8     8  __memcpy_veneer, kept by apply_filter
      28    16  SysTick_Handler
       8     8  __app_tick_veneer, kept by SysTick_Handler
       4     4  systick_count, kept by SysTick_Handler

filter_tap, __memcpy_veneer and __app_tick_veneer add nothing: other items named here keep them.

Shared with other code, so not counted: app_tick, memcpy.
",
    );
    // With a range, only the bytes in it count.
    assert_output(
        &stdout(&[&itcm[..], &["--range", "0x20000000..0x20000450"]].concat()),
        "\
Removing these 6 items would save 1028 bytes in 0x20000000..0x20000450 (.data and .bss):

RETAINED   OWN  ITEM
    1024     0  crc32
    1024  1024  crc_table, kept by crc32
       4     0  SysTick_Handler
       4     4  systick_count, kept by SysTick_Handler

filter_tap, __memcpy_veneer and __app_tick_veneer add nothing: other items named here keep them.
",
    );
    // When nothing reaches some SYMBOLs, removing the one that keeps the
    // others alone doesn't save their own bytes, so the note names the
    // ones it keeps.
    for (args, note) in [
        (
            &[SAMPLE, "process_frame", "crc32", "debug_dump"][..],
            "crc32 adds nothing: other items named here keep it.",
        ),
        (
            &[SAMPLE, "on_data", "data_events", "_IO_stdin_used"],
            "data_events adds nothing: other items named here keep it.",
        ),
        (
            &[
                "testdata/bin/c-softfloat-nodebug.elf",
                "Reset_Handler",
                "__aeabi_drsub",
                "__aeabi_dsub",
                "__aeabi_ui2d",
                "__aeabi_i2d",
                "__aeabi_f2d",
                "__aeabi_l2d",
            ],
            "__aeabi_dsub and __aeabi_l2d add nothing: other items named here keep them.",
        ),
    ] {
        let output = stdout(&[&args[..1], &["retained"], &args[1..]].concat());
        assert!(
            output.contains(&format!("\n\n{note}\n\nNothing reaches ")),
            "{output}"
        );
    }
}

#[test]
fn a_set_of_one_symbol_is_retained_of_that_symbol() {
    // 0x1280 is where crc32 starts, so it names crc32 a second time.
    for args in [
        &["crc32", "crc32"][..],
        &["crc32", "0x1280"],
        &["crc32", "crc32", "--tree"],
    ] {
        let alone = [&[SAMPLE, "retained", "crc32"][..], &args[2..]].concat();
        assert_output(
            &stdout(&[&[SAMPLE, "retained"][..], args].concat()),
            &stdout(&alone),
        );
    }
}

#[test]
fn rows_kept_together_name_their_keepers() {
    // errno is kept by all nine of the functions that set it.
    let sample = "testdata/bin/c-strtod.elf";
    let args = [
        sample,
        "retained",
        "_sbrk_r",
        "_isatty_r",
        "_kill_r",
        "_close_r",
        "_read_r",
        "_fstat_r",
        "_lseek_r",
        "_write_r",
        "_getpid_r",
    ];
    let output = stdout(&args);
    assert!(
        output.contains(
            "  errno, kept by _sbrk_r, _isatty_r, _kill_r and 6 more together\n\n\
             4 of these bytes go only when two or more of the 9 go.\n"
        ),
        "{output}"
    );
    let all = stdout(&[&args[..], &["--all"]].concat());
    assert!(
        all.contains(
            "  errno, kept by _sbrk_r, _isatty_r, _kill_r, _close_r, _read_r, _fstat_r, \
             _lseek_r, _write_r and _getpid_r together\n"
        ),
        "{all}"
    );
    // Four keepers are named, rather than three and "1 more".
    let output = stdout(&[
        "testdata/bin/c-cortex-m4.elf",
        "retained",
        "process_frame",
        "report",
        "app_tick",
        "on_button",
    ]);
    assert!(
        output.contains(
            "  log_msg, kept by process_frame, report, app_tick and on_button together\n"
        ),
        "{output}"
    );
}

#[test]
fn the_largest_rows_of_several_include_symbols() {
    // _impure_data and _impure_ptr are smaller than the 30 largest rows,
    // so they are summed up with each other at the end.
    let args = [
        "testdata/bin/c-strtod.elf",
        "retained",
        "_strtod_l",
        "_impure_data",
        "_impure_ptr",
    ];
    let output = stdout(&args);
    assert!(
        output.contains(
            "  __sf, kept by _strtod_l and _impure_ptr together\n      80        2 more\n\n\
             _impure_data adds nothing: other items named here keep it.\n"
        ),
        "{output}"
    );
    let all = stdout(&[&args[..], &["--all"]].concat());
    assert!(
        all.contains("\n      76    76  _impure_data\n       4     4  _impure_ptr\n"),
        "{all}"
    );
}

#[test]
fn retained_lists_every_symbol_it_cant_use() {
    // In the order given, each with the error it gets alone: an address
    // that no item holds, one that isn't an address, an import, a name
    // that no item has, with the name it may mean, and a pattern that
    // matches no item. A SYMBOL whose error says what another's says, as
    // for an address typed two ways or a name given twice, is listed once,
    // but the first line counts every one.
    assert_output(
        &stderr(&[
            SAMPLE,
            "retained",
            "crc32",
            "0x1",
            "0x01",
            "0xZZ",
            "__libc_start_main",
            "proces_frame",
            "nosuch_*",
            "frob",
            "frob",
        ]),
        "\
error: elfq can't use 8 of the 9 SYMBOLs:
  no function or data object contains address 0x1
  `0xZZ` is not a hexadecimal address: invalid digit found in string
  __libc_start_main is imported from a shared library, so its code isn't in this file
  no function or data object is named `proces_frame`; did you mean process_frame?
  no function or data object is named `nosuch_*`
  no function or data object is named `frob`
",
    );
    assert_output(
        &stderr(&[SAMPLE, "retained", "frob", "frob", "nosuch"]),
        "\
error: elfq can't use 3 of the 3 SYMBOLs:
  no function or data object is named `frob`
  no function or data object is named `nosuch`
",
    );
    // One that fails among good ones gets the error it gets alone, and so
    // does one given twice.
    assert_output(
        &stderr(&[SAMPLE, "retained", "crc32", "proces_frame", "report"]),
        "error: no function or data object is named `proces_frame`; did you mean process_frame?\n",
    );
    assert_output(
        &stderr(&[SAMPLE, "retained", "crc32", "0x1", "0x01"]),
        "error: no function or data object contains address 0x1\n",
    );
    // The list stops at 10 lines; 11 fit, as "and 1 more" would take a line too.
    let names: Vec<String> = (1..=12).map(|n| format!("frob{n}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let error = stderr(&[&[SAMPLE, "retained", "crc32"], &names[..]].concat());
    assert!(
        error.starts_with("error: elfq can't use 12 of the 13 SYMBOLs:\n")
            && error.ends_with("named `frob10`\n  and 2 more\n")
            && error.lines().count() == 12,
        "{error}"
    );
    let error = stderr(&[&[SAMPLE, "retained"], &names[..11]].concat());
    assert!(
        error.ends_with("named `frob11`\n") && error.lines().count() == 12,
        "{error}"
    );
    // `--all` lists every one.
    let error = stderr(&[&[SAMPLE, "retained", "crc32", "--all"], &names[..]].concat());
    assert!(
        error.ends_with("named `frob12`\n") && error.lines().count() == 13,
        "{error}"
    );
}

#[test]
fn retained_says_to_use_one_or_more_of_what_a_name_could_mean() {
    const CPP: &str = "testdata/bin/cpp-x86_64.elf";
    // It says to use one or more, unlike other commands, which take one
    // SYMBOL, and like `callers` and `callees`, which take a set too, it
    // offers the pattern that takes both.
    assert_output(
        &stderr(&[CPP, "retained", "log"]),
        "\
error: `log` could mean 2 functions; use one or more on the left instead, or 'log*' for all 2:
  'log(char const*)'   app::log(char const*)
  'log(unsigned int)'  app::log(unsigned int)
",
    );
    // In a list of errors, the same error, indented, with the full name
    // next to each address and how many more it doesn't list.
    assert_output(
        &stderr(&[CPP, "retained", "log", "log(unsigned int)", "frob"]),
        "\
error: elfq can't use 2 of the 3 SYMBOLs:
  `log` could mean 2 functions; use one or more on the left instead, or 'log*' for all 2:
    'log(char const*)'   app::log(char const*)
    'log(unsigned int)'  app::log(unsigned int)
  no function or data object is named `frob`
",
    );
    // 'fmt*' takes more than the 53, so it isn't offered.
    let error = stderr(&[
        "testdata/bin/rust-x86_64-nodebug.elf",
        "retained",
        "fmt",
        "nosuch",
    ]);
    assert!(
        error.starts_with(
            "error: elfq can't use 2 of the 2 SYMBOLs:\n  `fmt` could mean 53 functions; use one \
             or more on the left instead:\n"
        ) && error.contains("\n    0x3eeb0                   <&str as core::fmt::Display>::fmt\n")
            && error
                .ends_with("\n    and 43 more\n  no function or data object is named `nosuch`\n"),
        "{error}"
    );
    // `why` takes one item, so it offers no pattern.
    assert_output(
        &stderr(&[CPP, "why", "log"]),
        "\
error: `log` could mean 2 functions; use one on the left instead:
  'log(char const*)'   app::log(char const*)
  'log(unsigned int)'  app::log(unsigned int)
",
    );
}

#[test]
fn a_pattern_stands_for_every_item_it_matches() {
    // The rows of the three, under a first line that names the pattern,
    // and without a note on what adds nothing, since no SYMBOL was named.
    assert_output(
        &stdout(&[SAMPLE, "retained", "on_*"]),
        "\
Removing the 3 items named on_* would save 61 bytes:

RETAINED  OWN  ITEM
      31   19  on_button (through a pointer)
       8   ~8  .rodata@0x2004, kept by on_button
       4    4  button_presses, kept by on_button
      15   11  on_timer (through a pointer)
       4    4  timer_ticks, kept by on_timer
      15   11  on_data (through a pointer)
       4    4  data_events, kept by on_data

Shared with other code, so not counted: log_msg.
",
    );
    // One that matches one item answers as for that item, with its name
    // first.
    assert_output(
        &stdout(&[SAMPLE, "retained", "on_b*"]),
        "\
Removing on_button (the one item named on_b*) would save 31 bytes:

RETAINED  OWN  ITEM
      31   19  on_button (through a pointer)
       8   ~8  .rodata@0x2004, kept by on_button
       4    4  button_presses, kept by on_button

Shared with other code, so not counted: log_msg.
",
    );
    // A leading `::` matches only from the start of a name, and so leaves
    // out the 5 items that `alloc::*` matches only by the end of a longer
    // path, under std.
    const RUST: &str = "testdata/bin/rust-x86_64-nodebug.elf";
    let rooted = stdout(&[RUST, "retained", "::alloc::*"]);
    assert!(
        rooted.starts_with("Removing the 80 items named ::alloc::* would save 19225 bytes:\n"),
        "{rooted}"
    );
    assert!(!rooted.contains("only by the end"), "{rooted}");
    // The first line counts every item that a pattern matches, even one
    // whose end elfq doesn't know, which the answer leaves out as it does
    // when given by name: glibc's _init and _fini.
    let answer = stdout(&[SAMPLE, "retained", "_*"]);
    assert!(
        answer.starts_with("Removing the 8 items named _* would save 3823 bytes:\n")
            && answer.contains(
                "\n\nNot counted, since elfq doesn't know where they end: _init and _fini.\n"
            ),
        "{answer}"
    );
    assert_output(
        &stderr(&[SAMPLE, "retained", "_ini*"]),
        "error: elfq doesn't know where _init ends, so it can't read its code\n",
    );
    assert!(stdout(&[SAMPLE, "retained", "_ini*", "main"]).starts_with(
        "Removing _init (the one item named _ini*) and main would save 3597 bytes:\n"
    ));
    // So does the count of more parts than the first line names: the 8
    // items named _*, main, report and crc32.
    for args in [
        ["_*", "ma*", "pro*", "rep*"],
        ["_*", "main", "report", "crc32"],
    ] {
        let answer = stdout(&[&[SAMPLE, "retained"][..], &args].concat());
        assert!(
            answer.starts_with("Removing these 11 items would save 3823 bytes:\n"),
            "{args:?}: {answer}"
        );
    }
    // A shared library is refused, with the items named as the answer
    // would name them.
    assert_output(
        &stderr(&[LIBRARY_SAMPLE, "retained", "process_*", "rep*"]),
        "error: elfq can't yet tell what only the 4 items named process_* or rep* lead to in a \
         shared library\n",
    );
}

#[test]
fn patterns_mix_with_symbols_ranges_and_each_other() {
    // The first line names each part in the order given.
    let answer = stdout(&[TCM_SAMPLE, "retained", "leaf_*", "count_nodes", "0x0..0xe0"]);
    assert!(
        answer.starts_with(
            "Removing the 3 items named leaf_*, count_nodes and the 6 items in \
             0x00000000..0x000000e0 (.itcm) would save 1342 bytes:\n"
        ),
        "{answer}"
    );
    // An item that two patterns match, or a pattern and a name, counts
    // once; the patterns are named together.
    assert_output(
        &stdout(&[TCM_SAMPLE, "retained", "leaf_*", "*_a", "leaf_b"]),
        "\
Removing the 4 items named leaf_* or *_a and leaf_b would save 49 bytes:

RETAINED  OWN  ITEM
      25   25  frame_a
       8    8  leaf_c
       8    8  leaf_b
       8    8  leaf_a
",
    );
    // A pattern given twice is named once.
    assert_output(
        &stdout(&[TCM_SAMPLE, "retained", "leaf_*", "leaf_*"]),
        &stdout(&[TCM_SAMPLE, "retained", "leaf_*"]),
    );
    // The one item that patterns match is named, with every pattern that
    // matched it, and apart from the same item given by name.
    assert!(
        stdout(&[SAMPLE, "retained", "on_b*", "on_bu*", "on_but*", "on_butt*"]).starts_with(
            "Removing on_button (the one item named on_b*, on_bu*, on_but* or on_butt*) would \
             save 31 bytes:\n"
        )
    );
    assert!(
        stdout(&[SAMPLE, "retained", "on_b*", "on_button"]).starts_with(
            "Removing on_button (the one item named on_b*) and on_button would save 31 bytes:\n"
        )
    );
    // A pattern that matches nothing is an error, as a name that finds
    // nothing is, among the others or alone.
    assert_output(
        &stderr(&[SAMPLE, "retained", "on_*", "nosuch_*"]),
        "error: no function or data object is named `nosuch_*`\n",
    );
}

#[test]
fn retained_notes_the_matches_to_check() {
    // Items that a pattern matched only by the end of a longer path, when
    // it matched others by their names, as for a crate's name that is
    // also a module's in std.
    let answer = stdout(&[
        "testdata/bin/rust-x86_64-nodebug.elf",
        "retained",
        "alloc::*",
    ]);
    assert!(
        answer.starts_with("Removing the 85 items named alloc::* would save 19225 bytes:\n")
            && answer.contains(
                "\n\n5 of the 85 match only by the end of a longer path, under std; \
                 '::alloc::*' leaves them out.\n\n"
            ),
        "{answer}"
    );
    // Items that matched only through another of their names, as a soft
    // float routine and its EABI name.
    const SOFTFLOAT: &str = "testdata/bin/c-softfloat-nodebug.elf";
    assert_output(
        &stdout(&[SOFTFLOAT, "retained", "*df2"]),
        "\
Removing the 4 items named *df2 would save 204 bytes:

RETAINED  OWN  ITEM
     122  122  __cmpdf2

__aeabi_f2d matches only through another of its names, __extendsfdf2.

Nothing reaches __aeabi_f2d, __gedf2 and __ledf2: the program never uses them, or elfq can't see how. The total counts their own 82 bytes.
",
    );
    let answer = stdout(&[SOFTFLOAT, "retained", "*df2", "__aeabi_d*"]);
    assert!(
        answer.contains(
            "\n\nThese match only through other names: __adddf3 (as __aeabi_dadd) and \
             __aeabi_f2d (as __extendsfdf2).\n\n"
        ),
        "{answer}"
    );
    // The notes come when nothing reaches the items too.
    assert_output(
        &stdout(&[SOFTFLOAT, "retained", "__extendsf*", "__gedf*"]),
        "\
Nothing reaches __aeabi_f2d and __gedf2: the program never uses them, or elfq can't see how. Removing them would save their own 74 bytes.

__aeabi_f2d matches only through another of its names, __extendsfdf2.
",
    );
}

#[test]
fn a_name_with_a_star_in_it_is_a_name_first() {
    // A Rust name with a pointer type in it finds its item, as it does in
    // every command, though `*` would make it a pattern.
    assert!(
        stdout(&[
            "testdata/bin/rust-x86_64-nodebug.elf",
            "retained",
            "<*mut core::ffi::c_void as core::fmt::Debug>::fmt",
        ])
        .starts_with(
            "Removing <*mut core::ffi::c_void as core::fmt::Debug>::fmt would save 165 bytes:\n"
        )
    );
    // So does a C++ name's end, with a pointer in its parameters.
    assert!(
        stdout(&[
            "testdata/bin/cpp-x86_64.elf",
            "retained",
            "log(char const*)"
        ])
        .starts_with("Removing app::log(char const*) would save 80 bytes:\n")
    );
}

#[test]
fn retained_json_lists_how_each_pattern_matched() {
    // Each pattern's matches, in the order the patterns were given, with
    // how each matched; an item that two patterns matched has a row for
    // each, but is once in `symbols`.
    let answer = elfq_json(&[TCM_SAMPLE, "retained", "leaf_*", "*_a"]);
    assert_eq!(
        names(&answer["symbols"]),
        ["leaf_c", "leaf_b", "leaf_a", "frame_a"]
    );
    let matches: Vec<(&str, &str, &str, &str)> = (answer["matches"].as_array().unwrap().iter())
        .map(|found| {
            (
                found["pattern"].as_str().unwrap(),
                found["name"].as_str().unwrap(),
                found["matched_by"].as_str().unwrap(),
                found["matched_as"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        matches,
        [
            ("leaf_*", "leaf_c", "name", "leaf_c"),
            ("leaf_*", "leaf_b", "name", "leaf_b"),
            ("leaf_*", "leaf_a", "name", "leaf_a"),
            ("*_a", "frame_a", "name", "frame_a"),
            ("*_a", "leaf_a", "name", "leaf_a"),
        ]
    );
    let answer = elfq_json(&["testdata/bin/c-softfloat-nodebug.elf", "retained", "*df2"]);
    assert_eq!(
        answer["matches"][0],
        json!({
            "pattern": "*df2",
            "name": "__aeabi_f2d",
            "kind": "function",
            "section": ".text",
            "address": "0x8000344",
            "size": 66,
            "size_estimated": false,
            "matched_by": "alias",
            "matched_as": "__extendsfdf2"
        })
    );
    // Without a pattern, there is no `matches`.
    for args in [&["leaf_a"][..], &["leaf_a", "0x0..0xe0"]] {
        let answer = elfq_json(&[&[TCM_SAMPLE, "retained"], args].concat());
        assert_eq!(answer.get("matches"), None, "{answer}");
    }
    // An item whose end elfq doesn't know is in `left_out`, not in
    // `symbols`, but `matches` has its row.
    let answer = elfq_json(&[SAMPLE, "retained", "_*"]);
    assert_eq!(names(&answer["left_out"]), ["_init", "_fini"]);
    let mut matched: Vec<&str> = (answer["matches"].as_array().unwrap().iter())
        .map(|found| found["name"].as_str().unwrap())
        .collect();
    let mut given = [names(&answer["symbols"]), names(&answer["left_out"])].concat();
    matched.sort();
    given.sort();
    assert_eq!(matched, given);
}

#[test]
fn a_pattern_that_matches_nothing_gets_the_error_of_a_name() {
    assert_output(
        &stderr(&[SAMPLE, "retained", "nosuch_*"]),
        "error: no function or data object is named `nosuch_*`\n",
    );
    // When elfq can say more, as in a file without a symbol table, it
    // does.
    assert_output(
        &stderr(&["testdata/bin/c-x86_64-stripped.elf", "retained", "crc*"]),
        "error: no function or data object is named `crc*`; the file has no symbol table, so \
         elfq names its functions and data by address, like fn_0x10a0 and .rodata@0x2004\n",
    );
    // A mistyped C++ name with a space before a pointer's `*` is a
    // pattern, which matches nothing, but gets the names like it.
    assert_output(
        &stderr(&[
            "testdata/bin/cpp-x86_64.elf",
            "retained",
            "log(char const *)",
        ]),
        "error: no function or data object is named `log(char const *)`; did you mean \
         'log(char const*)'?\n",
    );
    // Only `*` makes a pattern.
    assert_output(
        &stderr(&[SAMPLE, "retained", "crc?2"]),
        "error: no function or data object is named `crc?2`; did you mean crc32?\n",
    );
}

#[test]
fn a_pattern_that_matches_nothing_gets_one_that_matches() {
    // Each error names what to type, with how many it matches.
    let error = |file: &str, pattern: &str| stderr(&[file, "retained", pattern]);
    let named = |pattern: &str, may_mean: &str| {
        format!(
            "error: no function or data object is named `{pattern}`; did you mean {may_mean}?\n"
        )
    };
    // In the file's case, as only case keeps it from matching.
    assert_output(
        &error(SAMPLE, "ON_*"),
        &named("ON_*", "'on_*' (3 functions)"),
    );
    assert_output(
        &error(SAMPLE, "Process_Frame*"),
        &named("Process_Frame*", "'process_frame*' (1 function)"),
    );
    // Without the quotes that a script left on.
    assert_output(
        &error(SAMPLE, "'on_*'"),
        &named("'on_*'", "on_* (3 functions), without the quotes"),
    );
    // Without a leading `::`, which leaves out the ends of longer paths.
    const CPP: &str = "testdata/bin/cpp-x86_64.elf";
    assert_output(
        &error(CPP, "::Gain::*"),
        &named("::Gain::*", "'Gain::*' (4 items)"),
    );
    // With `*` in front.
    assert_output(
        &error(SAMPLE, "n_b*"),
        &named("n_b*", "'*n_b*' (1 function)"),
    );
    // Else, the imports it matches, which a pattern doesn't pick, since
    // their code isn't in the file, though why and callers take their
    // names as SYMBOL.
    assert_output(
        &error(SAMPLE, "_ITM_*"),
        &named(
            "_ITM_*",
            "_ITM_deregisterTMCloneTable, _ITM_registerTMCloneTable",
        ),
    );
    // Else, the names like it, as for a name that finds nothing, which
    // a_pattern_that_matches_nothing_gets_the_error_of_a_name shows; here
    // there are none.
    assert_output(
        &error(SAMPLE, "::n_b*"),
        "error: no function or data object is named `::n_b*`\n",
    );
}

/// The functions in the Cortex-M7 sample's ITCM, 0x0..0xe0, by name, in
/// address order.
const ITCM_FUNCTIONS: [&str; 6] = [
    "SysTick_Handler",
    "crc32",
    "filter_tap",
    "apply_filter",
    "__app_tick_veneer",
    "__memcpy_veneer",
];

#[test]
fn a_range_stands_for_every_item_in_it() {
    // The rows of the six functions by name, under a first line that names
    // the range, and without the note on the three that add nothing, since
    // no SYMBOL was named.
    let named = |extra: &[&str]| {
        let args = [&[TCM_SAMPLE, "retained"], &ITCM_FUNCTIONS[..], extra].concat();
        stdout(&args)
            .replace(
                "Removing these 6 items",
                "Removing the 6 items in 0x00000000..0x000000e0 (.itcm)",
            )
            .replace(
                "\n\nfilter_tap, __app_tick_veneer and __memcpy_veneer add nothing: other items \
                 named here keep them.",
                "",
            )
    };
    for extra in [&[][..], &["--tree"], &["--range", "0x20000000..0x20000450"]] {
        for range in ["0x0..0xe0", "0x0+224"] {
            let args = [&[TCM_SAMPLE, "retained", range], extra].concat();
            assert_output(&stdout(&args), &named(extra));
        }
    }
    assert!(named(&[]).starts_with(
        "Removing the 6 items in 0x00000000..0x000000e0 (.itcm) would save 1284 bytes:\n"
    ));
    // In JSON, the items are SYMBOLs, as if given so.
    assert_eq!(
        elfq_json(&[TCM_SAMPLE, "retained", "0x0..0xe0"]),
        elfq_json(&[&[TCM_SAMPLE, "retained"], &ITCM_FUNCTIONS[..]].concat())
    );
    // A range that holds one item answers about it alone, under its name,
    // with the range as a note.
    assert_output(
        &stdout(&[TCM_SAMPLE, "retained", "0x0+16"]),
        &stdout(&[TCM_SAMPLE, "retained", "SysTick_Handler"]).replacen(
            "Removing SysTick_Handler",
            "Removing SysTick_Handler (the one item in 0x00000000..0x00000010)",
            1,
        ),
    );
    assert_output(
        &stdout(&[TCM_SAMPLE, "retained", "0x300009c0..0x30000de8"]),
        "Removing .rodata@0x30000de0 (the one item in 0x300009c0..0x30000de8) would save only \
         its own 8 bytes.\n",
    );
    // The sections are named by where they run, as callers names them: in
    // flash, they are the code and data stored there.
    assert!(
        stdout(&[TCM_SAMPLE, "retained", "0x30000000..0x30000edc"]).starts_with(
            "Removing the 44 items in 0x30000000..0x30000edc (.isr_vector, .text, .rodata and \
             .ARM.exidx) would save 7807 bytes:\n"
        )
    );
    // Up to 10, then how many more, or with --all, every one.
    let sections = ".interp, .note.gnu.property, .note.gnu.build-id, .note.ABI-tag, .gnu.hash, \
                    .dynsym, .dynstr, .gnu.version, .gnu.version_r, .rela.dyn";
    let lead = |sections: &str| {
        format!(
            "Removing the 66 items in 0x00000000..0x00005000 ({sections}) would save 4021 bytes:\n"
        )
    };
    assert!(
        stdout(&[SAMPLE, "retained", "0x0..0x5000"])
            .starts_with(&lead(&format!("{sections} and 17 more")))
    );
    assert!(
        stdout(&[SAMPLE, "retained", "0x0..0x5000", "--all"]).starts_with(&lead(&format!(
            "{sections}, .rela.plt, .init, .plt, .plt.got, .plt.sec, .text, .fini, .rodata, \
             .eh_frame_hdr, .eh_frame, .init_array, .fini_array, .data.rel.ro, .dynamic, .got, \
             .data and .bss"
        )))
    );
}

#[test]
fn ranges_mix_with_symbols_and_each_other() {
    // An item that two arguments find counts once: crc32, at 0x10..0x40,
    // is in both ranges. With a SYMBOL among them, the first line counts
    // the items; with only ranges, it names every range.
    let mixed = stdout(&[TCM_SAMPLE, "retained", "0x0..0x40", "crc32", "0x20..0x80"]);
    assert!(
        mixed.starts_with("Removing these 4 items would save 1284 bytes:\n"),
        "{mixed}"
    );
    assert_output(
        &stdout(&[TCM_SAMPLE, "retained", "0x0..0x40", "0x20..0x80"]),
        &mixed.replacen(
            "Removing these 4 items",
            "Removing the 4 items in 0x00000000..0x00000040 (.itcm) and 0x00000020..0x00000080 \
             (.itcm)",
            1,
        ),
    );
    // A range given twice is named once.
    assert_output(
        &stdout(&[TCM_SAMPLE, "retained", "0x0..0xe0", "0x0..0xe0"]),
        &stdout(&[TCM_SAMPLE, "retained", "0x0..0xe0"]),
    );
    // The note on what adds nothing names only the items that a SYMBOL
    // names, the first or a later argument, and not filter_tap when only a
    // range holds it.
    let add_nothing = |args: &[&str]| -> Vec<String> {
        let answer = stdout(&[&[TCM_SAMPLE, "retained"], args].concat());
        (answer.lines())
            .filter(|line| line.contains(" nothing: other items named here keep "))
            .map(str::to_string)
            .collect()
    };
    for (args, name) in [
        (&["crc_table", "0x0..0xe0"][..], "crc_table"),
        (&["0x0..0xe0", "filter_tap"], "filter_tap"),
        (&["crc32", "0x30000000..0x30000edc"], "crc32"),
    ] {
        assert_eq!(
            add_nothing(args),
            [format!(
                "{name} adds nothing: other items named here keep it."
            )]
        );
    }
    assert_eq!(
        add_nothing(&["0x0..0x40", "crc32", "0x20..0x80"]),
        Vec::<String>::new()
    );
    // Nor does the note that one item keeps the rest come with only ranges.
    let keeps = "\n\nSysTick_Handler keeps __app_tick_veneer, so removing it alone saves the same.";
    let named = stdout(&[
        TCM_SAMPLE,
        "retained",
        "SysTick_Handler",
        "__app_tick_veneer",
    ]);
    assert!(named.contains(keeps), "{named}");
    assert_output(
        &stdout(&[TCM_SAMPLE, "retained", "0x0+16", "0xd0+8"]),
        &named.replace(keeps, "").replacen(
            "Removing SysTick_Handler and __app_tick_veneer",
            "Removing the 2 items in 0x00000000..0x00000010 (.itcm) and 0x000000d0..0x000000d8 \
             (.itcm)",
            1,
        ),
    );
    let answer = elfq_json(&[TCM_SAMPLE, "retained", "0x0..0x40", "crc32", "0x20..0x80"]);
    assert_eq!(
        names(&answer["symbols"]),
        ["SysTick_Handler", "crc32", "filter_tap", "apply_filter"]
    );
}

#[test]
fn a_range_holds_items_that_retained_can_use() {
    // An empty range gets the error that callers gives it, alone and in a
    // list, which then counts arguments, not SYMBOLs.
    let empty = "the file has nothing in 0x00000000..0x00000010, only in 0x00000318..0x00004088";
    for command in ["retained", "callers"] {
        assert_output(
            &stderr(&[SAMPLE, command, "0x0..0x10"]),
            &format!("error: {empty}\n"),
        );
    }
    assert_output(
        &stderr(&[
            SAMPLE,
            "retained",
            "crc32",
            "0x0..0x10",
            "0x10..0x0",
            "frob",
        ]),
        &format!(
            "error: elfq can't use 3 of the 4 arguments:\n  {empty}\n  `0x10..0x0` isn't a \
             range: the range must end after it starts\n  no function or data object is named \
             `frob`\n"
        ),
    );
    // A range typed with `-` gets callers' hint, alone and in the list,
    // which counts it as an argument too.
    let dash = "`0x0-0x10` isn't a range: did you mean 0x0..0x10?";
    assert_output(
        &stderr(&[SAMPLE, "retained", "0x0-0x10", "crc32"]),
        &format!("error: {dash}\n"),
    );
    assert_output(
        &stderr(&[SAMPLE, "retained", "0x0-0x10", "frob"]),
        &format!(
            "error: elfq can't use 2 of the 2 arguments:\n  {dash}\n  no function or data object \
             is named `frob`\n"
        ),
    );
    // A range that holds only a load image gets callers' error, which says
    // where the program uses what is stored there.
    assert_output(
        &stderr(&[TCM_SAMPLE, "retained", "0x300009c0..0x30000de0"]),
        "error: elfq found no functions or data in 0x300009c0..0x30000de0 (the load image of \
         .data); the program uses .data at 0x20000000..0x20000420\n",
    );
    // A range that holds only _init gets _init's error; with more in it,
    // _init is left out.
    assert_output(
        &stderr(&[SAMPLE, "retained", "0x1000..0x1010"]),
        "error: elfq doesn't know where _init ends, so it can't read its code\n",
    );
    let more = stdout(&[SAMPLE, "retained", "0x1000..0x1100"]);
    assert!(
        more.starts_with(
            "Removing the 6 items in 0x00001000..0x00001100 (.init, .plt, .plt.got, .plt.sec and \
             .text) would save 3677 bytes:\n"
        ) && more.contains("\n\nNot counted, since elfq doesn't know where it ends: _init.\n"),
        "{more}"
    );
    assert_eq!(
        names(&elfq_json(&[SAMPLE, "retained", "0x1000..0x1100"])["left_out"]),
        ["_init"]
    );
}

#[test]
fn a_large_range_is_the_items_in_it() {
    // retained of 64 KiB of a Rust program's code answers as retained of
    // the addresses, from top, of every item with a byte in it.
    const RUST: &str = "testdata/bin/rust-x86_64-nodebug.elf";
    let (start, end) = (0x20000, 0x30000);
    let top = elfq_json(&[RUST, "top"]);
    let mut items: Vec<(u64, String)> = (top["items"].as_array().unwrap().iter())
        .chain(top["unreachable"].as_array().unwrap())
        .map(|item| {
            let address = item["address"].as_str().unwrap();
            let address = u64::from_str_radix(&address[2..], 16).unwrap();
            let size = item["size"].as_u64().unwrap_or(0).max(1);
            (address, size, item["address"].as_str().unwrap().to_string())
        })
        .filter(|&(address, size, _)| address < end && address + size > start)
        .map(|(address, _, text)| (address, text))
        .collect();
    items.sort();
    items.dedup();
    assert!(items.len() > 100, "{}", items.len());
    let addresses: Vec<&str> = items.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(
        elfq_json(&[RUST, "retained", "0x20000+64K"]),
        elfq_json(&[&[RUST, "retained"], &addresses[..]].concat())
    );
}

#[test]
fn retained_leaves_out_a_function_whose_end_elfq_doesnt_know() {
    // elfq can't read the code of glibc's _init, which has no size, so it
    // answers about the others and says which it left out, before the
    // notes that every answer about removing items has.
    assert_output(
        &stdout(&[SAMPLE, "retained", "crc32", "_init"]),
        "\
Removing crc32 would save 1083 bytes:

RETAINED   OWN  ITEM
    1083    59  crc32
    1024  1024  crc_table, kept by crc32

Not counted, since elfq doesn't know where it ends: _init.
",
    );
    assert_output(
        &stdout(&[SAMPLE, "retained", "report", "_init"]),
        "\
Removing report would save 705 bytes:

RETAINED  OWN  ITEM
     705  370  report
     162  162  format_u32, kept by report
      80   16  root, kept by report
      48   16  inner, kept by root
      16   16  leaf_c, kept by inner
      16   16  leaf_b, kept by inner
      16   16  leaf_a, kept by root
      46   46  count_nodes, kept by report
      10  ~10  .rodata@0x2031, kept by report
      10  ~10  .rodata@0x203b, kept by report
       9   ~9  .rodata@0x2045, kept by report
       8   ~8  .rodata@0x204e, kept by report
       6   ~6  .rodata@0x202b, kept by report
       4   ~4  .rodata@0x2027, kept by report

Not counted, since elfq doesn't know where it ends: _init.

Shared with other code, so not counted: __stack_chk_fail@plt, config_sum, frames_bad, frames_ok, log_msg, stats.
",
    );
    // With several SYMBOLs, after the notes that only an answer about
    // several has. A function given twice, or by its name and its address,
    // is left out once.
    let several = stdout(&[
        SAMPLE,
        "retained",
        "process_frame",
        "report",
        "_init",
        "0x1ad4",
        "_fini",
        "0x1000",
    ]);
    assert!(
        several.ends_with(
            "\n\n56 of these bytes go only when both go.\n\n\
             Not counted, since elfq doesn't know where they end: _init and _fini.\n\n\
             Shared with other code, so not counted: config_sum, dispatch_event, log_msg.\n"
        ),
        "{several}"
    );
    assert_output(
        &stdout(&[SAMPLE, "retained", "debug_dump", "_IO_stdin_used", "_init", "_fini"]),
        "\
Nothing reaches debug_dump and _IO_stdin_used: the program never uses them, or elfq can't see how. Removing them would save their own 33 bytes.

Not counted, since elfq doesn't know where they end: _init and _fini.
",
    );
    let answer = elfq_json(&[SAMPLE, "retained", "crc32", "_init", "0x1000"]);
    assert_eq!(
        answer["left_out"],
        json!([{
            "name": "_init",
            "kind": "function",
            "section": ".init",
            "address": "0x1000",
            "size": null,
            "size_estimated": false,
            "aliases": []
        }])
    );
    assert_eq!(answer["symbols"].as_array().unwrap().len(), 1);
    assert_eq!(answer["retained"], 1083);
    // With nothing else to answer about, the error names every one.
    for args in [&["_init"][..], &["_init", "0x1000"]] {
        assert_output(
            &stderr(&[&[SAMPLE, "retained"], args].concat()),
            "error: elfq doesn't know where _init ends, so it can't read its code\n",
        );
    }
    assert_output(
        &stderr(&[SAMPLE, "retained", "_init", "_fini", "0x1000"]),
        "error: elfq doesn't know where _init and _fini end, so it can't read their code\n",
    );
}

#[test]
fn retained_refuses_a_shared_library_and_asks_for_a_symbol() {
    assert_output(
        &stderr(&[LIBRARY_SAMPLE, "retained", "process_frame", "report"]),
        "error: elfq can't yet tell what only process_frame and report lead to in a shared \
         library\n",
    );
    // Ranges given alone are named as the answer would name them.
    assert_output(
        &stderr(&[LIBRARY_SAMPLE, "retained", "0x1000..0x2000"]),
        "error: elfq can't yet tell what only the 36 items in 0x00001000..0x00002000 (.init, \
         .plt, .plt.got, .plt.sec, .text and .fini) lead to in a shared library\n",
    );
    assert_output(
        &stderr(&[
            LIBRARY_SAMPLE,
            "retained",
            "0x1000..0x1100",
            "0x1200..0x1300",
        ]),
        "error: elfq can't yet tell what only the 8 items in 0x00001000..0x00001100 (.init, \
         .plt, .plt.got and .plt.sec) and 0x00001200..0x00001300 (.text) lead to in a shared \
         library\n",
    );
    assert_output(
        &stderr(&[LIBRARY_SAMPLE, "retained", "0x1020+4"]),
        "error: elfq can't yet tell what only .plt (the one item in 0x00001020..0x00001024) \
         leads to in a shared library\n",
    );
    // After saying which SYMBOLs it can't use.
    assert_output(
        &stderr(&[LIBRARY_SAMPLE, "retained", "frob", "process_frame"]),
        "error: no function or data object is named `frob`\n",
    );
    let error = stderr(&[SAMPLE, "retained"]);
    assert!(
        error.starts_with(
            "error: the following required arguments were not provided:\n  <SYMBOL|PATTERN|RANGE>...\n"
        ),
        "{error}"
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
    // crc32 has none of its own bytes in the range, but keeps some there.
    assert_eq!(names(&retained["items"]), ["crc32", "crc_table"]);
    assert_eq!(retained["items"][0]["own"], 0);

    let retained = elfq_json(&[TCM_SAMPLE, "retained", "process_frame", "--range", "0+224"]);
    assert_eq!(retained["retained"], 200);
    assert_eq!(
        names(&retained["items"]),
        [
            "process_frame",
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
    assert_eq!(retained["items"][1]["name"], "crc_table");
    assert_eq!(retained["items"][1]["own"], 2048);
    assert_eq!(retained["items"][1]["load_image"], 1024);
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
    let symbol = &retained["symbols"][0];
    assert_eq!(symbol["name"], "process_frame");
    assert_eq!(symbol["reachable"], true);
    assert_eq!(symbol["kept_by_others"], false);
    assert_eq!(retained["retained"], 2068);
    assert_eq!(retained["kept_together"], 0);
    // SYMBOL has the first row, and every row counts its own bytes once.
    assert_eq!(symbol["row"], 0);
    assert_eq!(retained["items"][0]["name"], "process_frame");
    assert_eq!(retained["items"][0]["parent"], Value::Null);
    assert_eq!(own_bytes(&retained["items"]), 2068);
    let crc32 = &retained["items"][1];
    assert_eq!(crc32["id"], 1);
    assert_eq!(crc32["name"], "crc32");
    assert_eq!(crc32["own"], 59);
    assert_eq!(crc32["retained"], 1083);
    assert_eq!(crc32["through_pointer"], false);
    assert_eq!(crc32["kept_together_by"], Value::Null);
    // process_frame keeps crc32, and crc32 keeps crc_table.
    assert_eq!(crc32["parent"], 0);
    let crc_table = &retained["items"][2];
    assert_eq!(crc_table["name"], "crc_table");
    assert_eq!(crc_table["parent"], 1);
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
    // The rows hold process_frame and every item that only it leads to, as
    // the text's rows do, and in their order.
    assert_eq!(
        names(&retained["items"]),
        text_rows(&[SAMPLE, "retained", "process_frame"])
    );
    assert_eq!(
        names(&retained["items"]),
        [
            "process_frame",
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
    let literal = &retained["items"][7];
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
    assert_eq!(unreachable["symbols"][0]["reachable"], false);
    assert_eq!(unreachable["symbols"][0]["row"], Value::Null);
    assert_eq!(unreachable["retained"], 29);
    assert_eq!(unreachable["items"], json!([]));
    assert_eq!(unreachable["shares"], json!([]));
    // A SYMBOL that nothing reaches still counts its own calls through
    // pointers: three in newlib's __sfvwrite_r here.
    let unreachable = elfq_json(&["testdata/bin/c-strtod.elf", "retained", "__sfvwrite_r"]);
    assert_eq!(unreachable["symbols"][0]["reachable"], false);
    assert_eq!(unreachable["retained"], 768);
    assert_eq!(unreachable["shares"], json!([]));
    assert_eq!(unreachable["indirect_calls"], 3);
}

#[test]
fn deep_trees_are_whole() {
    // `--tree` continues a tree deeper than 9 levels below it; JSON has all
    // of it, each row under the one before.
    let answer = elfq_json(&[DEEP_SAMPLE, "retained", "chain1"]);
    let items = answer["items"].as_array().unwrap();
    let expected: Vec<String> = (1..=20).map(|n| format!("chain{n}")).collect();
    assert_eq!(names(&answer["items"]), expected);
    assert_eq!(items[0]["parent"], Value::Null);
    for (id, item) in items.iter().enumerate().skip(1) {
        assert_eq!(item["parent"], id - 1);
    }
}

#[test]
fn retained_of_several_has_the_rows_of_the_text_as_json() {
    let args = [SAMPLE, "retained", "process_frame", "report"];
    let answer = elfq_json(&args);
    assert_eq!(answer["own"], 532 + 370);
    assert_eq!(answer["retained"], 2829);
    assert_eq!(answer["kept_together"], 56);
    assert_adds_up(&answer);
    // Each SYMBOL, in the order given, has its row at the top.
    assert_eq!(names(&answer["symbols"]), ["process_frame", "report"]);
    assert_eq!(rows_of(&answer), [Some(0), Some(9)]);
    for symbol in answer["symbols"].as_array().unwrap() {
        assert_eq!(symbol["kept_by_others"], false);
        let row = &answer["items"][symbol["row"].as_u64().unwrap() as usize];
        assert_eq!(row["parent"], Value::Null);
    }
    // The rows are the text's, and those that the two keep only together
    // name them by their addresses, in the order given.
    assert_eq!(names(&answer["items"]), text_rows(&args));
    let items = answer["items"].as_array().unwrap();
    let together: Vec<&Value> = (items.iter())
        .filter(|row| !row["kept_together_by"].is_null())
        .collect();
    assert_eq!(
        names(&json!(together)),
        ["stats", "__stack_chk_fail@plt", "frames_bad", "frames_ok"]
    );
    for row in together {
        assert_eq!(row["kept_together_by"], json!(["0x1700", "0x1920"]));
        assert_eq!(row["parent"], Value::Null);
    }
    // errno names all nine of the functions that set it.
    let errno_setters = [
        "_sbrk_r",
        "_isatty_r",
        "_kill_r",
        "_close_r",
        "_read_r",
        "_fstat_r",
        "_lseek_r",
        "_write_r",
        "_getpid_r",
    ];
    let args = [
        &["testdata/bin/c-strtod.elf", "retained"][..],
        &errno_setters,
    ]
    .concat();
    let answer = elfq_json(&args);
    assert_adds_up(&answer);
    assert_eq!(answer["kept_together"], 4);
    let errno = row_named(&answer, "errno");
    let addresses: Vec<&Value> = (answer["symbols"].as_array().unwrap().iter())
        .map(|symbol| &symbol["address"])
        .collect();
    assert_eq!(errno["kept_together_by"], json!(addresses));
}

#[test]
fn retained_says_of_each_symbol_what_it_adds() {
    // Nothing reaches debug_dump, so it has no row, and its own bytes count.
    let answer = elfq_json(&[SAMPLE, "retained", "debug_dump", "process_frame"]);
    assert_eq!(answer["retained"], 2097);
    assert_eq!(answer["kept_together"], 0);
    assert_adds_up(&answer);
    let debug_dump = &answer["symbols"][0];
    assert_eq!(debug_dump["reachable"], false);
    assert_eq!(debug_dump["own"], 29);
    assert_eq!(debug_dump["row"], Value::Null);
    assert_eq!(debug_dump["kept_by_others"], false);
    assert_eq!(rows_of(&answer), [None, Some(0)]);
    // process_frame keeps crc32, so crc32 adds nothing, and its row hangs
    // under process_frame's.
    let answer = elfq_json(&[SAMPLE, "retained", "process_frame", "crc32"]);
    assert_eq!(answer["retained"], 2068);
    assert_eq!(answer["kept_together"], 0);
    assert_adds_up(&answer);
    let crc32 = &answer["symbols"][1];
    assert_eq!(crc32["kept_by_others"], true);
    assert_eq!(
        answer["items"][crc32["row"].as_u64().unwrap() as usize]["parent"],
        0
    );
    // process_frame and report keep stats only together, so stats adds
    // nothing. Its row is at the top, but as a SYMBOL's own it names no
    // keepers, where frames_ok's, which they keep together too, does.
    // Removing stats alone saves its 32 bytes, so those don't go only
    // when several go.
    let args = [SAMPLE, "retained", "process_frame", "report", "stats"];
    let answer = elfq_json(&args);
    assert_eq!(answer["retained"], 2829);
    assert_eq!(answer["kept_together"], 56 - 32);
    assert_adds_up(&answer);
    assert_eq!(names(&answer["items"]), text_rows(&args));
    let stats = &answer["symbols"][2];
    assert_eq!(stats["kept_by_others"], true);
    let row = &answer["items"][stats["row"].as_u64().unwrap() as usize];
    assert_eq!(row["parent"], Value::Null);
    assert_eq!(row["kept_together_by"], Value::Null);
    let frames_ok = row_named(&answer, "frames_ok");
    assert_eq!(frames_ok["kept_together_by"], json!(["0x1700", "0x1920"]));
    // The two panic paths keep fmt only together, and fmt keeps
    // pad_integral, so pad_integral adds nothing, and its row hangs under
    // fmt's, which names the two.
    let args = [
        "testdata/bin/rust-cortex-m4.elf",
        "retained",
        "panic_bounds_check",
        "slice_index_fail",
        "pad_integral",
    ];
    let answer = elfq_json(&args);
    assert_eq!(answer["retained"], 2813);
    assert_adds_up(&answer);
    assert_eq!(names(&answer["items"]), text_rows(&args));
    let pad_integral = &answer["symbols"][2];
    assert_eq!(pad_integral["kept_by_others"], true);
    let row = &answer["items"][pad_integral["row"].as_u64().unwrap() as usize];
    assert_eq!(row["kept_together_by"], Value::Null);
    let fmt = &answer["items"][row["parent"].as_u64().unwrap() as usize];
    assert_eq!(fmt["name"], "<u32 as core::fmt::Display>::fmt");
    assert_eq!(fmt["through_pointer"], true);
    assert_eq!(fmt["parent"], Value::Null);
    let panic_paths = [
        &answer["symbols"][0]["address"],
        &answer["symbols"][1]["address"],
    ];
    assert_eq!(fmt["kept_together_by"], json!(panic_paths));
    // What goes uses log_msg, which other code uses too, and calls through
    // pointers.
    assert_eq!(names(&answer["shares"]), ["elfq_sample::log_msg"]);
    assert_eq!(answer["indirect_calls"], 8);
    // apply_filter keeps none of the range's bytes, so it has no row.
    let answer = elfq_json(&[
        TCM_SAMPLE,
        "retained",
        "crc32",
        "apply_filter",
        "--range",
        "0x20000000..0x20000450",
    ]);
    assert_eq!(answer["retained"], 1024);
    assert_adds_up(&answer);
    assert_eq!(answer["symbols"][1]["reachable"], true);
    assert_eq!(answer["symbols"][1]["own"], 0);
    assert_eq!(rows_of(&answer), [Some(0), None]);
    // A SYMBOL given twice is one, and one SYMBOL keeps nothing together.
    // 0x1280 is where crc32 starts.
    let answer = elfq_json(&[SAMPLE, "retained", "crc32", "0x1280"]);
    assert_eq!(answer, elfq_json(&[SAMPLE, "retained", "crc32"]));
    assert_eq!(names(&answer["symbols"]), ["crc32"]);
    assert_eq!(names(&answer["items"]), ["crc32", "crc_table"]);
    assert_eq!(rows_of(&answer), [Some(0)]);
    assert_eq!(answer["kept_together"], 0);
}

/// The row of the item named `name` in `retained`'s JSON.
#[track_caller]
fn row_named<'a>(answer: &'a Value, name: &str) -> &'a Value {
    (answer["items"].as_array().unwrap().iter())
        .find(|row| row["name"] == name)
        .unwrap()
}

/// The `row` of each SYMBOL in `retained`'s JSON, in the order given.
fn rows_of(answer: &Value) -> Vec<Option<u64>> {
    (answer["symbols"].as_array().unwrap().iter())
        .map(|symbol| symbol["row"].as_u64())
        .collect()
}

/// Checks that what `retained`'s JSON says goes adds up: the rows at the
/// top of its tree and the own bytes of the SYMBOLs that nothing reaches
/// make `retained`, and the `row` of each SYMBOL is its own.
fn assert_adds_up(answer: &Value) {
    let items = answer["items"].as_array().unwrap();
    let tops: u64 = (items.iter())
        .filter(|row| row["parent"].is_null())
        .map(|row| row["retained"].as_u64().unwrap())
        .sum();
    let symbols = answer["symbols"].as_array().unwrap();
    let unreached: u64 = (symbols.iter())
        .filter(|symbol| symbol["reachable"] == false)
        .map(|symbol| symbol["own"].as_u64().unwrap())
        .sum();
    assert_eq!(answer["retained"], tops + unreached, "{answer}");
    for symbol in symbols {
        if let Some(row) = symbol["row"].as_u64() {
            assert_eq!(items[row as usize]["address"], symbol["address"]);
        }
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
