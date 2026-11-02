//! `debug items` and `debug walk`, the hidden commands.
//!
//! They list what elfq found, and print only text. The tests that
//! `debug walk` lists the walk that `startup` reads are in startup.rs,
//! beside the samples they run on.

use crate::support::{SAMPLE, TCM_SAMPLE, assert_output, stderr, stdout};

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
fn debug_commands_print_only_text() {
    assert_eq!(
        stderr(&[SAMPLE, "debug", "items", "--json"]),
        "error: the debug commands print only text\n"
    );
}
