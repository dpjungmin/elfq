//! `reach`: what SYMBOL runs and uses, by block of memory.
//!
//! The tests check it as rows, as a tree and with `--to`, as text and as
//! JSON, for a pattern that matches one item, also in files made by
//! changing a sample's bytes, and where the program starts when there is
//! no SYMBOL.
//!
//! The checks after those run it in this process, on every item of the
//! Cortex-M samples against `retained`, `callees` and the tree that
//! `--tree` draws, with `--to` against `reach` without it and
//! `callees --to`, and on every function of each stripped one against
//! its twin with symbols; and without SYMBOL, on each start of every
//! sample against `reach START`.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Value, json};

use elfq::binary::{AddressRange, Binary, ItemKind};
use elfq::commands::Outcome;

use crate::support::elf::{ElfBytes, SectionField, for_arm_version};
use crate::support::{
    DEEP_SAMPLE, Exit, FAR_SAMPLES, LIBRARY_SAMPLE, SAMPLE, TCM_ENTRY_MAIN_SAMPLE, TCM_SAMPLE,
    assert_output, crossing, elfq_json, load_sample, names, run, run_in_process, sample_path,
    samples, stderr, stdout,
};

#[test]
fn reach_lists_what_a_function_runs_and_uses_by_block() {
    // Through a veneer into flash, and data in two blocks of RAM and in
    // flash.
    assert_output(
        &stdout(&[TCM_SAMPLE, "reach", "SysTick_Handler"]),
        "\
SysTick_Handler and the 12 items it reaches take 446 bytes of code and 4128 bytes of data; elfq can't follow 1 jump:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000e0    24     0  .itcm
0x20000000..0x20000450     0    24  .bss
0x20240000..0x20241000     0  4096  .ocram_bss
0x30000000..0x30000edc   422     8  .text .rodata

ADDRESS     SIZE  HOW                   ITEM
0x0           16                        SysTick_Handler
0xd0           8  tail call             __app_tick_veneer, used by SysTick_Handler
0x20000438    12  uses                  handlers, used by dispatch_event
0x20000444     4  uses                  systick_count, used by SysTick_Handler
0x2000044c     4  uses                  tx_head, used by platform_write
0x20000448     4  uses                  tx_tail, used by platform_write
0x20240000  4096  uses                  tx_buffer, used by platform_write
0x30000700   216  call                  strlen, used by log_msg
0x30000668    96  tail call             platform_write, used by log_msg
0x300002e4    60  tail call via veneer  app_tick, used by SysTick_Handler
0x30000288    26  call                  log_msg, used by app_tick
0x300002cc    24  tail call             dispatch_event, used by app_tick
0x30000de8    ~8  uses                  .rodata@0x30000de8, used by app_tick

dispatch_event also jumps through a pointer, at 0x300002da, which elfq can't follow, so SysTick_Handler may reach more.
",
    );
    // Code in ITCM that flash calls through veneers.
    assert_output(
        &stdout(&[TCM_SAMPLE, "reach", "process_frame"]),
        "\
process_frame and the 23 items it reaches take 1302 bytes of code and 5236 bytes of data; elfq can't follow 1 jump:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000e0   200     0  .itcm
0x20000000..0x20000450     0  1084  .data .bss
0x20240000..0x20241000     0  4096  .ocram_bss
0x30000000..0x30000edc  1102    56  .text .rodata

ADDRESS     SIZE  HOW              ITEM
0x30000320   376                   process_frame
0x74          92  call via veneer  apply_filter, used by process_frame
0x40          52  call             filter_tap, used by apply_filter
0x10          48  call via veneer  crc32, used by process_frame
0xd8           8  call             __memcpy_veneer, used by apply_filter
0x20000000  1024  uses             crc_table, used by crc32
0x20000400    32  uses             stats, used by process_frame
0x20000438    12  uses             handlers, used by dispatch_event
0x20000424     4  uses             frames_bad, used by process_frame
0x20000428     4  uses             frames_ok, used by process_frame
0x2000044c     4  uses             tx_head, used by platform_write
0x20000448     4  uses             tx_tail, used by platform_write
0x20240000  4096  uses             tx_buffer, used by platform_write
0x3000087c   308  call via veneer  memcpy, used by apply_filter
0x30000700   216  call             strlen, used by log_msg
0x30000668    96  tail call        platform_write, used by log_msg
0x30000160    40  call             decode_header, used by process_frame
0x30000288    26  call             log_msg, used by process_frame
0x300002cc    24  call             dispatch_event, used by process_frame
0x300009b8     8  call             __apply_filter_veneer, used by process_frame
0x300009b0     8  call             __crc32_veneer, used by process_frame
0x30000eb4    32  uses             fir_coeffs, used by filter_tap
0x30000df0   ~12  uses             .rodata@0x30000df0, used by process_frame
0x30000dfc   ~12  uses             .rodata@0x30000dfc, used by process_frame

dispatch_event also jumps through a pointer, at 0x300002da, which elfq can't follow, so process_frame may reach more.
",
    );
    // The MEMORY table has every block, with nothing in some.
    assert_output(
        &stdout(&[TCM_SAMPLE, "reach", "crc32"]),
        "\
crc32 and the data object it reaches take 48 bytes of code and 1024 bytes of data:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000e0    48     0  .itcm
0x20000000..0x20000450     0  1024  .data
0x20240000..0x20241000     0     0
0x30000000..0x30000edc     0     0

ADDRESS     SIZE  HOW   ITEM
0x10          48        crc32
0x20000000  1024  uses  crc_table, used by crc32
",
    );
    // A table that the program fills in as it runs.
    assert_output(
        &stdout(&[TCM_SAMPLE, "reach", "handlers"]),
        "handlers doesn't call or use anything.\n",
    );
    // A call to itself is no row.
    assert_output(
        &stdout(&[TCM_SAMPLE, "reach", "count_nodes"]),
        "count_nodes calls only itself.\n",
    );
    // Calls and jumps through pointers, which may go anywhere.
    assert_output(
        &stdout(&["testdata/bin/rust-cortex-m4.elf", "reach", "0x8001728"]),
        "<core::fmt::Formatter>::pad_integral::write_prefix makes 1 call and 1 jump through \
         pointers, at 0x8001744 and 0x8001762, which elfq can't follow.\n",
    );
}

#[test]
fn reach_sums_up_the_rest_of_each_block() {
    let reach = stdout(&[TCM_SAMPLE, "reach", "main"]);
    assert_output(
        &reach,
        "\
main and the 55 items it reaches take 2036 bytes of code and 5431 bytes of data; elfq can't follow 1 jump:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000e0   200     0  .itcm
0x20000000..0x20000450     0  1100  .data .bss
0x20240000..0x20241000     0  4096  .ocram_bss
0x30000000..0x30000edc  1836   235  .text .rodata

ADDRESS     SIZE  HOW              ITEM
0x30000570   132                   main
0x74          92  call via veneer  apply_filter, used by process_frame
0x40          52  call             filter_tap, used by apply_filter
0x10          48  call via veneer  crc32, used by process_frame
0xd8           8  call             __memcpy_veneer, used by apply_filter
0x20000000  1024  uses             crc_table, used by crc32
0x20000400    32  uses             stats, used by process_frame
              44                   9 more data objects in 0x20000000..0x20000450
0x20240000  4096  uses             tx_buffer, used by platform_write
0x30000320   376  call             process_frame, used by main
0x3000087c   308  call via veneer  memcpy, used by apply_filter
0x30000498   216  call             report, used by main
0x30000700   216  call             strlen, used by log_msg
0x30000668    96  tail call        platform_write, used by log_msg
0x30000188    84  call             parse_value, used by main
0x3000023c    76  call             format_u32, used by report
0x300002e4    60  call             app_tick, used by main
0x300001dc    60  tail call        parse_list, used by parse_value
0x30000160    40  call             decode_header, used by process_frame
0x30000218    34  call             count_nodes, used by report
0x30000288    26  call             log_msg, used by process_frame
0x300002cc    24  call             dispatch_event, used by main
0x300002a4    24  address taken    on_button (through a pointer), used by main
0x30000150    16  address taken    on_data (through a pointer), used by main
0x30000140    16  address taken    on_timer (through a pointer), used by main
0x300002bc    16  call             register_handler, used by main
              16                   2 more functions in 0x30000000..0x30000edc
0x30000eb4    32  uses             fir_coeffs, used by filter_tap
0x30000e70    25  uses             frame_a, used by main
0x30000e40   ~20  uses             .rodata@0x30000e40, used by main
0x30000e5c    17  uses             frame_b, used by main
0x30000df0   ~12  uses             .rodata@0x30000df0, used by process_frame
0x30000dfc   ~12  uses             .rodata@0x30000dfc, used by process_frame
0x30000e14   ~12  uses             .rodata@0x30000e14, used by report
             105                   13 more data objects in 0x30000000..0x30000edc

dispatch_event also jumps through a pointer, at 0x300002da, which elfq can't follow, so main may reach more.
",
    );
    // --all shows the rows that the sums stood for, in their place.
    let all = stdout(&[TCM_SAMPLE, "reach", "main", "--all"]);
    assert!(!all.contains(" more "), "{all}");
    let rows = |text: &str| {
        text.lines()
            .filter(|line| line.contains(", used by "))
            .count()
    };
    assert_eq!(rows(&all), 55);
    assert_eq!(rows(&reach), 31);
    assert!(
        all.contains(
            "\n0x20000400    32  uses             stats, used by process_frame\n\
             0x20000438    12  uses             handlers, used by register_handler\n"
        ),
        "{all}"
    );
}

#[test]
fn reach_on_stripped_firmware_has_the_same_rows() {
    assert_output(
        &stdout(&["testdata/bin/c-cortex-m7-stripped.elf", "reach", "0x0"]),
        "\
fn_0x0 and the 12 items it reaches take 446 bytes of code and 4128 bytes of data; elfq can't follow 1 jump:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000e0    24     0  .itcm
0x20000000..0x20000450     0    24  .bss
0x20240000..0x20241000     0  4096  .ocram_bss
0x30000000..0x30000edc   422     8  .text .rodata

ADDRESS      SIZE  HOW                   ITEM
0x0           ~16                        fn_0x0
0xd0           ~8  tail call             fn_0xd0, used by fn_0x0
0x20000438    ~12  uses                  .bss@0x20000438, used by fn_0x300002cc
0x20000444     ~4  uses                  .bss@0x20000444, used by fn_0x0
0x20000448     ~4  uses                  .bss@0x20000448, used by fn_0x30000668
0x2000044c     ~4  uses                  .bss@0x2000044c, used by fn_0x30000668
0x20240000  ~4096  uses                  .ocram_bss@0x20240000, used by fn_0x30000668
0x30000700   ~216  call                  fn_0x30000700, used by fn_0x30000288
0x30000668    ~96  tail call             fn_0x30000668, used by fn_0x30000288
0x300002e4    ~60  tail call via veneer  fn_0x300002e4, used by fn_0x0
0x30000288    ~26  call                  fn_0x30000288, used by fn_0x300002e4
0x300002cc    ~24  tail call             fn_0x300002cc, used by fn_0x300002e4
0x30000de8     ~8  uses                  .rodata@0x30000de8, used by fn_0x300002e4

fn_0x300002cc also jumps through a pointer, at 0x300002da, which elfq can't follow, so fn_0x0 may reach more.
",
    );
}

#[test]
fn reach_lists_calls_to_code_outside_any_function() {
    // A routine in the chip's ROM, which a veneer jumps to.
    let far = "testdata/bin/far-gnu-m4-nodebug.elf";
    assert_output(
        &stdout(&[far, "reach", "ram_calls_rom"]),
        "\
ram_calls_rom and the 2 items it reaches take 36 bytes of code and 4 bytes of data; elfq can't follow 1 call:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000a0    36     0  .ram
0x20000000..0x20000004     0     4  .bss
0x30000000..0x30000168     0     0

ADDRESS     SIZE  HOW              ITEM
0x3c          28                   ram_calls_rom
0x90           8  call             __rom_routine_veneer, used by ram_calls_rom
0x20000000     4  uses             counter, used by ram_calls_rom
0x1fff0000        call via veneer  (outside any function), used by ram_calls_rom
",
    );
    // Two veneers share a name, and each call to the ROM through them has a
    // row, after the items.
    let reach = stdout(&[far, "reach", "Reset_Handler"]);
    let rows: Vec<&str> = reach
        .lines()
        .filter(|line| line.contains("rom_routine") || line.contains("(outside"))
        .collect();
    assert_eq!(
        rows,
        [
            "0x90           8  call                  __rom_routine_veneer, used by ram_calls_rom",
            "0x30000138     8  call                  __rom_routine_veneer, used by flash_calls_rom",
            "0x1fff0000        call via veneer       (outside any function), used by ram_calls_rom",
            "0x1fff0000        call via veneer       (outside any function), used by flash_calls_rom",
        ],
        "{reach}"
    );
    // Each veneer's own jump is named by its address after "used by",
    // since its name finds both.
    assert_output(
        &stdout(&[far, "reach", "0x90"]),
        "\
__rom_routine_veneer takes 8 bytes of code and 0 bytes of data; elfq can't follow 1 jump:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000a0     8     0  .ram
0x20000000..0x20000004     0     0
0x30000000..0x30000168     0     0

ADDRESS     SIZE  HOW   ITEM
0x90           8        __rom_routine_veneer
0x1fff0000        jump  (outside any function), used by 0x90
",
    );
}

#[test]
fn reach_lists_imports_last() {
    // One block of memory, so no MEMORY table.
    assert_output(
        &stdout(&[SAMPLE, "reach", "log_msg"]),
        "\
log_msg and the 3 functions it reaches take 103 bytes of code and 0 bytes of data, plus 2 imports:

ADDRESS  SIZE  HOW        ITEM
0x1610     51             log_msg
0x1ac0     20  tail call  platform_write, used by log_msg
0x1080     16  call       strlen@plt, used by log_msg
0x1070     16  tail call  write@plt, used by platform_write
               tail call  strlen, used by strlen@plt
               tail call  write, used by write@plt
",
    );
    assert_output(
        &stdout(&[SAMPLE, "reach", "0x1080"]),
        "\
strlen@plt takes 16 bytes of code and 0 bytes of data, plus 1 import:

ADDRESS  SIZE  HOW        ITEM
0x1080     16             strlen@plt
               tail call  strlen, used by strlen@plt
",
    );
    // With one block, a row that sums up the rest names no block.
    let main = stdout(&[SAMPLE, "reach", "main"]);
    assert!(
        main.contains(
            "\n          111                 17 more data objects\n\
             \x20                             3 more imports\n\n"
        ),
        "{main}"
    );
    // The note on calls through pointers, made in several functions.
    let cpp = stdout(&["testdata/bin/cpp-x86_64.elf", "reach", "main"]);
    assert!(
        cpp.ends_with(
            "\napp::process_frame(app::Frame const&, app::Stage&) and app::dispatch(unsigned int, \
             unsigned int) also call and jump through pointers, which elfq can't follow, so main \
             may reach more.\n"
        ),
        "{cpp}"
    );
}

#[test]
fn reach_works_on_a_shared_library() {
    // No entry point is needed. The calls through PLT stubs are listed as
    // calls, and the jump in the PLT's header, to the dynamic loader, isn't
    // something elfq can't follow.
    assert_output(
        &stdout(&["testdata/bin/c-plt.so", "reach", "run"]),
        "\
run and the 14 items it reaches take 291 bytes of code and 4 bytes of data; elfq can't follow 1 call:

ADDRESS  SIZE  HOW            ITEM
0x1250     62                 run
0x1170     69  address taken  sum_plain (through a pointer), used by sum_to
0x1220     27  call via PLT   sum_to, used by run
0x1200     24  call via PLT   pick, used by run
0x1020     16  tail call      .plt, used by twice@plt
0x1040     16  call           pick@plt, used by run
0x11c0     16  address taken  sum_fast (through a pointer), used by sum_to
0x1060     16  call           sum_to@plt, used by run
0x1030     16  call           twice@plt, used by run
0x1070      8  call           negate@plt, used by run
0x1080      8  call           thrice@plt, used by run
0x1160      5  call via PLT   negate, used by run
0x1150      4  call via PLT   thrice, used by run
0x11f0      4  call via PLT   twice, used by run
0x4028      4  uses           fast, used by sum_to

run also calls through a pointer, at 0x1283, which elfq can't follow, so it may reach more.
",
    );
    assert_output(
        &stdout(&["testdata/bin/c-plt.so", "reach", ".plt"]),
        ".plt is the PLT header, which jumps only to the dynamic loader.\n",
    );
    // A stub keeps its `@plt` after "used by", in its shortest form.
    let stub = stdout(&["testdata/bin/cpp-x86_64.so", "reach", "0x1150"]);
    assert!(
        stub.contains("  app::log(char const*), used by log(char const*)@plt\n"),
        "{stub}"
    );
}

#[test]
fn reach_draws_what_it_reaches_as_a_tree() {
    // Veneers are steps of their own, each item hangs under the item
    // that uses it, in the order that one first uses them.
    assert_output(
        &stdout(&[TCM_SAMPLE, "reach", "SysTick_Handler", "--tree"]),
        "\
SysTick_Handler and the 12 items it reaches take 446 bytes of code and 4128 bytes of data; elfq can't follow 1 jump:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000e0    24     0  .itcm
0x20000000..0x20000450     0    24  .bss
0x20240000..0x20241000     0  4096  .ocram_bss
0x30000000..0x30000edc   422     8  .text .rodata

ADDRESS     SIZE  HOW        ITEM
0x0           16             SysTick_Handler
0x20000444     4  uses       ├─ systick_count
0xd0           8  tail call  └─ __app_tick_veneer
0x300002e4    60  tail call     └─ app_tick
0x300002cc    24  tail call        ├─ dispatch_event
0x20000438    12  uses             │  └─ handlers
0x30000de8    ~8  uses             ├─ .rodata@0x30000de8
0x30000288    26  call             └─ log_msg
0x30000700   216  call                ├─ strlen
0x30000668    96  tail call           └─ platform_write
0x2000044c     4  uses                   ├─ tx_head
0x20240000  4096  uses                   ├─ tx_buffer
0x20000448     4  uses                   └─ tx_tail

dispatch_event also jumps through a pointer, at 0x300002da, which elfq can't follow, so SysTick_Handler may reach more.
",
    );
    // The 30 largest items, with the items on their chains from main, and
    // rows that sum up the rest under each item.
    assert_output(
        &stdout(&[TCM_SAMPLE, "reach", "main", "--tree"]),
        "\
main and the 55 items it reaches take 2036 bytes of code and 5431 bytes of data; elfq can't follow 1 jump:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000e0   200     0  .itcm
0x20000000..0x20000450     0  1100  .data .bss
0x20240000..0x20241000     0  4096  .ocram_bss
0x30000000..0x30000edc  1836   235  .text .rodata

ADDRESS     SIZE  HOW            ITEM
0x30000570   132                 main
0x30000140    16  address taken  ├─ on_timer (through a pointer)
0x20000434     4  uses           │  └─ timer_ticks
0x300002bc    16  call           ├─ register_handler
0x20000438    12  uses           │  └─ handlers
0x300002a4    24  address taken  ├─ on_button (through a pointer)
              12                 │  └─ 2 more
0x30000150    16  address taken  ├─ on_data (through a pointer)
0x2000042c     4  uses           │  └─ data_events
0x30000e40   ~20  uses           ├─ .rodata@0x30000e40
0x30000188    84  call           ├─ parse_value
0x20000420     4  uses           │  ├─ config_sum
0x300001dc    60  tail call      │  └─ parse_list
0x30000e70    25  uses           ├─ frame_a
0x30000320   376  call           ├─ process_frame
0x30000160    40  call           │  ├─ decode_header
0x300009b0     8  call           │  ├─ __crc32_veneer
0x10          48  tail call      │  │  └─ crc32
0x20000000  1024  uses           │  │     └─ crc_table
0x300009b8     8  call           │  ├─ __apply_filter_veneer
0x74          92  tail call      │  │  └─ apply_filter
0x40          52  call           │  │     ├─ filter_tap
0x30000eb4    32  uses           │  │     │  └─ fir_coeffs
0xd8           8  call           │  │     └─ __memcpy_veneer
0x3000087c   308  tail call      │  │        └─ memcpy
0x20000400    32  uses           │  ├─ stats
0x30000df0   ~12  uses           │  ├─ .rodata@0x30000df0
0x30000288    26  call           │  ├─ log_msg
0x30000700   216  call           │  │  ├─ strlen
0x30000668    96  tail call      │  │  └─ platform_write
0x20240000  4096  uses           │  │     ├─ tx_buffer
               8                 │  │     └─ 2 more
0x30000dfc   ~12  uses           │  ├─ .rodata@0x30000dfc
               8                 │  └─ 2 more
0x30000e5c    17  uses           ├─ frame_b
0x30000e54     5  uses           ├─ frame_c
0x300002e4    60  call           ├─ app_tick
0x30000de8    ~8  uses           │  └─ .rodata@0x30000de8
0x300002cc    24  call           ├─ dispatch_event
0x30000498   216  call           └─ report
0x3000023c    76  call              ├─ format_u32
0x30000e14   ~12  uses              ├─ .rodata@0x30000e14
0x30000218    34  call              ├─ count_nodes
              84                    └─ 10 more

dispatch_event also jumps through a pointer, at 0x300002da, which elfq can't follow, so main may reach more.
",
    );
    // A row that sums up only imports has no SIZE, as their own rows have
    // none.
    let rust = stdout(&[
        "testdata/bin/rust-x86_64-nodebug.elf",
        "reach",
        "0x42060",
        "--tree",
    ]);
    let imports = rust.lines().find(|line| line.ends_with("└─ 3 more"));
    assert!(
        imports.is_some_and(|line| line.trim_start().starts_with('└')),
        "{rust}"
    );
    // --all shows every item, with no rows that sum some up.
    let all = stdout(&[TCM_SAMPLE, "reach", "main", "--tree", "--all"]);
    assert_eq!(
        all.lines().filter(|line| line.contains("─ ")).count(),
        55,
        "{all}"
    );
    assert!(!all.contains(" more\n"), "{all}");
    // The jump in the veneer to the chip's ROM hangs under the veneer, and
    // the first line counts it as a jump, as the tree shows it.
    assert_output(
        &stdout(&["testdata/bin/far-gnu-m4-nodebug.elf", "reach", "ram_calls_rom", "--tree"]),
        "\
ram_calls_rom and the 2 items it reaches take 36 bytes of code and 4 bytes of data; elfq can't follow 1 jump:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000a0    36     0  .ram
0x20000000..0x20000004     0     4  .bss
0x30000000..0x30000168     0     0

ADDRESS     SIZE  HOW   ITEM
0x3c          28        ram_calls_rom
0x20000000     4  uses  ├─ counter
0x90           8  call  └─ __rom_routine_veneer
0x1fff0000        jump     └─ (outside any function)
",
    );
    // Imports hang under the PLT stubs that jump to them.
    assert_output(
        &stdout(&[SAMPLE, "reach", "log_msg", "--tree"]),
        "\
log_msg and the 3 functions it reaches take 103 bytes of code and 0 bytes of data, plus 2 imports:

ADDRESS  SIZE  HOW        ITEM
0x1610     51             log_msg
0x1080     16  call       ├─ strlen@plt
               tail call  │  └─ strlen
0x1ac0     20  tail call  └─ platform_write
0x1070     16  tail call     └─ write@plt
               tail call        └─ write
",
    );
}

#[test]
fn reach_continues_a_deep_chain_below() {
    assert_output(
        &stdout(&[DEEP_SAMPLE, "reach", "chain1", "--tree"]),
        "\
chain1 and the 20 items it reaches take 402 bytes of code and 1024 bytes of data:

ADDRESS  SIZE  HOW   ITEM
0x13f0     16        chain1
0x13d0     17  call  └─ chain2
0x13b0     17  call     └─ chain3
0x1390     20  call        └─ chain4
0x1370     17  call           └─ chain5
0x1350     20  call              └─ chain6
0x1330     27  call                 └─ chain7
0x1310     20  call                    └─ chain8
0x12f0     17  call                       └─ chain9 (continued below)

                     chain9 (continued)
0x12d0     20  call  └─ chain10
0x12b0     20  call     └─ chain11
0x1290     23  call        └─ chain12
0x1270     20  call           └─ chain13
0x1250     19  call              └─ chain14
0x1230     23  call                 └─ chain15
0x1210     19  call                    └─ chain16
0x11f0     22  call                       └─ chain17 (continued below)

                     chain17 (continued)
0x11d0     20  call  └─ chain18
0x11b0     20  call     └─ chain19
0x1190     25  call        └─ chain20
0x4420   1024  uses           └─ chain_table
",
    );
}

#[test]
fn reach_to_lists_only_what_it_reaches_in_ranges() {
    // The blocks of the M7 sample that aren't ITCM or DTCM: OCRAM and
    // flash. app_tick's row, through the veneer in ITCM, is the one that
    // callees lists for SysTick_Handler with --to, and the jump through a
    // pointer stays, since flash holds functions whose addresses main
    // takes.
    let slow = ["--to", "0x20240000+1M", "--to", "0x30000000+16M"];
    assert_output(
        &crossing(&[&[TCM_SAMPLE, "reach", "SysTick_Handler"][..], &slow].concat()),
        "\
SysTick_Handler reaches 7 items in 0x20240000..0x20340000 (.ocram_bss) and 0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx), which take 422 bytes of code and 4104 bytes of data; elfq can't follow 1 jump, which may go there:

ADDRESS     SIZE  HOW                   AT                      ITEM
                  jump                  0x300002da              (through a pointer), used by dispatch_event
0x20240000  4096  uses                  0x30000674, 0x30000696  tx_buffer, used by platform_write
0x30000700   216  call                  0x30000292              strlen, used by log_msg
0x30000668    96  tail call             0x3000029e              platform_write, used by log_msg
0x300002e4    60  tail call via veneer  0x8                     app_tick, used by SysTick_Handler
0x30000288    26  call                  0x30000304              log_msg, used by app_tick
0x300002cc    24  tail call             0x300002fe, 0x30000310  dispatch_event, used by app_tick
0x30000de8    ~8  uses                  0x30000302              .rodata@0x30000de8, used by app_tick
",
    );
    assert!(
        crossing(&[&[TCM_SAMPLE, "callees", "SysTick_Handler"][..], &slow].concat())
            .ends_with("  tail call via veneer  0x8  app_tick, used by SysTick_Handler\n")
    );
    // The chains to them, with the jump under the code it is in.
    assert_output(
        &crossing(&[&[TCM_SAMPLE, "reach", "SysTick_Handler", "--tree"][..], &slow].concat()),
        "\
SysTick_Handler reaches 7 items in 0x20240000..0x20340000 (.ocram_bss) and 0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx), which take 422 bytes of code and 4104 bytes of data; elfq can't follow 1 jump, which may go there:

ADDRESS     SIZE  HOW        ITEM
0x0           16             SysTick_Handler
0xd0           8  tail call  └─ __app_tick_veneer
0x300002e4    60  tail call     └─ app_tick
0x300002cc    24  tail call        ├─ dispatch_event
                  jump             │  └─ (through a pointer)
0x30000de8    ~8  uses             ├─ .rodata@0x30000de8
0x30000288    26  call             └─ log_msg
0x30000700   216  call                ├─ strlen
0x30000668    96  tail call           └─ platform_write
0x20240000  4096  uses                   └─ tx_buffer
",
    );
    // crc32 stays in TCM, which a script can check for with exit status 0.
    for format in [&[][..], &["--tree"]] {
        assert_output(
            &stdout(&[&[TCM_SAMPLE, "reach", "crc32"][..], &slow, format].concat()),
            "crc32 reaches nothing in 0x20240000..0x20340000 (.ocram_bss) and \
             0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx).\n",
        );
    }
    // No function in ITCM has its address taken, so no call through a
    // pointer goes there.
    assert_output(
        &crossing(&[TCM_SAMPLE, "reach", "main", "--to", "0x0+256K"]),
        "\
main reaches 4 functions in 0x00000000..0x00040000 (.itcm), which take 200 bytes of code and 0 bytes of data:

ADDRESS  SIZE  HOW              AT                      ITEM
0x74       92  call via veneer  0x30000386, 0x30000454  apply_filter, used by process_frame
0x40       52  call             0x9a                    filter_tap, used by apply_filter
0x10       48  call via veneer  0x3000034c              crc32, used by process_frame
0xd8        8  call             0xbc                    __memcpy_veneer, used by apply_filter
",
    );
    // SYMBOL counts too: callees refuses a range that holds only SYMBOL,
    // but reach lists it.
    assert_output(
        &crossing(&[TCM_SAMPLE, "reach", "crc32", "--to", "0x10+48"]),
        "\
crc32 takes 48 bytes of code and 0 bytes of data in 0x00000010..0x00000040 (.itcm), and reaches nothing else there:

ADDRESS  SIZE  HOW  AT  ITEM
0x10       48           crc32
",
    );
    assert_output(
        &stderr(&[TCM_SAMPLE, "callees", "crc32", "--to", "0x10+48"]),
        "error: --to 0x00000010..0x00000040 holds nothing outside crc32, and callees lists only \
         what it uses outside it\n",
    );
    // A range that holds nothing is most likely mistyped.
    assert_output(
        &stderr(&[TCM_SAMPLE, "reach", "main", "--to", "0x40000000+4K"]),
        "error: --to 0x40000000..0x40001000 holds no functions or data, and no code in the file \
         calls or jumps there\n",
    );
}

#[test]
fn reach_takes_a_pattern_that_matches_one_item() {
    // With --to too, the answer is the item's, and its first line says
    // what the pattern found; across_commands/names.rs checks the answer
    // without --to against the name's.
    let dtcm = ["--to", "0x20000000+64K"];
    let reach = |symbol: &str, json: &[&str]| {
        crossing(&[&[TCM_SAMPLE, "reach", symbol][..], &dtcm, json].concat())
    };
    assert_output(
        &reach("on_b*", &[]),
        "\
on_button (the one item named on_b*) reaches 3 data objects in 0x20000000..0x20010000 (.data and .bss), which take 0 bytes of code and 12 bytes of data:

ADDRESS     SIZE  HOW   AT          ITEM
0x20000430     4  uses  0x300002a4  button_presses, used by on_button
0x2000044c     4  uses  0x3000066a  tx_head, used by platform_write
0x20000448     4  uses  0x3000068e  tx_tail, used by platform_write
",
    );
    assert_output(
        &reach("on_b*", &["--json"]),
        &reach("on_button", &["--json"]),
    );
    // Several are an error that lists them.
    assert_output(
        &stderr(&[TCM_SAMPLE, "reach", "on_*"]),
        "\
error: `on_*` matches 3 functions; use one on the left instead:
  on_timer
  on_data
  on_button
",
    );
    // --to takes only ranges.
    assert_output(
        &stderr(&[TCM_SAMPLE, "reach", "on_button", "--to", "on_*"]),
        "\
error: invalid value 'on_*' for '--to <RANGE>': expected START..END or START+SIZE, such as 0x0+256K

For more information, try '--help'.
",
    );
}

#[test]
fn reach_to_lists_a_veneer_and_its_target() {
    // The veneer in ITCM is in the ranges too, so it has a row of its own,
    // where callees steps over it.
    let ranges = ["--to", "0xd0+8", "--to", "0x30000000+16M"];
    assert_output(
        &crossing(&[&[TCM_SAMPLE, "reach", "SysTick_Handler"][..], &ranges].concat()),
        "\
SysTick_Handler reaches 7 items in 0x000000d0..0x000000d8 (.itcm) and 0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx), which take 430 bytes of code and 8 bytes of data; elfq can't follow 1 jump, which may go there:

ADDRESS     SIZE  HOW                   AT                      ITEM
                  jump                  0x300002da              (through a pointer), used by dispatch_event
0xd0           8  tail call             0x8                     __app_tick_veneer, used by SysTick_Handler
0x30000700   216  call                  0x30000292              strlen, used by log_msg
0x30000668    96  tail call             0x3000029e              platform_write, used by log_msg
0x300002e4    60  tail call via veneer  0x8                     app_tick, used by SysTick_Handler
0x30000288    26  call                  0x30000304              log_msg, used by app_tick
0x300002cc    24  tail call             0x300002fe, 0x30000310  dispatch_event, used by app_tick
0x30000de8    ~8  uses                  0x30000302              .rodata@0x30000de8, used by app_tick
",
    );
    assert_output(
        &crossing(&[&[TCM_SAMPLE, "callees", "SysTick_Handler"][..], &ranges].concat()),
        "\
SysTick_Handler uses 1 function in 0x000000d0..0x000000d8 (.itcm) and 0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx):

ADDRESS     SIZE  HOW                   AT   CALLEE
0x300002e4    60  tail call via veneer  0x8  app_tick, used by SysTick_Handler
",
    );
}

#[test]
fn reach_to_lists_calls_to_code_outside_any_function() {
    // Calls through veneers to the chip's ROM, which holds no item. callees
    // sees none from Reset_Handler, which reaches them through others.
    let far = "testdata/bin/far-gnu-m4-nodebug.elf";
    let rom = ["--to", "0x1fff0000+64K"];
    assert_output(
        &crossing(&[&[far, "reach", "Reset_Handler"][..], &rom].concat()),
        "\
Reset_Handler reaches no items in 0x1fff0000..0x20000000, but elfq can't follow 2 calls, which go there:

ADDRESS     SIZE  HOW              AT          ITEM
0x1fff0000        call via veneer  0x46        (outside any function), used by ram_calls_rom
0x1fff0000        call via veneer  0x300000ee  (outside any function), used by flash_calls_rom
",
    );
    assert_output(
        &crossing(&[&[far, "reach", "Reset_Handler", "--tree"][..], &rom].concat()),
        "\
Reset_Handler reaches no items in 0x1fff0000..0x20000000, but elfq can't follow 2 jumps, which go there:

ADDRESS     SIZE  HOW        ITEM
0x30000110    32             Reset_Handler
0x30000158     8  call       ├─ __ram_calls_rom_veneer
0x3c          28  tail call  │  └─ ram_calls_rom
0x90           8  call       │     └─ __rom_routine_veneer
0x1fff0000        jump       │        └─ (outside any function)
0x300000e4    28  call       └─ flash_calls_rom
0x30000138     8  call          └─ __rom_routine_veneer
0x1fff0000        jump             └─ (outside any function)
",
    );
    assert_output(
        &stdout(&[&[far, "callees", "Reset_Handler"][..], &rom].concat()),
        "Reset_Handler uses nothing in 0x1fff0000..0x20000000.\n",
    );
}

#[test]
fn reach_needs_an_item_whose_code_elfq_can_read() {
    assert_output(
        &stderr(&[SAMPLE, "reach", "_init"]),
        "error: elfq doesn't know where _init ends, so it can't read its code\n",
    );
    assert_output(
        &stderr(&[SAMPLE, "reach", "__libc_start_main"]),
        "error: __libc_start_main is imported from a shared library, so its code isn't in this \
         file\n",
    );
}

#[test]
fn reach_without_symbol_lists_where_the_program_starts() {
    // Each block's cell is code and data together, as `reach START` counts
    // them: SysTick_Handler's are its MEMORY table's. Default_Handler, which
    // only loops, takes 2 bytes itself, and its WHY names the exceptions it
    // handles.
    assert_output(
        &stdout(&[TCM_SAMPLE, "reach"]),
        "\
Where testdata/bin/c-cortex-m7.elf starts, and the bytes of code and data that each reaches in each block of memory:

0x00000000  0x20000000  0x20240000  0x30000000  WHY                        START
       200        1100        4096        2071  main function              main (may reach more)
       200        1100        4096        2345  Reset (entry point)        Reset_Handler (may reach more)
         0           0           0           2  NMI, HardFault and 6 more  Default_Handler
        24          24        4096         430  SysTick                    SysTick_Handler (may reach more)

The starts marked (may reach more) reach calls or jumps that elfq can't follow; reach START names them.
",
    );
    // WHY comes before the name, so it keeps its cut, as AT does; --json
    // names every exception.
    assert_output(
        &stdout(&[TCM_SAMPLE, "reach", "--all"]),
        &stdout(&[TCM_SAMPLE, "reach"]),
    );
    // One block, so CODE and DATA. The startup and exit arrays' functions
    // come in the order the program runs the arrays. _init and _fini are
    // roots, but elfq can't read them, and the PLT's header only jumps to
    // the dynamic loader.
    assert_output(
        &stdout(&[SAMPLE, "reach"]),
        "\
Where testdata/bin/c-x86_64.elf starts, and the bytes of code and data that each reaches:

CODE  DATA  WHY                    START
2218  1379  main function          main (may reach more)
2256  1379  entry point            _start (may reach more)
  80     1  listed in .init_array  frame_dummy (may reach more)
 128    25  listed in .fini_array  __do_global_dtors_aux (may reach more)

The starts marked (may reach more) reach calls or jumps that elfq can't follow; reach START names them.
_init and _fini are starts too, but elfq doesn't know where they end, so it can't count what they reach.
",
    );
    // A shared library's main is where no program starts.
    assert_output(
        &stdout(&[LIBRARY_SAMPLE, "reach"]),
        "\
Where testdata/bin/c-x86_64.so starts, and the bytes of code and data that each reaches:

CODE  DATA  WHY                    START
  80     1  listed in .init_array  frame_dummy (may reach more)
 128    33  listed in .fini_array  __do_global_dtors_aux (may reach more)

The starts marked (may reach more) reach calls or jumps that elfq can't follow; reach START names them.
_init and _fini are starts too, but elfq doesn't know where they end, so it can't count what they reach.
elfq doesn't count a shared library's exported functions as starting points yet.
",
    );
    // Stripped, the same firmware has no main, and the functions elfq
    // finds by following the code are named by address.
    assert_output(
        &stdout(&["testdata/bin/c-cortex-m7-stripped.elf", "reach"]),
        "\
Where testdata/bin/c-cortex-m7-stripped.elf starts, and the bytes of code and data that each reaches in each block of memory:

0x00000000  0x20000000  0x20240000  0x30000000  WHY                        START
       200        1100        4096        2354  Reset (entry point)        entry (may reach more)
         0           0           0           2  NMI, HardFault and 6 more  fn_0x300005f4
        24          24        4096         430  SysTick                    fn_0x0 (may reach more)

The starts marked (may reach more) reach calls or jumps that elfq can't follow; reach START names them.
",
    );
}

#[test]
fn reach_finds_the_vector_table_when_main_is_the_entry_point() {
    // No word of the vector table holds the entry point, so elfq takes the
    // table at the start of .isr_vector, the first section by address that
    // begins like one. Reset_Handler is startup code all the same: its
    // copies of .itcm and .data aren't uses of the items there, so it
    // reaches what it reaches as the entry point of c-cortex-m7.elf.
    assert_output(
        &stdout(&[TCM_ENTRY_MAIN_SAMPLE, "reach"]),
        "\
Where testdata/bin/c-cortex-m7-entry-main.elf starts, and the bytes of code and data that each reaches in each block of memory:

0x00000000  0x20000000  0x20240000  0x30000000  WHY                        START
       200        1100        4096        2071  entry point                main (may reach more)
       200        1100        4096        2345  Reset                      Reset_Handler (may reach more)
         0           0           0           2  NMI, HardFault and 6 more  Default_Handler
        24          24        4096         430  SysTick                    SysTick_Handler (may reach more)

The starts marked (may reach more) reach calls or jumps that elfq can't follow; reach START names them.
",
    );
    // Stripped, elfq finds the handlers' code from the table too, so
    // nothing is left unreached.
    let stripped = "testdata/bin/c-cortex-m7-entry-main-stripped.elf";
    assert_output(
        &stdout(&[stripped, "reach"]),
        "\
Where testdata/bin/c-cortex-m7-entry-main-stripped.elf starts, and the bytes of code and data that each reaches in each block of memory:

0x00000000  0x20000000  0x20240000  0x30000000  WHY                        START
       200        1100        4096        2080  entry point                entry (may reach more)
       200        1100        4096        2354  Reset                      fn_0x300005f8 (may reach more)
         0           0           0           2  NMI, HardFault and 6 more  fn_0x300005f4
        24          24        4096         430  SysTick                    fn_0x0 (may reach more)

The starts marked (may reach more) reach calls or jumps that elfq can't follow; reach START names them.
",
    );
    assert!(!stdout(&[stripped, "top"]).contains("Nothing reaches"));
    // The overview starts from the reset handler, not from main, which
    // the stripped file doesn't name.
    let overview = stdout(&[stripped]);
    assert!(overview.contains(&format!("elfq {stripped} retained fn_0x300005f8\n")));
    assert_eq!(
        stdout(&[TCM_ENTRY_MAIN_SAMPLE, "why", "Reset_Handler"]),
        "Reset_Handler is the reset handler in the vector table: the processor runs it at reset.\n"
    );
    assert_eq!(
        stdout(&[TCM_ENTRY_MAIN_SAMPLE, "why", "main"]),
        "main is the entry point that the ELF header names, but at reset the processor runs the \
         reset handler in the vector table.\n"
    );
}

#[test]
fn reach_without_symbol_refuses_what_needs_one() {
    // --to, --range and --tree need SYMBOL. The command that each refusal
    // suggests names the function that the overview starts from, as its
    // Try line does, keeps the other options, and answers.
    for (args, expected) in [
        (
            &[TCM_SAMPLE, "reach", "--to", "0x0+1K", "--json"][..],
            "error: reach --to needs a SYMBOL, such as elfq testdata/bin/c-cortex-m7.elf reach \
             main --to 0x00000000..0x00000400 --json\n",
        ),
        (
            &[
                "testdata/bin/c-cortex-m7-stripped.elf",
                "reach",
                "--tree",
                "--all",
            ],
            "error: reach --tree needs a SYMBOL, such as elfq \
             testdata/bin/c-cortex-m7-stripped.elf reach entry --all --tree\n",
        ),
        (
            &[
                "testdata/bin/c-x86_64-stripped.elf",
                "reach",
                "--range",
                "0x1000+4K",
                "--tree",
            ],
            "error: reach takes --to, not --range, to list only what a SYMBOL reaches in a \
             range, such as elfq testdata/bin/c-x86_64-stripped.elf reach fn_0x1170 --to \
             0x00001000..0x00002000 --tree\n",
        ),
    ] {
        assert_output(&stderr(args), expected);
        let (_, suggested) = expected.trim_end().split_once("such as elfq ").unwrap();
        run(&suggested.split(' ').collect::<Vec<&str>>()).printed(Exit::ZeroOrOne);
    }
}

#[test]
fn reach_has_the_whole_tree() {
    let reach = elfq_json(&[TCM_SAMPLE, "reach", "SysTick_Handler"]);
    assert_eq!(reach["symbol"]["name"], "SysTick_Handler");
    assert_eq!(reach["code_bytes"], 446);
    assert_eq!(reach["data_bytes"], 4128);
    // Each item after the one that uses it, its parent, and that one's
    // children in the order it uses them. The veneer has a row of its own,
    // which the text leaves out of how app_tick is used.
    let rows: Vec<String> = reach["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            let name = item["name"].as_str().unwrap();
            format!("{} {name} ({}) {}", item["id"], item["how"], item["parent"])
        })
        .collect();
    assert_eq!(
        rows,
        [
            "0 SysTick_Handler (null) null",
            "1 systick_count (\"address taken\") 0",
            "2 __app_tick_veneer (\"tail call\") 0",
            "3 app_tick (\"tail call\") 2",
            "4 dispatch_event (\"tail call\") 3",
            "5 handlers (\"address taken\") 4",
            "6 .rodata@0x30000de8 (\"address taken\") 3",
            "7 log_msg (\"call\") 3",
            "8 strlen (\"call\") 7",
            "9 platform_write (\"tail call\") 7",
            "10 tx_head (\"address taken\") 9",
            "11 tx_buffer (\"address taken\") 9",
            "12 tx_tail (\"address taken\") 9",
        ]
    );
    assert_eq!(reach["items"][11]["block"], "0x20240000");
    assert_eq!(
        reach["items"][11]["sites"],
        json!(["0x30000674", "0x30000696"])
    );
    // Only the sites of the use on the chain: cleanup_stdio also tail-calls
    // _fflush_r, at 0x800336c.
    let cleanup = elfq_json(&["testdata/bin/c-strtod.elf", "reach", "cleanup_stdio"]);
    let fflush = &cleanup["items"][2];
    assert_eq!(
        (&fflush["name"], &fflush["how"], &fflush["parent"]),
        (&json!("_fflush_r"), &json!("call"), &json!(0))
    );
    assert_eq!(fflush["sites"], json!(["0x800334c", "0x800335a"]));
    assert_eq!(
        reach["blocks"][3],
        json!({
            "start": "0x30000000",
            "end": "0x30000edc",
            "code_bytes": 422,
            "data_bytes": 8,
            "sections": [".text", ".rodata"]
        })
    );
    // The jump through the handler table, which elfq can't follow.
    let unresolved = &reach["unresolved"];
    assert_eq!(names(&json!([unresolved[0]["caller"]])), ["dispatch_event"]);
    assert_eq!(unresolved[0]["site"], "0x300002da");
    assert_eq!(unresolved[0]["target"], Value::Null);
    assert_eq!(reach["complete"], false);

    // Functions there only because main takes their addresses.
    let main = elfq_json(&[TCM_SAMPLE, "reach", "main"]);
    let mut through_pointer: Vec<&str> = main["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["through_pointer"] == true)
        .map(|item| item["name"].as_str().unwrap())
        .collect();
    through_pointer.sort();
    assert_eq!(through_pointer, ["on_button", "on_data", "on_timer"]);
    // Only the processor calls through the vector table, so a handler it
    // lists is not there through a pointer.
    let vector_table = elfq_json(&[TCM_SAMPLE, "reach", "vector_table"]);
    let handlers: Vec<(&Value, &Value)> = vector_table["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["parent"] == 0 && item["kind"] == "function")
        .map(|item| (&item["name"], &item["through_pointer"]))
        .collect();
    assert_eq!(
        handlers,
        [
            (&json!("Reset_Handler"), &json!(false)),
            (&json!("Default_Handler"), &json!(false)),
            (&json!("SysTick_Handler"), &json!(false)),
        ]
    );
    // A veneer's jump to the chip's ROM is where it is, in the veneer.
    let rom = elfq_json(&[
        "testdata/bin/far-gnu-m4-nodebug.elf",
        "reach",
        "ram_calls_rom",
    ]);
    assert_eq!(
        rom["unresolved"][0]["caller"]["name"],
        "__rom_routine_veneer"
    );
    assert_eq!(rom["unresolved"][0]["target"], "0x1fff0000");
    // An import is in no block.
    let log_msg = elfq_json(&[SAMPLE, "reach", "log_msg"]);
    let write = &log_msg["items"][5];
    assert_eq!(
        (&write["name"], &write["block"]),
        (&json!("write"), &Value::Null)
    );
    assert_eq!(log_msg["complete"], true);
    // Without --to, nothing says what is in its ranges.
    assert_eq!(reach["to"], Value::Null);
    assert_eq!(reach["items"][0]["in_to"], Value::Null);
}

#[test]
fn reach_to_has_the_chains_to_what_is_in_its_ranges() {
    let reach = run(&[
        TCM_SAMPLE,
        "reach",
        "SysTick_Handler",
        "--to",
        "0x30000000+16M",
        "--json",
    ])
    .json(1);
    assert_eq!(
        reach["to"],
        json!([{"start": "0x30000000", "end": "0x31000000"}])
    );
    // Only what is in flash counts.
    assert_eq!(reach["code_bytes"], 422);
    assert_eq!(reach["data_bytes"], 8);
    assert_eq!(reach["blocks"][3]["code_bytes"], 422);
    assert_eq!(reach["blocks"][2]["data_bytes"], 0);
    // The items in flash, and those on their chains from SYMBOL, numbered
    // again: the data in DTCM and OCRAM is gone, and the veneer in ITCM
    // stays, on app_tick's chain.
    let rows: Vec<String> = reach["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            let name = item["name"].as_str().unwrap();
            format!("{} {name} {} {}", item["id"], item["in_to"], item["parent"])
        })
        .collect();
    assert_eq!(
        rows,
        [
            "0 SysTick_Handler false null",
            "1 __app_tick_veneer false 0",
            "2 app_tick true 1",
            "3 dispatch_event true 2",
            "4 .rodata@0x30000de8 true 2",
            "5 log_msg true 2",
            "6 strlen true 5",
            "7 platform_write true 5",
        ]
    );
    // The jump through a pointer may go to the handlers in flash.
    assert_eq!(reach["unresolved"][0]["site"], "0x300002da");
    assert_eq!(reach["complete"], false);

    // It can't go to ITCM, whose functions no pointer goes to.
    let itcm = elfq_json(&[TCM_SAMPLE, "reach", "dispatch_event", "--to", "0x0+256K"]);
    assert_eq!(itcm["items"].as_array().unwrap().len(), 1);
    assert_eq!(itcm["unresolved"], json!([]));
    assert_eq!(itcm["complete"], true);

    // Calls to a known address in the ranges are a whole answer. The
    // veneer that makes the call is on the chain to it.
    let rom = run(&[
        "testdata/bin/far-gnu-m4-nodebug.elf",
        "reach",
        "ram_calls_rom",
        "--to",
        "0x1fff0000+64K",
        "--json",
    ])
    .json(1);
    assert_eq!(rom["items"][1]["name"], "__rom_routine_veneer");
    assert_eq!(rom["items"][1]["parent"], 0);
    assert_eq!(rom["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        rom["unresolved"][0]["caller"]["name"],
        "__rom_routine_veneer"
    );
    assert_eq!(rom["unresolved"][0]["target"], "0x1fff0000");
    assert_eq!(rom["complete"], true);
}

#[test]
fn reach_without_symbol_has_each_start() {
    // Each start is a line, with its bytes in each block apart, and every
    // exception that it handles.
    let text = stdout(&[TCM_SAMPLE, "reach", "--json"]);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 6, "{text}");
    assert_eq!(lines[0], r#"{"starts":["#);
    assert_eq!(lines[5], r#"],"unreadable":[]}"#);
    let starts: Value = serde_json::from_str(&text).unwrap();
    let starts = &starts["starts"];
    assert_eq!(
        names(starts),
        [
            "main",
            "Reset_Handler",
            "Default_Handler",
            "SysTick_Handler"
        ]
    );
    assert_eq!(starts[0]["root"], Value::Null);
    assert_eq!(starts[1]["root"], "entry point");
    assert_eq!(starts[1]["exceptions"], json!(["Reset"]));
    assert_eq!(starts[2]["root"], "listed in the vector table");
    assert_eq!(
        starts[2]["exceptions"],
        json!([
            "NMI",
            "HardFault",
            "MemManage",
            "BusFault",
            "UsageFault",
            "SVCall",
            "DebugMonitor",
            "PendSV"
        ])
    );
    assert_eq!(
        starts[3],
        json!({
            "name": "SysTick_Handler",
            "kind": "function",
            "section": ".itcm",
            "address": "0x0",
            "size": 16,
            "size_estimated": false,
            "root": "listed in the vector table",
            "exceptions": ["SysTick"],
            "code_bytes": 446,
            "data_bytes": 4128,
            "blocks": [
                {"start": "0x0", "end": "0xe0", "code_bytes": 24, "data_bytes": 0},
                {"start": "0x20000000", "end": "0x20000450", "code_bytes": 0, "data_bytes": 24},
                {"start": "0x20240000", "end": "0x20241000", "code_bytes": 0, "data_bytes": 4096},
                {"start": "0x30000000", "end": "0x30000edc", "code_bytes": 422, "data_bytes": 8}
            ],
            "complete": false
        })
    );
    assert_eq!(starts[2]["complete"], true);

    // The roots whose end elfq doesn't know are listed apart.
    let library = elfq_json(&[LIBRARY_SAMPLE, "reach"]);
    assert_eq!(
        names(&library["starts"]),
        ["frame_dummy", "__do_global_dtors_aux"]
    );
    assert_eq!(names(&library["unreadable"]), ["_init", "_fini"]);
    assert_eq!(library["starts"][0]["root"], "listed in .init_array");
    assert_eq!(library["starts"][0]["exceptions"], json!([]));
    let stripped = elfq_json(&["testdata/bin/c-cortex-m7-stripped.elf", "reach"]);
    assert_eq!(
        names(&stripped["starts"]),
        ["entry", "fn_0x300005f4", "fn_0x0"]
    );
    assert_eq!(stripped["starts"][0]["root"], "entry point");
    assert_eq!(stripped["starts"][0]["exceptions"], json!(["Reset"]));
}

#[test]
fn reach_names_what_it_reaches_but_cant_read() {
    // apply_filter calls memcpy through __memcpy_veneer, which has no size
    // now, so memcpy isn't there.
    let path = ElfBytes::sample("testdata/bin/c-cortex-m7-nodebug.elf")
        .set_symbol_size("__memcpy_veneer", 0)
        .save("reach-unsized-veneer.elf");
    assert_output(
        &run(&[&path, "reach", "apply_filter"]).printed(0),
        "\
apply_filter and the 3 items it reaches take 144 bytes of code and 32 bytes of data:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000e0   144     0  .itcm
0x20000000..0x20000450     0     0
0x20240000..0x20241000     0     0
0x30000000..0x30000edc     0    32  .rodata

ADDRESS     SIZE  HOW   ITEM
0x74          92        apply_filter
0x40          52  call  filter_tap, used by apply_filter
0xd8           ?  call  __memcpy_veneer, used by apply_filter
0x30000eb4    32  uses  fir_coeffs, used by filter_tap

elfq doesn't know where __memcpy_veneer ends, so it can't read its code.
",
    );
    let reach = elfq_json(&[&path, "reach", "apply_filter"]);
    assert_eq!(reach["unreadable"][0]["name"], "__memcpy_veneer");
    assert_eq!(reach["complete"], false);
    // With --to, what elfq can't read may reach the ranges, so the exit
    // status is 1, even when nothing is listed.
    for (to, expected) in [
        (
            "0x20240000+1M",
            "\
In the code elfq can read, apply_filter reaches nothing in 0x20240000..0x20340000 (.ocram_bss).
elfq doesn't know where __memcpy_veneer ends, so it can't read its code.
",
        ),
        (
            "0x30000000+16M",
            "\
apply_filter reaches 1 data object in 0x30000000..0x31000000 (.isr_vector, .text, .rodata and .ARM.exidx), which takes 0 bytes of code and 32 bytes of data:

ADDRESS     SIZE  HOW   AT    ITEM
0x30000eb4    32  uses  0x56  fir_coeffs, used by filter_tap

elfq doesn't know where __memcpy_veneer ends, so it can't read its code.
",
        ),
    ] {
        let stdout = crossing(&[&path, "reach", "apply_filter", "--to", to]);
        assert_output(&stdout, expected);
        let reach = run(&[&path, "reach", "apply_filter", "--to", to, "--json"]).json(1);
        assert_eq!(reach["unreadable"][0]["name"], "__memcpy_veneer");
        assert_eq!(reach["complete"], false);
    }
}

#[test]
fn reach_lists_an_item_in_no_block_after_the_blocks() {
    // tx_buffer, in a section of size 0 now, so in no block of memory.
    let path = ElfBytes::sample("testdata/bin/c-cortex-m7-nodebug.elf")
        .set_section(".ocram_bss", SectionField::Size, 0)
        .save("reach-no-block.elf");
    assert_output(
        &stdout(&[&path, "reach", "platform_write"]),
        "\
platform_write and the 3 data objects it reaches take 96 bytes of code and 4104 bytes of data:

MEMORY                  CODE  DATA  SECTIONS
0x00000000..0x000000e0     0     0
0x20000000..0x20000450     0     8  .bss
0x30000000..0x30000edc    96     0  .text

ADDRESS     SIZE  HOW   ITEM
0x30000668    96        platform_write
0x2000044c     4  uses  tx_head, used by platform_write
0x20000448     4  uses  tx_tail, used by platform_write
0x20240000  4096  uses  tx_buffer, used by platform_write
",
    );
    let reach = elfq_json(&[&path, "reach", "platform_write"]);
    let tx_buffer = &reach["items"][2];
    assert_eq!(tx_buffer["name"], "tx_buffer");
    assert_eq!(tx_buffer["block"], Value::Null);
}

#[test]
fn reach_names_the_exceptions_that_the_architecture_has() {
    // Default_Handler's exceptions, as `reach --json` names them, in the
    // file at `path`, a build of c-cortex-m7.elf, where it is in entries 2
    // to 6, 11, 12 and 14 of the vector table.
    let exceptions = |path: &str| {
        let reach = elfq_json(&[path, "reach"]);
        let starts = reach["starts"].as_array().unwrap();
        let handler = starts
            .iter()
            .find(|start| start["name"] == "Default_Handler")
            .unwrap();
        handler["exceptions"].clone()
    };
    // In the reserved entries too, which the processor never takes, so
    // they aren't named or counted.
    let mut data = ElfBytes::sample(TCM_SAMPLE);
    let address = data.symbol_address("Default_Handler");
    for entry in [7, 8, 9, 10, 13] {
        data.set_vector_entry(entry, address | 1);
    }
    let path = data.save("reserved-entries.elf");
    assert_eq!(
        exceptions(&path),
        json!([
            "NMI",
            "HardFault",
            "MemManage",
            "BusFault",
            "UsageFault",
            "SVCall",
            "DebugMonitor",
            "PendSV"
        ])
    );
    let stdout = run(&[&path, "reach"]).printed(0);
    assert!(
        stdout.contains("  NMI, HardFault and 6 more  Default_Handler\n"),
        "{stdout}"
    );
    // Armv6S-M, as on a Cortex-M0, has no MemManage, BusFault, UsageFault
    // or DebugMonitor: their entries are reserved.
    let path = for_arm_version(TCM_SAMPLE, Some(12)).save("armv6s-m.elf");
    assert_eq!(
        exceptions(&path),
        json!(["NMI", "HardFault", "SVCall", "PendSV"])
    );
}

/// The C and Rust samples for Cortex-M with symbols.
///
/// The checks run on every item of these and of [`FAR_SAMPLES`], the
/// builds of c/far.c; see [`on_each_sample`].
const C_AND_RUST_SAMPLES: [&str; 5] = [
    "testdata/bin/c-cortex-m4.elf",
    "testdata/bin/c-cortex-m4-cmsis-nodebug.elf",
    TCM_SAMPLE,
    "testdata/bin/c-softfloat-nodebug.elf",
    "testdata/bin/rust-cortex-m4-nodebug.elf",
];

/// What `elfq SAMPLE ARGS` prints, run in this process.
///
/// It runs with [`run_in_process`], which is much quicker than starting
/// elfq for each function, and checks, as `Run::printed` does, that the
/// outcome means exit status `status`. Nothing checks stderr: a warning
/// goes to the test's own.
#[track_caller]
fn text_in_process(sample: &str, args: &[&str], status: impl Into<Exit>) -> String {
    let status = status.into();
    let (outcome, text) = run_in_process(sample, args);
    let exit = match outcome {
        Outcome::Answered => 0,
        Outcome::Crossed | Outcome::Differs | Outcome::Problems => 1,
    };
    let command = format!("elfq {sample} {}", args.join(" "));
    assert!(
        status.allows(exit),
        "expected exit status {status}, but `{command}` ended with {outcome:?}, \
         exit status {exit}:\n{text}"
    );
    text
}

/// What `elfq SAMPLE ARGS --json` prints, run in this process, parsed.
///
/// It makes the checks of [`text_in_process`].
#[track_caller]
fn json_in_process(sample: &str, args: &[&str], status: impl Into<Exit>) -> Value {
    let args = [args, &["--json"]].concat();
    let text = text_in_process(sample, &args, status);
    match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => panic!(
            "`elfq {sample} {}` printed no JSON ({err}):\n{text}",
            args.join(" ")
        ),
    }
}

/// A row of the tree that `--tree` draws: how deep it is, counting from 0,
/// its ADDRESS cell, and the name after the branches.
struct Drawn {
    level: usize,
    address: String,
    name: String,
}

/// The rows of the tree in the table of `text`, as `--tree` draws it, in
/// the order of the lines. The tree must be no more than 9 levels deep, so
/// that no part of it is continued below the table.
fn drawn(text: &str) -> Vec<Drawn> {
    assert!(!text.contains(" (continued below)\n"), "{text}");
    let mut lines = text.lines();
    let Some(header) = lines.find(|line| line.starts_with("ADDRESS")) else {
        return Vec::new();
    };
    // The tree is in the last column, after cells that are all ASCII. Each
    // level is 3 columns.
    let column = header.rfind(' ').unwrap() + 1;
    lines
        .take_while(|line| !line.is_empty())
        .map(|line| {
            let tree = &line[column..];
            let name = tree.trim_start_matches(['│', '├', '└', '─', ' ']);
            Drawn {
                level: (tree.chars().count() - name.chars().count()) / 3,
                address: line.split(' ').next().unwrap().to_string(),
                name: name.to_string(),
            }
        })
        .collect()
}

/// The items of `kind` in `binary` that reach takes, by address, as SYMBOL
/// takes them, so that a stripped file's items are asked about the same
/// way: every data object, and every function whose code elfq can read.
fn symbols(binary: &Binary, kind: ItemKind) -> Vec<String> {
    binary
        .items
        .iter()
        .filter(|item| item.kind == kind && (kind == ItemKind::Data || item.size > 0))
        .map(|item| format!("{:#x}", item.address))
        .collect()
}

/// An item in an answer, told apart from the others by name and address.
fn item(item: &Value) -> (String, String) {
    (
        item["name"].as_str().unwrap().to_string(),
        item["address"].to_string(),
    )
}

#[test]
fn each_start_reaches_what_reach_start_counts() {
    let mut checked = 0;
    for sample in samples() {
        let starts = json_in_process(&sample, &["reach"], 0);
        for start in starts["starts"].as_array().unwrap() {
            let address = start["address"].as_str().unwrap();
            let reach = json_in_process(&sample, &["reach", address], 0);
            let context = format!("{sample} reach {address}");
            assert_eq!(item(&reach["symbol"]), item(start), "{context}");
            for key in ["code_bytes", "data_bytes", "complete"] {
                assert_eq!(reach[key], start[key], "{context}: {key}");
            }
            // The bytes in each block, without the sections that hold them.
            let mut blocks = reach["blocks"].clone();
            for block in blocks.as_array_mut().unwrap() {
                block.as_object_mut().unwrap().remove("sections");
            }
            assert_eq!(blocks, start["blocks"], "{context}");
            checked += 1;
        }
    }
    assert!(checked >= 90, "{checked}");
}

#[test]
fn reach_lists_a_tree_of_what_callees_and_retained_list() {
    let checked = on_each_sample(check_tree);
    assert!(checked >= 450, "{checked}");
}

/// Checks `reach` on every item of `sample` against `callees` and
/// `retained`, and returns how many items it checked.
fn check_tree(sample: &str) -> usize {
    let mut checked = 0;
    let binary = load_sample(sample);
    let symbols = [ItemKind::Function, ItemKind::Data].map(|kind| symbols(&binary, kind));
    for symbol in symbols.concat() {
        let reach = json_in_process(sample, &["reach", &symbol], 0);
        let context = format!("{sample} reach {symbol}");
        let items = reach["items"].as_array().unwrap();

        // A tree, each row after its parent, with SYMBOL first.
        assert_eq!(items[0]["id"], 0, "{context}");
        assert_eq!(item(&items[0]), item(&reach["symbol"]), "{context}");
        assert!(items[0]["parent"].is_null(), "{context}");
        for (id, row) in items.iter().enumerate().skip(1) {
            assert_eq!(row["id"], id, "{context}");
            let parent = row["parent"].as_u64().unwrap() as usize;
            assert!(parent < id, "{context}: {row}");
        }
        let reached: BTreeSet<(String, String)> = items.iter().map(item).collect();
        assert_eq!(reached.len(), items.len(), "{context}: an item twice");

        // The sizes add up, in all and by block.
        let bytes = |kind: &str| -> u64 {
            items
                .iter()
                .filter(|row| row["kind"] == kind)
                .map(|row| row["size"].as_u64().unwrap())
                .sum()
        };
        assert_eq!(reach["code_bytes"], bytes("function"), "{context}");
        assert_eq!(reach["data_bytes"], bytes("data"), "{context}");
        for (key, total) in ["code_bytes", "data_bytes"].map(|key| (key, &reach[key])) {
            let blocks: u64 = reach["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|block| block[key].as_u64().unwrap())
                .sum();
            assert_eq!(*total, blocks, "{context}: {key}");
        }

        // What SYMBOL uses itself hangs under it.
        let callees = json_in_process(sample, &["callees", &symbol], 0);
        let used: BTreeSet<(String, String)> = callees["callees"]
            .as_array()
            .unwrap()
            .iter()
            .map(|callee| item(&callee["item"]))
            .filter(|callee| *callee != item(&reach["symbol"]))
            .collect();
        let children: BTreeSet<(String, String)> = items
            .iter()
            .filter(|row| row["parent"] == 0)
            .map(item)
            .collect();
        assert_eq!(children, used, "{context}");

        // What removing SYMBOL would save is in what it reaches.
        let retained = json_in_process(sample, &["retained", &symbol], 0);
        for kept in retained["items"].as_array().unwrap() {
            assert!(reached.contains(&item(kept)), "{context}: {kept}");
        }

        check_drawn(sample, &symbol, items);
        checked += 1;
    }
    checked
}

/// Checks the tree that `reach SYMBOL --tree` draws against `items`, the
/// rows of the JSON: with `--all`, it has every one, each under its parent,
/// in the same order; without, its rows that sum up others, such as "4
/// more", count the rest, and none sums up a single item.
fn check_drawn(sample: &str, symbol: &str, items: &[Value]) {
    let context = format!("{sample} reach {symbol} --tree");
    let all = text_in_process(sample, &["reach", symbol, "--tree", "--all"], 0);
    let rows: Vec<Drawn> = drawn(&all)
        .into_iter()
        .filter(|row| row.name != "(outside any function)")
        .collect();
    if rows.is_empty() {
        assert_eq!(items.len(), 1, "{context}: {all}");
        return;
    }
    let mut levels: Vec<usize> = Vec::new();
    for row in items {
        let level = row["parent"]
            .as_u64()
            .map_or(0, |parent| levels[parent as usize] + 1);
        levels.push(level);
    }
    let expected = |(item, level): (&Value, usize)| {
        let address = item["address"].as_str().unwrap_or_default();
        let mark = if item["through_pointer"] == true {
            " (through a pointer)"
        } else {
            ""
        };
        (
            level,
            address.to_string(),
            format!("{}{mark}", item["name"].as_str().unwrap()),
        )
    };
    let expected: Vec<(usize, String, String)> = items.iter().zip(levels).map(expected).collect();
    let found: Vec<(usize, String, String)> = rows
        .into_iter()
        .map(|row| (row.level, row.address, row.name))
        .collect();
    assert_eq!(found, expected, "{context} --all");

    let capped = text_in_process(sample, &["reach", symbol, "--tree"], 0);
    let counted: usize = drawn(&capped)
        .iter()
        .filter(|row| row.name != "(outside any function)")
        .map(|row| match row.name.strip_suffix(" more") {
            Some(count) if row.address.is_empty() => {
                let count = count.parse().unwrap();
                assert!(count > 1, "{context}: {capped}");
                count
            }
            _ => 1,
        })
        .sum();
    assert_eq!(counted, items.len(), "{context}: {capped}");
}

#[test]
fn reach_draws_plt_stubs_and_imports_in_the_tree() {
    // Every function of x86-64 files, whose PLT stubs jump to imports.
    let check = |sample: &str| {
        let binary = load_sample(sample);
        for symbol in symbols(&binary, ItemKind::Function) {
            let reach = json_in_process(sample, &["reach", &symbol], 0);
            check_drawn(sample, &symbol, reach["items"].as_array().unwrap());
        }
    };
    std::thread::scope(|scope| {
        for sample in [
            SAMPLE,
            "testdata/bin/cpp-x86_64.elf",
            "testdata/bin/c-plt.so",
        ] {
            scope.spawn(move || check(sample));
        }
    });
}

#[test]
fn reach_to_lists_the_rows_that_callees_to_lists() {
    let checked = on_each_sample(check_to);
    assert!(checked >= 800, "{checked}");
}

/// Checks `reach SYMBOL --to RANGE` for every item of `sample`, with each
/// block of memory that the overview lists as RANGE, the range of a chip's
/// ROM that the far samples call, and the range of a function that code
/// reaches only through a pointer, which the code that takes its address
/// is outside of: against `reach SYMBOL`, whose items in RANGE it
/// must list, with the calls and jumps that elfq can't follow that may go
/// there, and `callees USER --to RANGE`, which must list each of its rows
/// whose USER is outside RANGE. Returns how many such rows it checked.
///
/// With these ranges, a veneer and its target are never both in RANGE when
/// the code that calls the veneer isn't; then reach lists the veneer too,
/// which `callees` doesn't (see `reach_to_lists_a_veneer_and_its_target`).
fn check_to(sample: &str) -> usize {
    let binary = load_sample(sample);
    let overview = json_in_process(sample, &[], 0);
    let mut ranges: Vec<String> = overview["memory"]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| {
            format!(
                "{}..{}",
                block["start"].as_str().unwrap(),
                block["end"].as_str().unwrap()
            )
        })
        .collect();
    if FAR_SAMPLES.contains(&sample) {
        ranges.push("0x1fff0000+64K".to_string());
    }
    let symbols = [ItemKind::Function, ItemKind::Data].map(|kind| symbols(&binary, kind));
    let wholes: Vec<(String, Value)> = symbols
        .concat()
        .into_iter()
        .map(|symbol| {
            let whole = json_in_process(sample, &["reach", &symbol], 0);
            (symbol, whole)
        })
        .collect();
    let pointed: BTreeSet<String> = wholes
        .iter()
        .flat_map(|(_, whole)| whole["items"].as_array().unwrap())
        .filter(|item| item["through_pointer"] == true)
        .map(|item| {
            let size = item["size"].as_u64().unwrap().max(1);
            format!("{}+{size}", item["address"].as_str().unwrap())
        })
        .collect();
    ranges.extend(pointed.into_iter().take(1));
    // Whether a call through a pointer may go to each range: when it holds
    // a function whose address is taken or stored, as `callers` lists them.
    let pointer: HashMap<&String, bool> = ranges
        .iter()
        .map(|range| {
            let callers = json_in_process(sample, &["callers", range], Exit::ZeroOrOne);
            (
                range,
                !callers["through_pointer"].as_array().unwrap().is_empty(),
            )
        })
        .collect();

    let mut callees: HashMap<(usize, String), Vec<Row>> = HashMap::new();
    let mut checked = 0;
    for (symbol, whole) in &wholes {
        for range in &ranges {
            let context = format!("{sample} reach {symbol} --to {range}");
            let to: AddressRange = range.parse().unwrap();
            let (outcome, text) = run_in_process(sample, &["reach", symbol, "--to", range]);
            let reach = json_in_process(sample, &["reach", symbol, "--to", range], Exit::ZeroOrOne);

            // The calls and jumps that elfq can't follow in what SYMBOL
            // reaches that may go to RANGE: those to an address in it, and
            // those through a pointer, when one may go there.
            let unresolved = |answer: &Value| -> Vec<((String, String), Value, Value)> {
                let unresolved = answer["unresolved"].as_array().unwrap();
                unresolved
                    .iter()
                    .map(|jump| {
                        let caller = item(&jump["caller"]);
                        (caller, jump["site"].clone(), jump["target"].clone())
                    })
                    .collect()
            };
            let expected: Vec<_> = unresolved(whole)
                .into_iter()
                .filter(|(_, _, target)| match target.as_str() {
                    Some(target) => to.contains(u64::from_str_radix(&target[2..], 16).unwrap()),
                    None => pointer[range],
                })
                .collect();
            let kept = unresolved(&reach);
            assert_eq!(kept, expected, "{context}: {text}");
            assert_eq!(reach["unreadable"], whole["unreadable"], "{context}");
            let complete = kept.iter().all(|(_, _, target)| !target.is_null())
                && whole["unreadable"].as_array().unwrap().is_empty();
            assert_eq!(reach["complete"], complete, "{context}");

            // The JSON: the items of `reach SYMBOL` in RANGE, those with
            // the calls and jumps it keeps, those on their chains, and
            // SYMBOL.
            let items = whole["items"].as_array().unwrap();
            let callers: BTreeSet<&(String, String)> =
                kept.iter().map(|(caller, _, _)| caller).collect();
            let in_to: Vec<bool> = items.iter().map(|item| is_in(item, to)).collect();
            let mut on_chain = in_to.clone();
            on_chain[0] = true;
            for (id, row) in items.iter().enumerate().rev() {
                on_chain[id] |= callers.contains(&item(row));
                if on_chain[id]
                    && let Some(parent) = row["parent"].as_u64()
                {
                    on_chain[parent as usize] = true;
                }
            }
            // Renumbered, so each row is compared by its parent's address.
            let parent = |items: &[Value], row: &Value| {
                let parent = row["parent"].as_u64()?;
                Some(items[parent as usize]["address"].to_string())
            };
            let expected: Vec<_> = items
                .iter()
                .zip(&in_to)
                .zip(&on_chain)
                .filter(|(_, on_chain)| **on_chain)
                .map(|((row, &in_to), _)| (item(row), Value::Bool(in_to), parent(items, row)))
                .collect();
            let listed = reach["items"].as_array().unwrap();
            let found: Vec<_> = listed
                .iter()
                .map(|row| (item(row), row["in_to"].clone(), parent(listed, row)))
                .collect();
            assert_eq!(found, expected, "{context}");
            let bytes = |kind: &str| -> u64 {
                listed
                    .iter()
                    .filter(|row| row["kind"] == kind && row["in_to"] == true)
                    .map(|row| row["size"].as_u64().unwrap())
                    .sum()
            };
            assert_eq!(reach["code_bytes"], bytes("function"), "{context}");
            assert_eq!(reach["data_bytes"], bytes("data"), "{context}");

            // The exit status, and the text: a row for each item in RANGE,
            // and one for each call or jump that elfq can't follow that may
            // go there.
            let found = in_to.iter().any(|&in_to| in_to)
                || !kept.is_empty()
                || !whole["unreadable"].as_array().unwrap().is_empty();
            let expected = if found {
                Outcome::Crossed
            } else {
                Outcome::Answered
            };
            assert_eq!(outcome, expected, "{context}: {text}");
            let shown = table(&text);
            let named: BTreeSet<(String, String)> = shown
                .iter()
                .filter(|row| !row.address.is_empty() && !row.item.starts_with('('))
                .map(|row| (row.item.clone(), row.address.clone()))
                .collect();
            let in_range: BTreeSet<(String, String)> = items
                .iter()
                .zip(&in_to)
                .filter(|(_, in_to)| **in_to)
                .map(|(row, _)| {
                    (
                        row["name"].as_str().unwrap().to_string(),
                        row["address"].as_str().unwrap().to_string(),
                    )
                })
                .collect();
            assert_eq!(named, in_range, "{context}: {text}");
            // The rows of the calls and jumps it keeps, each named by the
            // item it is in, or for a veneer's jump, by the item that calls
            // the veneer, and each of these rows by one of them.
            let users = |caller: &(String, String)| -> Vec<String> {
                let row = items.iter().find(|row| item(row) == *caller).unwrap();
                let parent = row["parent"].as_u64().map(|parent| &items[parent as usize]);
                [Some(row), parent]
                    .into_iter()
                    .flatten()
                    .map(|row| row["name"].as_str().unwrap().to_string())
                    .collect()
            };
            let unfollowed: Vec<&Row> = shown
                .iter()
                .filter(|row| row.item.starts_with('('))
                .collect();
            let same = |row: &Row, (caller, _, target): &((String, String), Value, Value)| {
                let (address, what) = match target.as_str() {
                    Some(target) => (target, "(outside any function)"),
                    None => ("", "(through a pointer)"),
                };
                (row.address.as_str(), row.item.as_str()) == (address, what)
                    && users(caller).contains(row.user.as_ref().unwrap())
            };
            for jump in &kept {
                let listed = unfollowed.iter().any(|row| same(row, jump));
                assert!(listed, "{context}: no row for {jump:?}: {text}");
            }
            for row in &unfollowed {
                let kept = kept.iter().any(|jump| same(row, jump));
                assert!(kept, "{context}: {row:?} is for nothing kept");
            }

            // Each row whose user is outside RANGE is a row of `callees USER
            // --to RANGE`, which names the user by its whole name, as the
            // row does: one of the items of that name, if there are several.
            for row in &shown {
                let Some(user) = &row.user else {
                    continue;
                };
                let users: Vec<usize> = (0..binary.items.len())
                    .filter(|&index| binary.items[index].name == *user)
                    .collect();
                assert!(!users.is_empty(), "{context}: no item is named {user}");
                let outside: Vec<usize> = users
                    .into_iter()
                    .filter(|&index| !binary.items[index].is_in(&[to]))
                    .collect();
                if outside.is_empty() {
                    continue;
                }
                let listed = outside.iter().any(|&index| {
                    let rows = callees.entry((index, range.clone())).or_insert_with(|| {
                        let address = format!("{:#x}", binary.items[index].address);
                        table(&text_in_process(
                            sample,
                            &["callees", &address, "--to", range],
                            1,
                        ))
                    });
                    rows.contains(row)
                });
                assert!(listed, "{context}: {row:?} isn't in callees --to {range}");
                checked += 1;
            }
        }
    }
    checked
}

/// Whether `item`, from an answer's JSON, has a byte in `range`, where an
/// item of unknown size counts as its first byte.
fn is_in(item: &Value, range: AddressRange) -> bool {
    let Some(address) = item["address"].as_str() else {
        return false;
    };
    let address = u64::from_str_radix(address.trim_start_matches("0x"), 16).unwrap();
    range.overlap(address, item["size"].as_u64().unwrap().max(1)) > 0
}

/// A row of the table of `reach --to` or `callees --to`: its cells, with
/// the name split from the user after "used by", if there is one.
#[derive(Debug, PartialEq)]
struct Row {
    address: String,
    size: String,
    how: String,
    at: String,
    item: String,
    user: Option<String>,
}

/// The rows of the table in `text`, whose columns are ADDRESS, SIZE, HOW,
/// AT, and the names.
fn table(text: &str) -> Vec<Row> {
    let mut lines = text.lines();
    let Some(header) = lines.find(|line| line.starts_with("ADDRESS")) else {
        return Vec::new();
    };
    // Every cell but the last is ASCII, SIZE is aligned right, and the
    // others left.
    let size_end = header.find("SIZE").unwrap() + "SIZE".len();
    let how = header.find("HOW").unwrap();
    let at = header.find("AT ").unwrap();
    let names = header.rfind(' ').unwrap() + 1;
    lines
        .take_while(|line| !line.is_empty())
        .map(|line| {
            let (item, user) = match line[names..].rsplit_once(", used by ") {
                Some((item, user)) => (item.to_string(), Some(user.to_string())),
                None => (line[names..].to_string(), None),
            };
            Row {
                address: line[..size_end].split(' ').next().unwrap().to_string(),
                size: line[..size_end].rsplit(' ').next().unwrap().to_string(),
                how: line[how..at].trim().to_string(),
                at: line[at..names].trim().to_string(),
                item,
                user,
            }
        })
        .collect()
}

#[test]
fn a_stripped_file_reaches_the_same_functions() {
    let checked = on_each_sample(check_twin);
    assert!(checked >= 280, "{checked}");
}

/// Checks that `reach` gives the same functions for every function of
/// `sample`'s stripped twin, if it has one, and returns how many functions
/// it checked.
fn check_twin(sample: &str) -> usize {
    // A stripped file's twin is the build without DWARF debug info, which
    // differs from it only in its symbols.
    let with_symbols = match sample.contains("-nodebug") {
        true => sample.to_string(),
        false => sample.replace(".elf", "-nodebug.elf"),
    };
    let stripped = with_symbols.replace("-nodebug", "-stripped");
    if !sample_path(&stripped).exists() {
        return 0;
    }
    // The functions as addresses and sizes, each with its parent's address,
    // since the stripped file names its items by address. Its data may
    // differ: without symbols, elfq can take data objects next to each
    // other for one.
    let tree = |sample: &str, function: &str| -> BTreeSet<[String; 3]> {
        let reach = json_in_process(sample, &["reach", function], 0);
        let items = reach["items"].as_array().unwrap();
        items
            .iter()
            .filter(|row| row["kind"] == "function")
            .map(|row| {
                let parent = &items[row["parent"].as_u64().unwrap_or(0) as usize];
                [&row["address"], &row["size"], &parent["address"]].map(Value::to_string)
            })
            .collect()
    };
    let found: BTreeSet<String> = symbols(&load_sample(&stripped), ItemKind::Function)
        .into_iter()
        .collect();
    let mut checked = 0;
    for function in symbols(&load_sample(&with_symbols), ItemKind::Function) {
        // A function that nothing calls has no name and no item in the
        // stripped file.
        if !found.contains(&function) {
            continue;
        }
        assert_eq!(
            tree(&stripped, &function),
            tree(&with_symbols, &function),
            "{stripped} reach {function}"
        );
        checked += 1;
    }
    checked
}

/// Runs `check` on each sample, each on a thread of its own.
///
/// The samples are [`C_AND_RUST_SAMPLES`] and [`FAR_SAMPLES`]. It adds up
/// what the checks return.
fn on_each_sample(check: fn(&str) -> usize) -> usize {
    std::thread::scope(|scope| {
        let threads: Vec<_> = C_AND_RUST_SAMPLES
            .into_iter()
            .chain(FAR_SAMPLES)
            .map(|sample| scope.spawn(move || check(sample)))
            .collect();
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .sum()
    })
}
