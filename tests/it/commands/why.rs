//! `why`: the chains from the roots that keep SYMBOL.
//!
//! The tests check them as text and as JSON, where they start, and what
//! `why` says when nothing reaches SYMBOL.

use serde_json::{Value, json};

use crate::support::{SAMPLE, assert_output, elfq_json, names, stdout};

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
fn why_has_the_chain_through_every_user() {
    let why = elfq_json(&[SAMPLE, "why", "log_msg"]);
    // Each chain as one line: every item, and how it is used.
    let chains: Vec<String> = why["chains"]
        .as_array()
        .unwrap()
        .iter()
        .map(|chain| {
            let steps: Vec<String> = chain
                .as_array()
                .unwrap()
                .iter()
                .map(|step| {
                    let name = step["item"]["name"].as_str().unwrap();
                    format!("{name} ({})", step["how"].as_str().unwrap())
                })
                .collect();
            steps.join(" > ")
        })
        .collect();
    assert_eq!(
        chains,
        [
            "_start (entry point) > main (address taken) > app_tick (call) > log_msg (call)",
            "_start (entry point) > main (address taken) > on_button (address taken) > log_msg \
             (tail call)",
            "_start (entry point) > main (address taken) > process_frame (call) > log_msg (call)",
            "_start (entry point) > main (address taken) > report (call) > log_msg (call)",
        ]
    );
    assert_eq!(why["root"], Value::Null);
    assert_eq!(names(&why["unreachable_users"]), ["debug_dump"]);
    assert_eq!(why["users_it_keeps"], json!([]));
    assert_eq!(why["on_every_chain"]["name"], "main");
    // parse_list calls parse_value back, so it doesn't keep it.
    let parse_value = elfq_json(&[SAMPLE, "why", "parse_value"]);
    assert_eq!(names(&parse_value["users_it_keeps"]), ["parse_list"]);
    assert_eq!(parse_value["unreachable_users"], json!([]));
    // JSON keeps its words where the HOW column shortened them: Arm code
    // takes the address of `stats` to use it, and a handler is listed in
    // the vector table.
    const FIRMWARE: &str = "testdata/bin/c-cortex-m4.elf";
    assert_eq!(
        elfq_json(&[FIRMWARE, "why", "stats"])["chains"][0][3]["how"],
        "address taken"
    );
    assert_eq!(
        elfq_json(&[FIRMWARE, "why", "app_tick"])["chains"][0][0]["how"],
        "listed in the vector table"
    );
    assert_eq!(
        elfq_json(&[FIRMWARE, "why", "SysTick_Handler"])["root"],
        "listed in the vector table"
    );

    // Nothing needs to keep where the program starts.
    let start = elfq_json(&[SAMPLE, "why", "_start"]);
    assert_eq!(start["root"], "entry point");
    assert_eq!(start["chains"], json!([]));
    assert_eq!(start["users_it_keeps"], json!([]));
    assert_eq!(start["on_every_chain"], Value::Null);
}
