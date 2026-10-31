//! `startup`: what the reset code of each Cortex-M sample does before
//! `main`, the problems it finds there, its exit status, and the files it
//! refuses.

use std::path::Path;

use serde_json::{Value, json};

use crate::damaged::{self, SIZE, set_section_field};
use crate::support::{
    SAMPLE, TCM_ENTRY_MAIN_SAMPLE, TCM_SAMPLE, assert_output, command, elfq, samples,
};

/// [`TCM_SAMPLE`] with memcpy linked into ITCM, which Reset_Handler calls
/// to copy ITCM's code there.
const TRAP_SAMPLE: &str = "testdata/bin/c-cortex-m7-trap.elf";
/// [`TCM_SAMPLE`] with Reset_Handler linked into ITCM.
const RESET_ITCM_SAMPLE: &str = "testdata/bin/c-cortex-m7-reset-itcm.elf";
/// A reset handler that calls SystemInit, which calls code in ITCM, before
/// it copies ITCM's code there.
const SYSINIT_SAMPLE: &str = "testdata/bin/c-cortex-m7-sysinit.elf";
/// A reset handler that calls 32 functions, then one through a pointer,
/// after it sets up the memory: more steps than the table shows.
const STEPS_SAMPLE: &str = "testdata/bin/c-cortex-m7-steps.elf";
/// A reset handler in assembly that copies and zeroes in loops.
const LOOPS_SAMPLE: &str = "testdata/bin/c-cortex-m7-loops.elf";
/// A reset handler in C, built without optimization, that copies and
/// zeroes in loops.
const LOOPS_O0_SAMPLE: &str = "testdata/bin/c-cortex-m7-loops-O0.elf";
/// A reset handler that copies the sections that a table lists, in a loop
/// over its rows.
const TABLES_SAMPLE: &str = "testdata/bin/c-cortex-m7-tables.elf";
/// A reset handler that passes each row of tables of sections to a
/// function that copies or zeroes it, as MCUXpresso IDE's does.
const SECTIONS_SAMPLE: &str = "testdata/bin/c-cortex-m7-sections.elf";
/// A reset handler laid out as the Pico SDK's, whose table of sections to
/// copy ends with a zero word.
const PICO_SAMPLE: &str = "testdata/bin/c-cortex-m0-pico.elf";
/// A reset handler that copies .data with a function from the boot ROM,
/// which isn't in the file.
const ROM_SAMPLE: &str = "testdata/bin/c-cortex-m7-rom.elf";
/// A reset handler that calls a function that sets up the memory and
/// doesn't return, as Zephyr's does.
const NORETURN_SAMPLE: &str = "testdata/bin/c-cortex-m7-noreturn.elf";

/// Runs `elfq FILE startup` with `options`, expecting nothing on stderr,
/// and returns its exit status and what it printed.
fn startup(file: &str, options: &[&str]) -> (i32, String) {
    let output = elfq(&[&[file, "startup"], options].concat());
    assert!(output.stderr.is_empty(), "{output:?}");
    let status = output.status.code().unwrap();
    (status, String::from_utf8(output.stdout).unwrap())
}

/// Runs `elfq FILE startup` with `options`, expecting it to fail, and
/// returns what it printed on stderr.
fn refusal(file: &str, options: &[&str]) -> String {
    let output = elfq(&[&[file, "startup"], options].concat());
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    String::from_utf8(output.stderr).unwrap()
}

/// The answer to `elfq FILE startup --json`, and its exit status.
fn answer(file: &str) -> (i32, Value) {
    let (status, text) = startup(file, &["--json"]);
    (status, serde_json::from_str(&text).unwrap())
}

/// Saves a copy of `sample`, changed by `change`, as `name` in a folder for
/// the tests' files, and returns its path.
fn changed(sample: &str, name: &str, change: &dyn Fn(&mut Vec<u8>)) -> String {
    let mut data = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(sample)).unwrap();
    change(&mut data);
    damaged::save(name, &data)
}

/// The 4 bytes at `at`. The samples are little-endian.
fn word(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

fn set_word(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Where the program header of the load image for `address` is in the
/// 32-bit file `data`: of the segment there whose load address isn't its
/// address.
fn load_image_header(data: &[u8], address: u32) -> usize {
    let half = |at: usize| usize::from(u16::from_le_bytes([data[at], data[at + 1]]));
    // e_phoff, e_phentsize and e_phnum; and in a program header, p_vaddr
    // and p_paddr.
    let (headers, size, count) = (word(data, 0x1c) as usize, half(0x2a), half(0x2c));
    (0..count)
        .map(|n| headers + n * size)
        .find(|&at| word(data, at + 8) == address && word(data, at + 12) != address)
        .unwrap()
}

#[test]
fn firmware_that_sets_up_its_memory_before_main_has_no_problems() {
    let (status, text) = startup(TCM_SAMPLE, &[]);
    assert_eq!(status, 0);
    assert_output(
        &text,
        "\
No problems: Reset_Handler copies .itcm and .data, zeroes .bss, then calls main.

STEP  DOES  FROM        TO          SIZE  AT          WHAT
   1  copy  0x30000040  0x0          224  0x3000060e  .itcm, by memcpy
   2  copy  0x300009c0  0x20000000  1056  0x30000626  .data, by memcpy
   3  zero              0x20000420    48  0x3000063e  .bss, by memset
   4  call                                0x30000642  main

Not zeroed, which may be intended: .ocram_bss (tx_buffer).
",
    );
    // Without symbols, main is the last function that the reset handler
    // calls.
    let (status, text) = startup("testdata/bin/c-cortex-m7-stripped.elf", &[]);
    assert_eq!(status, 0);
    assert_output(
        &text,
        "\
No problems: entry copies .itcm and .data, zeroes .bss, then calls fn_0x30000570 (taken as main).

STEP  DOES  FROM        TO          SIZE  AT          WHAT
   1  copy  0x30000040  0x0          224  0x3000060e  .itcm, by fn_0x3000087c
   2  copy  0x300009c0  0x20000000  1056  0x30000626  .data, by fn_0x3000087c
   3  zero              0x20000420    48  0x3000063e  .bss, by fn_0x300007d8
   4  call                                0x30000642  fn_0x30000570 (taken as main)

Not zeroed, which may be intended: .ocram_bss.
",
    );
}

#[test]
fn a_copy_by_code_that_nothing_has_copied_yet() {
    // Reset_Handler calls memcpy, in ITCM, to copy ITCM's code: the
    // processor runs whatever ITCM holds at reset.
    let (status, text) = startup(TRAP_SAMPLE, &[]);
    assert_eq!(status, 1);
    assert_output(
        &text,
        "\
1 problem in what Reset_Handler does before it calls main:

  Step 1 calls memcpy at 0x10 in .itcm, which nothing has copied yet.

STEP  DOES  FROM        TO          SIZE  AT          WHAT
   1  copy  0x30000040  0x0          528  0x3000074e  .itcm, by memcpy via veneer
   2  copy  0x300009d8  0x20000000  1056  0x30000766  .data, by memcpy via veneer
   3  zero              0x20000420    48  0x3000077e  .bss, by memset
   4  call                                0x30000782  main

Steps 1 to 4 may never run, so .data and .bss may not be set up.
Not zeroed, which may be intended: .ocram_bss (tx_buffer).
",
    );
    let (status, text) = startup("testdata/bin/c-cortex-m7-trap-stripped.elf", &[]);
    assert_eq!(status, 1);
    assert_output(
        &text,
        "\
1 problem in what entry does before it calls fn_0x300006b0 (taken as main):

  Step 1 calls fn_0x10 in .itcm, which nothing has copied yet.

STEP  DOES  FROM        TO          SIZE  AT          WHAT
   1  copy  0x30000040  0x0          528  0x3000074e  .itcm, by fn_0x10 via veneer
   2  copy  0x300009d8  0x20000000  1056  0x30000766  .data, by fn_0x10 via veneer
   3  zero              0x20000420    48  0x3000077e  .bss, by fn_0x30000918
   4  call                                0x30000782  fn_0x300006b0 (taken as main)

Steps 1 to 4 may never run, so .data and .bss may not be set up.
Not zeroed, which may be intended: .ocram_bss.
",
    );
}

#[test]
fn a_call_before_the_copy_that_leads_to_code_that_nothing_has_copied_yet() {
    let (status, text) = startup(SYSINIT_SAMPLE, &[]);
    assert_eq!(status, 1);
    assert_output(
        &text,
        "\
1 problem in what Reset_Handler does before it calls main:

  Step 1, SystemInit, calls clock_setup at 0x0 in .itcm before step 2 copies .itcm.

STEP  DOES  FROM        TO          SIZE  AT          WHAT
   1  call                                0x30000038  SystemInit
   2  copy  0x30000008  0x0           16  0x30000042  .itcm, by memcpy
   3  copy  0x30000268  0x20000000     4  0x3000004e  .data, by memcpy
   4  zero              0x20000004     4  0x3000005a  .bss, by memset
   5  call                                0x3000005e  main

Steps 2 to 5 may never run, so .data and .bss may not be set up.

Try:
  elfq testdata/bin/c-cortex-m7-sysinit.elf reach SystemInit --to 0x0+16
",
    );
    let (_, answer) = answer(SYSINIT_SAMPLE);
    assert_eq!(answer["problems"][0]["chain"], json!(["SystemInit"]));
    assert_eq!(answer["problems"][0]["before_step"], 2);
    // Reset_Handler sets r0 for the copy of .itcm before it calls
    // SystemInit, which keeps it, as GCC knows; and main, which never
    // returns, is the last call, though memset leaves its first argument
    // in r0 for it.
    let (status, text) = startup("testdata/bin/c-cortex-m7-sysinit-stripped.elf", &[]);
    assert_eq!(status, 1);
    assert_output(
        &text,
        "\
1 problem in what entry does before it calls fn_0x3000001c (taken as main):

  Step 1, fn_0x30000018, calls fn_0x0 in .itcm before step 2 copies .itcm.

STEP  DOES  FROM        TO          SIZE  AT          WHAT
   1  call                                0x30000038  fn_0x30000018
   2  copy  0x30000008  0x0           16  0x30000042  .itcm, by fn_0x30000128
   3  copy  0x30000268  0x20000000     4  0x3000004e  .data, by fn_0x30000128
   4  zero              0x20000004     4  0x3000005a  .bss, by fn_0x30000084
   5  call                                0x3000005e  fn_0x3000001c (taken as main)

Steps 2 to 5 may never run, so .data and .bss may not be set up.

Try:
  elfq testdata/bin/c-cortex-m7-sysinit-stripped.elf reach fn_0x30000018 --to 0x0+16
",
    );
}

#[test]
fn a_reset_handler_in_memory_that_nothing_has_copied_yet() {
    let (status, text) = startup(RESET_ITCM_SAMPLE, &[]);
    assert_eq!(status, 1);
    assert_output(
        &text,
        "\
1 problem in what Reset_Handler does before it calls main:

  Reset_Handler runs at 0x10 in .itcm, which nothing has copied yet.

STEP  DOES  FROM        TO          SIZE  AT    WHAT
   1  copy  0x30000040  0x0          352  0x26  .itcm, by memcpy via veneer
   2  copy  0x300009c0  0x20000000  1056  0x3e  .data, by memcpy via veneer
   3  zero              0x20000420    48  0x56  .bss, by memset via veneer
   4  call                                0x5a  main via veneer

Steps 1 to 4 may never run, so .data and .bss may not be set up.
Not zeroed, which may be intended: .ocram_bss (tx_buffer).
",
    );
}

#[test]
fn code_in_ram_that_nothing_copies() {
    // far.c's reset handler calls code in .ram, through veneers, and
    // nothing copies .ram; two of those functions call into a ROM outside
    // the file, which elfq can't follow.
    let (status, text) = startup("testdata/bin/far-gnu-m4-nodebug.elf", &[]);
    assert_eq!(status, 1);
    assert_output(
        &text,
        "\
1 problem in what Reset_Handler does before it calls flash_calls_rom (taken as main):

  Steps 1 to 3, 5, 7 and 8 call ram_calls_flash, ram_tails_flash, ram_calls_rom, ram_tails_rom, ram_long_calls_flash and ram_leaf in .ram before anything copies .ram.

STEP  DOES    FROM  TO  SIZE  AT          WHAT
   1  call                    0x30000112  ram_calls_flash via veneer
   2  call                    0x30000116  ram_tails_flash via veneer
   3  call                    0x3000011a  ram_calls_rom via veneer
   4  unread                  0x46        a call to 0x1fff0000 in ram_calls_rom, outside the file
   5  call                    0x3000011e  ram_tails_rom via veneer
   6  unread                  0x3000011e  a call to 0x1fff0000 in ram_tails_rom, outside the file
   7  call                    0x30000122  ram_long_calls_flash via veneer
   8  call                    0x30000126  flash_calls_ram
   9  call                    0x3000012a  flash_calls_rom (taken as main)

Steps 2 to 9 may never run, so .bss may not be set up.
",
    );
}

#[test]
fn a_reset_handler_that_copies_and_zeroes_nothing() {
    let (status, text) = startup("testdata/bin/c-vfp.elf", &[]);
    assert_eq!(status, 1);
    assert_output(
        &text,
        "\
2 problems in what Reset_Handler does before it calls run (taken as main):

  Nothing copies .data, 8 bytes at 0x20000000 stored at 0x8000140.
  Nothing zeroes .bss, 16 bytes at 0x20000008.

STEP  DOES  FROM  TO  SIZE  AT         WHAT
   1  call                  0x800013a  run (taken as main)
",
    );
    // This one is the program: it ends in a loop that it runs for good.
    let (status, text) = startup("testdata/bin/c-softfloat-nodebug.elf", &[]);
    assert_eq!(status, 1);
    assert_output(
        &text,
        "\
1 problem in what Reset_Handler does before its endless loop at 0x800001e:

  Nothing zeroes .bss, 56 bytes at 0x20000000.
",
    );
}

#[test]
fn code_that_elfq_cant_follow_may_be_a_problem() {
    // The boot ROM's function, which elfq can't read, may copy .data. The
    // one after it takes what memset would to zero .bss, so it zeroes it.
    let (status, text) = startup(ROM_SAMPLE, &[]);
    assert_eq!(status, 1);
    assert_output(
        &text,
        "\
1 problem in what Reset_Handler does before it calls main:

  Nothing copies .data, 4 bytes at 0x20000000 stored at 0x300000b8, unless step 2 (a call through r2) does.

STEP  DOES    FROM        TO          SIZE  AT          WHAT
   1  copy    0x30000008  0x0           12  0x30000056  .itcm, by a loop in Reset_Handler
   2  unread                                0x30000076  a call through r2 in Reset_Handler
   3  zero                0x20000004     4  0x30000084  .bss, by a call through r3 in Reset_Handler
   4  call                                  0x30000086  main
",
    );
    // That problem may not be one. The first function may zero .bss too.
    let (status, answer) = answer(ROM_SAMPLE);
    assert_eq!(status, 1);
    assert_eq!(answer["problems_certain"], 0);
    assert_eq!(answer["complete"], false);
    assert_eq!(answer["problems"][0]["kind"], "can't follow");
    assert_eq!(answer["problems"][0]["certain"], false);
    assert_eq!(answer["problems"][0]["sections"], json!([".data"]));
    assert_eq!(answer["steps"][1]["may_set_up"], json!([".data", ".bss"]));
    assert_eq!(answer["sections"][1]["status"], "unknown");
}

#[test]
fn loops_that_copy_and_zero_are_steps() {
    // Loops of conditional instructions in assembly, the second going on
    // from where the first left its source, and one that counts down.
    let (status, text) = startup(LOOPS_SAMPLE, &[]);
    assert_eq!(status, 0);
    assert_output(
        &text,
        "\
No problems: Reset_Handler copies .itcm and .data, zeroes .bss, then calls main.

STEP  DOES  FROM        TO          SIZE  AT          WHAT
   1  copy  0x30000098  0x0           12  0x30000016  .itcm, by a loop in Reset_Handler
   2  copy  0x300000a4  0x20000000    12  0x30000028  .data, by a loop in Reset_Handler
   3  zero              0x2000000c    12  0x3000003a  .bss, by a loop in Reset_Handler
   4  call                                0x3000003e  main
",
    );
    // Loops in C without optimization, whose pointers are on the stack, the
    // zeroing going on from where the copy left its destination.
    let (status, text) = startup(LOOPS_O0_SAMPLE, &[]);
    assert_eq!(status, 0);
    assert_output(
        &text,
        "\
No problems: Reset_Handler copies .itcm and .data, zeroes .bss, then calls main.

STEP  DOES  FROM        TO          SIZE  AT          WHAT
   1  copy  0x30000008  0x0           24  0x3000007e  .itcm, by a loop in Reset_Handler
   2  copy  0x300000f0  0x20000000    12  0x300000a0  .data, by a loop in Reset_Handler
   3  zero              0x2000000c    12  0x300000b4  .bss, by a loop in Reset_Handler
   4  call                                0x300000be  main
",
    );
    let (_, answer) = answer(LOOPS_SAMPLE);
    assert_eq!(answer["complete"], true);
    assert_eq!(answer["steps"][0]["how"], "loop");
    assert_eq!(answer["steps"][0]["function"]["name"], "Reset_Handler");
}

#[test]
fn tables_of_sections_are_read_row_by_row() {
    // A loop over each table, as CMSIS's startup code in C goes, with a
    // loop inside for each row.
    let (status, text) = startup(TABLES_SAMPLE, &[]);
    assert_eq!(status, 0);
    assert_output(
        &text,
        "\
No problems: Reset_Handler copies .itcm and .data, zeroes .bss, then calls main.

STEP  DOES  FROM        TO          SIZE  AT          WHAT
   1  copy  0x30000008  0x0           12  0x30000076  .itcm, by a loop in Reset_Handler, from table 0x30000014
   2  copy  0x300000e8  0x20000000     4  0x30000076  .data, by a loop in Reset_Handler, from table 0x30000020
   3  zero              0x20000004     4  0x300000ba  .bss, by a loop in Reset_Handler, from table 0x3000002c
   4  call                                0x300000cc  main
",
    );
    // A function for each kind of row, which MCUXpresso IDE's startup code
    // calls with the row: the step is the call's.
    let (status, text) = startup(SECTIONS_SAMPLE, &[]);
    assert_eq!(status, 0);
    assert_output(
        &text,
        "\
No problems: ResetISR copies .itcm and .data, zeroes .bss and .ocram_bss, then calls main.

STEP  DOES  FROM        TO          SIZE  AT          WHAT
   1  copy  0x30000008  0x0           12  0x300000ac  .itcm, by data_init, from table 0x30000014
   2  copy  0x300000f0  0x20000000     4  0x300000ac  .data, by data_init, from table 0x30000020
   3  zero              0x20000004     4  0x300000cc  .bss, by bss_init, from table 0x3000002c
   4  zero              0x20240000    64  0x300000cc  .ocram_bss, by bss_init, from table 0x30000034
   5  call                                0x300000d8  main
",
    );
    let (_, answer) = answer(SECTIONS_SAMPLE);
    assert_eq!(answer["steps"][0]["how"], "call");
    assert_eq!(answer["steps"][0]["function"]["name"], "data_init");
    assert_eq!(answer["steps"][0]["table"], "0x30000014");
    assert_eq!(answer["steps"][4]["table"], Value::Null);
    // A table that ends with a zero word, as the Pico SDK's, whose rows a
    // loop copies that is code of the reset handler's own; the one after
    // core 1 goes back to the boot ROM. The row of .scratch_y copies no
    // bytes, and its call to the loop is no step.
    let (status, text) = startup(PICO_SAMPLE, &[]);
    assert_eq!(status, 0);
    assert_output(
        &text,
        "\
No problems: _reset_handler copies .data and .scratch_x, zeroes .bss, then calls main.

STEP  DOES  FROM        TO          SIZE  AT          WHAT
   1  copy  0x100001e8  0x20000000    20  0x10000132  .data, by a loop in _reset_handler, from table 0x10000160
   2  copy  0x100001fc  0x20040000    28  0x10000132  .scratch_x, by a loop in _reset_handler, from table 0x1000016c
   3  zero              0x20000014    32  0x10000140  .bss, by a loop in _reset_handler
   4  call                                0x10000148  runtime_init
   5  call                                0x1000014c  main

The vector table at 0x10000100 gives _reset_handler for reset, not the entry point _entry_point.
At 0x10000124 elfq takes the branch, since the other way ends in a branch through r2 at 0x1000011c.
",
    );
    // Without symbols, a function finder takes the loop's test, which the
    // call goes to, for a function of its own.
    let (status, text) = startup("testdata/bin/c-cortex-m0-pico-stripped.elf", &[]);
    assert_eq!(status, 0);
    assert!(
        text.contains("  1  copy  0x100001e8  0x20000000    20  0x10000132  .data, by fn_0x10000156, from table 0x10000160\n"),
        "{text}"
    );
    // The walk goes through the loop over the table each time, the empty
    // row's and the zero word's too.
    let output = elfq(&[PICO_SAMPLE, "debug", "walk"]);
    assert!(output.status.success(), "{output:?}");
    let walk = String::from_utf8(output.stdout).unwrap();
    assert!(
        walk.contains("loop back from 0x10000136 over the 7 rows above, runs 4 times, each time walked, reading rows at 0x10000160 +12n\n"),
        "{walk}"
    );
}

#[test]
fn the_walk_tells_how_often_each_loop_runs_and_what_it_stores() {
    // Each loop's row says how many times its head runs, the last time
    // being the one that leaves, and what each store in it stores those
    // times after the first, which the row above shows.
    let output = elfq(&[LOOPS_SAMPLE, "debug", "walk"]);
    assert!(output.status.success(), "{output:?}");
    assert_output(
        &String::from_utf8(output.stdout).unwrap(),
        "\
from Reset_Handler (0x30000008), stack pointer 0x20040000, main 0x30000060

      SITE  DEPTH  WHAT
0x30000016      0  store 0x4380f04f at 0x0, 4 bytes, loaded from 0x30000098 at 0x30000012
0x3000000e      0  loop back from 0x3000001a over the row above, runs 4 times; 0x30000016 stores 4 bytes at 0x0 +4n 3 times, from 0x30000098 +4n
0x30000028      0  store 0x1 at 0x20000000, 4 bytes, loaded from 0x300000a4 at 0x30000024
0x30000020      0  loop back from 0x3000002c over the row above, runs 4 times; 0x30000028 stores 4 bytes at 0x20000000 +4n 3 times, from 0x300000a4 +4n
0x3000003a      0  store 0x0 at 0x20000014, 4 bytes
0x30000038      0  loop back from 0x3000003c over the row above, runs 3 times; 0x3000003a stores 4 bytes at 0x20000014 -4n 3 times, of 0x0
0x3000003e      0  call main (0x30000060) (r0=0x0 r1=0x2000000c r2=0x0 r3=0x2000000c)
0x3000003e         ends: reaches main
",
    );
}

#[test]
fn setup_in_a_call_that_doesnt_return_is_what_that_call_does() {
    // prep copies and zeroes, then starts the program's threads, which
    // call main through a pointer.
    let (status, text) = startup(NORETURN_SAMPLE, &[]);
    assert_eq!(status, 0);
    assert_output(
        &text,
        "\
No problems: Reset_Handler calls prep, which copies .itcm and .data, zeroes .bss and doesn't return.

STEP  DOES  FROM        TO          SIZE  AT          WHAT
   1  call                                0x30000092  prep
   2  copy  0x30000008  0x0           12  0x3000004e  .itcm, by memcpy, in prep
   3  copy  0x30000278  0x20000000     8  0x3000005a  .data, by memcpy, in prep
   4  zero              0x20000008     4  0x30000066  .bss, by memset, in prep
",
    );
}

#[test]
fn a_copy_from_elsewhere_or_of_part_of_a_section_is_a_problem() {
    // The file says that it stores .data 4 bytes lower than where
    // Reset_Handler copies it from.
    let elsewhere = changed(TCM_SAMPLE, "c-cortex-m7-elsewhere.elf", &|data| {
        let at = load_image_header(data, 0x2000_0000) + 12;
        let stored = word(data, at) - 4;
        set_word(data, at, stored);
    });
    let (status, text) = startup(&elsewhere, &[]);
    assert_eq!(status, 1);
    assert!(
        text.starts_with(
            "1 problem in what Reset_Handler does before it calls main:\n\n  Step 2 copies .data \
             from 0x300009c0, but the file stores it at 0x300009bc.\n\n"
        ),
        "{text}"
    );
    let problem = &answer(&elsewhere).1["problems"][0];
    assert_eq!(
        (&problem["kind"], &problem["from"]),
        (&json!("copied from elsewhere"), &json!("0x300009c0"))
    );
    // .data, its load image and .bss each 16 bytes larger than what
    // Reset_Handler sets up.
    let partly = changed(TCM_SAMPLE, "c-cortex-m7-partly.elf", &|data| {
        set_section_field(data, ".data", SIZE, 1056 + 16);
        set_section_field(data, ".bss", SIZE, 48 + 16);
        let at = load_image_header(data, 0x2000_0000);
        set_word(data, at + 16, 1056 + 16);
        set_word(data, at + 20, 1056 + 16);
    });
    let (status, text) = startup(&partly, &[]);
    assert_eq!(status, 1);
    assert!(
        text.starts_with(
            "2 problems in what Reset_Handler does before it calls main:\n\n  Step 2 copies 1056 \
             of .data's 1072 bytes.\n  Step 3 zeroes 48 of .bss's 64 bytes.\n\n"
        ),
        "{text}"
    );
    let (_, partly) = answer(&partly);
    let statuses: Vec<_> = partly["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|section| {
            (
                section["name"].as_str().unwrap(),
                section["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        statuses,
        [
            (".itcm", "copied"),
            (".data", "partly copied"),
            (".bss", "partly zeroed"),
            (".ocram_bss", "not zeroed"),
        ]
    );
}

#[test]
fn the_table_shows_the_first_30_steps_and_all_shows_every_step() {
    let (status, text) = startup(STEPS_SAMPLE, &[]);
    // The hook may zero .ocram_bss, which needn't be zeroed: a note, not a
    // problem.
    assert_eq!(status, 0);
    let rows: String = (4..=29)
        .map(|step| {
            format!(
                "{step:>5}  call                                {:#x}  init_{}\n",
                0x3000_01de + 4 * (step - 4),
                step - 4
            )
        })
        .collect();
    assert_output(
        &text,
        &format!(
            "\
No problems: Reset_Handler copies .itcm and .data, zeroes .bss, then calls main.

 STEP  DOES  FROM        TO          SIZE  AT          WHAT
    1  copy  0x30000008  0x0           12  0x300001c2  .itcm, by memcpy
    2  copy  0x30000470  0x20000000     4  0x300001ce  .data, by memcpy
    3  zero              0x20000004     4  0x300001da  .bss, by memset
{rows}30-37                                                  8 more steps; --all lists them

elfq can't follow step 36 (a call through r3), which may zero .ocram_bss.
"
        ),
    );
    let (status, text) = startup(STEPS_SAMPLE, &["--all"]);
    assert_eq!(status, 0);
    // The lead, a blank line, the header, a row for each of the 37 steps, a
    // blank line and the note.
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 42, "{text}");
    assert_eq!(
        lines[38],
        "  36  unread                                0x30000262  a call through r3 in Reset_Handler"
    );
    // The JSON gives every step, without --all.
    let (status, answer) = answer(STEPS_SAMPLE);
    assert_eq!(status, 0);
    assert_eq!(answer["steps"].as_array().unwrap().len(), 37);
    let hook = &answer["steps"][35];
    assert_eq!(
        (
            &hook["does"],
            &hook["how"],
            &hook["register"],
            &hook["may_set_up"]
        ),
        (
            &json!("unread"),
            &json!("register call"),
            &json!("r3"),
            &json!([".ocram_bss"])
        )
    );
    assert_eq!(
        (
            &answer["complete"],
            &answer["problems_certain"],
            &answer["notes"][0]["kind"]
        ),
        (&json!(false), &json!(0), &json!("can't tell"))
    );
}

#[test]
fn startup_reads_a_pipe_and_extended_section_numbers() {
    use std::io::Write;
    use std::process::Stdio;

    let (_, expected) = startup(TCM_SAMPLE, &[]);
    // A pipe, which can be read only once.
    let mut child = command()
        .args(["/dev/stdin", "startup"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let data = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(TCM_SAMPLE)).unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(&data));
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_output(&String::from_utf8(output.stdout).unwrap(), &expected);
    // e_shnum 0, with the number of sections in the first section header's
    // sh_size, as in a file with more sections than e_shnum holds.
    let extended = changed(TCM_SAMPLE, "c-cortex-m7-extended.elf", &|data| {
        let count = u16::from_le_bytes([data[0x30], data[0x31]]);
        data[0x30..0x32].fill(0);
        let first = word(data, 0x20) as usize;
        set_word(data, first + 0x14, u32::from(count));
    });
    assert_output(&startup(&extended, &[]).1, &expected);
}

#[test]
fn a_vector_table_whose_reset_handler_isnt_the_entry_point() {
    let (status, text) = startup(TCM_ENTRY_MAIN_SAMPLE, &[]);
    assert_eq!(status, 0);
    assert!(
        text.ends_with(
            "\n\nThe vector table at 0x30000000 gives Reset_Handler for reset, not the entry point \
             main.\nNot zeroed, which may be intended: .ocram_bss (tx_buffer).\n"
        ),
        "{text}"
    );
}

#[test]
fn the_json_gives_the_steps_the_sections_and_the_problems() {
    let (status, answer) = answer(TRAP_SAMPLE);
    assert_eq!(status, 1);
    let memcpy = json!({
        "name": "memcpy",
        "kind": "function",
        "section": ".itcm",
        "address": "0x10",
        "size": 308,
        "size_estimated": false,
    });
    assert_eq!(answer["entry"]["name"], "Reset_Handler");
    assert_eq!(
        (
            &answer["main"]["name"],
            &answer["main_at"],
            &answer["main_from"]
        ),
        (&json!("main"), &json!("0x300006b0"), &json!("symbol"))
    );
    assert_eq!(
        (&answer["complete"], &answer["problems_certain"]),
        (&json!(true), &json!(1))
    );
    let steps = answer["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 4);
    assert_eq!(
        steps[0],
        json!({
            "step": 1,
            "site": "0x3000074e",
            "does": "copy",
            "how": "call",
            "register": null,
            "function": memcpy,
            "veneer": "0x300009c0",
            "inside": answer["entry"],
            "from": "0x30000040",
            "to": "0x0",
            "size": 528,
            "sections": [".itcm"],
            "table": null,
            "may_set_up": [],
        })
    );
    assert_eq!(
        answer["sections"][0],
        json!({
            "name": ".itcm",
            "start": "0x0",
            "end": "0x210",
            "size": 528,
            "load_address": "0x30000040",
            "status": "not reached",
            "step": 1,
            "bytes": 0,
        })
    );
    assert_eq!(
        answer["problems"],
        json!([{
            "kind": "runs before copy",
            "certain": true,
            "steps": [1],
            "sections": [".itcm"],
            "items": [memcpy],
            "chain": [],
            "before_step": 1,
            "bytes": null,
            "from": null,
            "sites": ["0x3000074e"],
            "text": "Step 1 calls memcpy at 0x10 in .itcm, which nothing has copied yet.",
        }])
    );
    assert_eq!(
        answer["notes"][1],
        json!({
            "kind": "not zeroed",
            "sections": [".ocram_bss"],
            "items": [],
            "text": "Not zeroed, which may be intended: .ocram_bss (tx_buffer).",
        })
    );
    // memcpy must stay out of ITCM, as must the reset handler.
    assert_eq!(
        answer["keep_out"],
        json!([{"section": ".itcm", "items": [answer["entry"], memcpy]}])
    );
    // One row per line.
    let (_, text) = startup(TRAP_SAMPLE, &["--json"]);
    assert!(
        text.contains("\"steps\":[\n{\"step\":1,") && text.contains("},\n{\"step\":2,"),
        "{text}"
    );
}

#[test]
fn keep_out_names_what_runs_before_each_copy_of_code() {
    let names = |row: &Value| -> Vec<String> {
        (row["items"].as_array().unwrap().iter())
            .map(|item| item["name"].as_str().unwrap().to_string())
            .collect()
    };
    // SystemInit calls clock_setup, in ITCM, before the copy, and memcpy
    // does the copy: each would run from ITCM too early. .data holds no
    // code.
    let (_, sysinit) = answer(SYSINIT_SAMPLE);
    let rows = sysinit["keep_out"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["section"], ".itcm");
    assert_eq!(
        names(&rows[0]),
        ["Reset_Handler", "SystemInit", "clock_setup", "memcpy"]
    );
    // prep holds the copy, as it calls memcpy, then memset and
    // start_threads after it, so only prep and memcpy run before.
    let (_, noreturn) = answer(NORETURN_SAMPLE);
    assert_eq!(
        names(&noreturn["keep_out"][0]),
        ["Reset_Handler", "prep", "memcpy"]
    );
    // data_init does the copy itself, with its own loop.
    let (_, sections) = answer(SECTIONS_SAMPLE);
    assert_eq!(names(&sections["keep_out"][0]), ["ResetISR", "data_init"]);
    // Nothing copies .ram, so nothing that runs before main may be in it.
    let (_, far) = answer("testdata/bin/far-gnu-m4-nodebug.elf");
    let row = &far["keep_out"][0];
    assert_eq!(row["section"], ".ram");
    assert_eq!(names(row).first().unwrap(), "Reset_Handler");
    assert!(names(row).contains(&"ram_leaf".to_string()), "{row}");
    // In every sample, the reset handler comes first, and the functions
    // that a row lists in its own section are those that a problem says
    // run from it before its copy.
    for sample in samples() {
        let name = sample.trim_start_matches("testdata/bin/");
        if name.contains("x86_64") || name == "c-deep.elf" || name == "c-plt.so" {
            continue;
        }
        let (_, answer) = answer(&sample);
        for row in answer["keep_out"].as_array().unwrap() {
            let section = &row["section"];
            let items = row["items"].as_array().unwrap();
            assert_eq!(items[0], answer["entry"], "{sample}: {row}");
            let mut inside: Vec<&Value> = (items.iter())
                .filter(|item| &item["section"] == section)
                .collect();
            let mut early: Vec<&Value> = (answer["problems"].as_array().unwrap().iter())
                .filter(|p| p["kind"] == "runs before copy" && p["sections"][0] == *section)
                .flat_map(|p| p["items"].as_array().unwrap())
                .collect();
            let address = |item: &&Value| item["address"].as_str().unwrap().to_string();
            inside.sort_by_key(address);
            early.sort_by_key(address);
            assert_eq!(inside, early, "{sample}: {row}");
        }
    }
}

#[test]
fn every_arm_sample_gets_an_answer_and_the_exit_status_it_means() {
    let problems = [
        "c-cortex-m7-reset-itcm",
        "c-cortex-m7-rom",
        "c-cortex-m7-sysinit",
        "c-cortex-m7-trap",
        "c-softfloat",
        "c-strtod",
        "c-vfp",
        "far-",
    ];
    for sample in samples() {
        let name = sample.trim_start_matches("testdata/bin/");
        if name.contains("x86_64") || name == "c-deep.elf" || name == "c-plt.so" {
            continue;
        }
        let expected = if problems.iter().any(|prefix| name.starts_with(prefix)) {
            1
        } else {
            0
        };
        let (status, text) = startup(&sample, &[]);
        assert_eq!(status, expected, "{sample}: {text}");
        // The JSON gives the same exit status, and as many problems as
        // the text names.
        let (status, answer) = answer(&sample);
        assert_eq!(status, expected, "{sample}");
        let problems = answer["problems"].as_array().unwrap().len();
        let lead = text.lines().next().unwrap();
        match problems {
            0 => assert!(lead.starts_with("No problems: "), "{sample}: {lead}"),
            1 => assert!(lead.starts_with("1 problem "), "{sample}: {lead}"),
            n => assert!(
                lead.starts_with(&format!("{n} problems ")),
                "{sample}: {lead}"
            ),
        }
        // --all lifts caps that these samples don't reach, but for
        // STEPS_SAMPLE's.
        if !text.contains("--all lists them") {
            assert_eq!(startup(&sample, &["--all"]), (status, text), "{sample}");
        }
    }
}

#[test]
fn startup_reads_only_cortex_m_firmware() {
    for sample in samples() {
        if sample.contains("x86_64") || sample.ends_with("c-deep.elf") || sample.ends_with(".so") {
            assert_output(
                &refusal(&sample, &[]),
                &format!(
                    "error: {sample} is x86-64 code; startup reads the reset code of Cortex-M \
                     firmware\n"
                ),
            );
        }
    }
    // Copies of the firmware with their headers changed: e_type, e_machine,
    // the first program header's p_type, and e_shoff, e_shnum and
    // e_shstrndx.
    let object = changed(TCM_SAMPLE, "c-cortex-m7-object.elf", &|data| data[0x10] = 1);
    let library = changed(TCM_SAMPLE, "c-cortex-m7-library.elf", &|data| {
        data[0x10] = 3
    });
    let aarch64 = changed(TCM_SAMPLE, "c-cortex-m7-aarch64.elf", &|data| {
        data[0x12] = 183
    });
    let loaded = changed(TCM_SAMPLE, "c-cortex-m7-interpreter.elf", &|data| {
        let at = word(data, 0x1c) as usize;
        set_word(data, at, 3);
    });
    let dynamic = changed(TCM_SAMPLE, "c-cortex-m7-dynamic.elf", &|data| {
        let at = word(data, 0x1c) as usize;
        set_word(data, at, 2);
    });
    let no_sections = changed(TCM_SAMPLE, "c-cortex-m7-no-section-headers.elf", &|data| {
        data[0x20..0x24].fill(0);
        data[0x30..0x34].fill(0);
    });
    for (path, what) in [
        (&object, "an object file, not a linked program"),
        (&library, "a shared library"),
        (&aarch64, "64-bit Arm code"),
        (&loaded, "a program that an operating system loads"),
        (&dynamic, "a program that an operating system loads"),
    ] {
        assert_output(
            &refusal(path, &[]),
            &format!(
                "error: {path} is {what}; startup reads the reset code of Cortex-M firmware\n"
            ),
        );
    }
    assert_output(
        &refusal(&no_sections, &["--json"]),
        &format!(
            "error: {no_sections} has no section headers, so startup can't tell what to copy and \
             zero\n"
        ),
    );
    // With e_shnum 0, as in a file with too many sections for it, and
    // e_shoff too large to add to.
    let huge = changed(SAMPLE, "c-x86_64-huge-shoff.elf", &|data| {
        data[0x28..0x30].copy_from_slice(&u64::MAX.to_le_bytes());
        data[0x3c..0x3e].fill(0);
    });
    assert_output(
        &refusal(&huge, &[]),
        &format!(
            "error: {huge} is x86-64 code; startup reads the reset code of Cortex-M firmware\n"
        ),
    );
    assert_output(
        &refusal("Cargo.toml", &[]),
        "error: cannot load Cargo.toml: not an ELF file\n",
    );
    assert_output(
        &refusal("missing.elf", &[]),
        "error: cannot read missing.elf: No such file or directory (os error 2)\n",
    );
}

#[test]
fn startup_takes_no_tree_or_range() {
    assert_output(
        &refusal(TCM_SAMPLE, &["--tree"]),
        "error: only the overview, top, retained, why, reach SYMBOL and inlined SYMBOL take --tree\n",
    );
    assert_output(
        &refusal(TCM_SAMPLE, &["--json", "--tree"]),
        "error: --tree draws trees in the text, so it doesn't go with --json\n",
    );
    assert_output(
        &refusal(TCM_SAMPLE, &["--range", "0+4"]),
        "error: only the overview, top and retained take --range\n",
    );
}
