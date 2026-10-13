//! `--format json`: the same answers as the text, in a shape scripts can
//! read.

use std::process::Command;

use serde_json::{Value, json};

const SAMPLE: &str = "testdata/bin/c-x86_64.elf";

/// Runs elfq with `--format json`, expecting success, and parses what it
/// printed.
fn elfq_json(args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_elfq"))
        .args(args)
        .args(["--format", "json"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The names of the items in a list of objects that each have an `item`, or
/// are items.
fn names(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.get("item").unwrap_or(entry)["name"].as_str().unwrap())
        .collect()
}

#[test]
fn items_have_the_same_keys_everywhere() {
    // An item, and an answer with nothing in it.
    assert_eq!(
        elfq_json(&[SAMPLE, "retained", "debug_dump"]),
        json!({
            "symbol": {
                "name": "debug_dump",
                "kind": "function",
                "address": "0x1aa0",
                "size": 29,
                "size_estimated": false
            },
            "reachable": false,
            "range": null,
            "retained": 29,
            "keeps": [],
            "shares": [],
            "indirect_calls": 0
        })
    );
    // An import has no address or size in this file.
    let callees = elfq_json(&[SAMPLE, "callees", "_start"]);
    assert_eq!(
        callees["callees"][1]["item"],
        json!({
            "name": "__libc_start_main",
            "kind": "import",
            "address": null,
            "size": null,
            "size_estimated": false
        })
    );
}

#[test]
fn callers_and_callees_list_every_use() {
    let callers = elfq_json(&[SAMPLE, "callers", "log_msg"]);
    assert_eq!(callers["symbol"]["name"], "log_msg");
    assert_eq!(
        names(&callers["callers"]),
        [
            "app_tick",
            "debug_dump",
            "on_button",
            "process_frame",
            "report"
        ]
    );
    assert_eq!(callers["callers"][2]["how"], "tail call");
    assert_eq!(callers["callers"][3]["sites"], json!(["0x18ee", "0x1908"]));
    assert_eq!(callers["complete"], true);
    // Unlike the HOW column, which says "uses" for both, JSON tells reading
    // data apart from loading its address.
    let cleanup = elfq_json(&[
        "testdata/bin/rust-x86_64-nodebug.elf",
        "callers",
        "std::rt::cleanup::CLEANUP",
    ]);
    assert_eq!(cleanup["callers"][0]["how"], "access");
    assert_eq!(cleanup["callers"][1]["how"], "address taken");
    // Calls through a pointer to on_button don't show.
    assert_eq!(
        elfq_json(&[SAMPLE, "callers", "on_button"])["complete"],
        false
    );

    // The jump through the handler table goes where only the running
    // program knows.
    let callees = elfq_json(&[SAMPLE, "callees", "dispatch_event"]);
    assert_eq!(names(&callees["callees"]), ["handlers"]);
    assert_eq!(
        callees["unresolved"],
        json!([{ "site": "0x16ad", "how": "jump", "target": null }])
    );
    assert_eq!(callees["complete"], false);
}

#[test]
fn inlined_lists_every_function_and_the_whole_tree() {
    let list = elfq_json(&[SAMPLE, "inlined"]);
    assert_eq!(list["inlined_bytes"], 420);
    assert_eq!(list["code_bytes"], 2557);
    assert_eq!(
        list["functions"][1],
        json!({ "name": "append", "bytes": 150, "copies": 6 })
    );

    let tree = elfq_json(&[
        "testdata/bin/rust-cortex-m4.elf",
        "inlined",
        "--in",
        "memcpy",
    ]);
    assert_eq!(tree["function"]["name"], "compiler_builtins::mem::memcpy");
    assert_eq!(tree["inlined_bytes"], 712);
    let copy_forward = &tree["inlined"][0];
    assert_eq!(
        copy_forward["name"],
        "compiler_builtins::mem::impls::copy_forward"
    );
    assert_eq!(
        copy_forward["inlined"][0]["name"],
        "compiler_builtins::mem::impls::copy_forward::copy_forward_misaligned_words"
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
fn a_range_limits_the_counts_to_it() {
    let tcm_sample = "testdata/bin/c-cortex-m7.elf";
    let overview = elfq_json(&[tcm_sample, "--range", "0x0..0xe0"]);
    assert_eq!(overview["range"], json!({"start": "0x0", "end": "0xe0"}));
    assert_eq!(overview["functions"], 6);
    assert_eq!(overview["data_objects"], 0);
    assert_eq!(overview["bytes"], 224);
    assert_eq!(overview["imports"], Value::Null);
    assert_eq!(overview["references"], Value::Null);
    assert_eq!(overview["memory"].as_array().unwrap().len(), 4);

    let top = elfq_json(&[tcm_sample, "top", "--range", "0x0..0xe0"]);
    assert_eq!(top["range"], json!({"start": "0x0", "end": "0xe0"}));
    assert_eq!(top["bytes"], 224);
    // Only items that keep some of the range's bytes.
    assert_eq!(
        tree_names(&top["items"]),
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

    let retained = elfq_json(&[
        tcm_sample,
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
    assert_eq!(tree_names(&retained["keeps"]), ["crc_table"]);

    let retained = elfq_json(&[tcm_sample, "retained", "process_frame", "--range", "0+224"]);
    assert_eq!(retained["retained"], 200);
    assert_eq!(
        tree_names(&retained["keeps"]),
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

/// The names of the items in a tree from `top` or `retained`, each followed
/// by those it keeps.
fn tree_names(items: &Value) -> Vec<&str> {
    let mut names = Vec::new();
    for item in items.as_array().unwrap() {
        names.push(item["name"].as_str().unwrap());
        names.extend(tree_names(&item["keeps"]));
    }
    names
}

#[test]
fn top_has_every_item_in_the_file() {
    let top = elfq_json(&[SAMPLE, "top"]);
    // What the tree keeps and what nothing reaches add up to the file.
    let kept: u64 = top["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["retained"].as_u64().unwrap())
        .sum();
    let unreachable: u64 = top["unreachable"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["size"].as_u64().unwrap())
        .sum();
    assert_eq!(top["bytes"], 4021);
    assert_eq!(kept + unreachable, 4021);
    assert_eq!(tree_names(&top["items"]).len(), 65);
    assert_eq!(top["items"][0]["name"], "_start");
    assert_eq!(
        names(&top["unreachable"]),
        ["debug_dump", ".rodata@0x2056", "_IO_stdin_used"]
    );
}

#[test]
fn retained_has_what_symbol_keeps_as_a_tree() {
    let retained = elfq_json(&[SAMPLE, "retained", "process_frame"]);
    assert_eq!(retained["reachable"], true);
    assert_eq!(retained["retained"], 2068);
    let crc32 = &retained["keeps"][0];
    assert_eq!(crc32["name"], "crc32");
    assert_eq!(crc32["retained"], 1083);
    assert_eq!(crc32["through_pointer"], false);
    assert_eq!(crc32["keeps"][0]["name"], "crc_table");
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
    // The tree holds every item that only process_frame leads to.
    assert_eq!(
        tree_names(&retained["keeps"]),
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
    let literal = &retained["keeps"][3];
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
    assert_eq!(unreachable["keeps"], json!([]));
    assert_eq!(unreachable["shares"], json!([]));
}

#[test]
fn deep_trees_are_whole() {
    // The text continues a tree deeper than 9 levels below it; JSON nests
    // all of it.
    let answer = elfq_json(&["testdata/bin/c-deep.elf", "retained", "chain1"]);
    let mut chain = Vec::new();
    let mut keeps = &answer["keeps"];
    while let [item] = &keeps.as_array().unwrap()[..] {
        chain.push(item["name"].as_str().unwrap());
        keeps = &item["keeps"];
    }
    let expected: Vec<String> = (2..=20).map(|n| format!("chain{n}")).collect();
    assert_eq!(chain, expected);
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
    assert_eq!(why["on_every_chain"]["name"], "main");
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
    assert_eq!(start["on_every_chain"], Value::Null);
}

#[test]
fn debug_commands_print_only_text() {
    let output = Command::new(env!("CARGO_BIN_EXE_elfq"))
        .args([SAMPLE, "debug", "items", "--format", "json"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: the debug commands print only text\n"
    );
}
