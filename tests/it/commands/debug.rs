//! `debug items`, `debug walk` and `debug matches`, the hidden commands.
//!
//! They list what elfq found: the first two as text, and `debug matches`
//! as JSON. The tests that `debug walk` lists the walk that `startup`
//! reads are in startup.rs, beside the samples they run on.

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
fn debug_items_and_walk_refuse_json() {
    for command in ["items", "walk"] {
        assert_eq!(
            stderr(&[SAMPLE, "debug", command, "--json"]),
            "error: the debug commands print only text\n"
        );
    }
}

#[test]
fn debug_matches_lists_what_each_pattern_matches_as_json() {
    // A line for each match: the patterns in the order given, then each
    // one's items in address order. So write@plt comes last, though its
    // address is the lowest, since `write*` comes second.
    assert_output(
        &stdout(&[SAMPLE, "debug", "matches", "on_*", "write*"]),
        concat!(
            r#"{"pattern":"on_*","name":"on_timer","kind":"function","section":".text","address":"0x1260","size":11,"size_estimated":false,"matched_by":"name","matched_as":"on_timer"}"#,
            "\n",
            r#"{"pattern":"on_*","name":"on_data","kind":"function","section":".text","address":"0x1270","size":11,"size_estimated":false,"matched_by":"name","matched_as":"on_data"}"#,
            "\n",
            r#"{"pattern":"on_*","name":"on_button","kind":"function","section":".text","address":"0x1650","size":19,"size_estimated":false,"matched_by":"name","matched_as":"on_button"}"#,
            "\n",
            r#"{"pattern":"write*","name":"write@plt","kind":"function","section":".plt.sec","address":"0x1070","size":16,"size_estimated":false,"matched_by":"name","matched_as":"write@plt"}"#,
            "\n",
        ),
    );
    // A pattern that matches nothing has no line.
    assert_output(&stdout(&[SAMPLE, "debug", "matches", "On_*"]), "");
    // Each argument must be a pattern, checked before any line.
    assert_output(
        &stderr(&[SAMPLE, "debug", "matches", "on_*", "process_frame"]),
        "error: `process_frame` isn't a pattern: it has no * that stands for any text\n",
    );
}
