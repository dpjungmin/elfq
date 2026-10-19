//! `--json`: the same answers as the text, in a shape scripts can read.

use serde_json::{Value, json};

use elfq::binary::{ItemKind, Source};

use crate::support::{DEEP_SAMPLE, LIBRARY_SAMPLE, SAMPLE, TCM_SAMPLE, command, elfq, load_sample};

/// Runs elfq with `--json`, expecting success, and parses what it printed.
fn elfq_json(args: &[&str]) -> Value {
    let output = command().args(args).arg("--json").output().unwrap();
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
                "section": ".text",
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
    // An import has no section, address or size in this file.
    let callees = elfq_json(&[SAMPLE, "callees", "_start"]);
    assert_eq!(
        callees["callees"][1]["item"],
        json!({
            "name": "__libc_start_main",
            "kind": "import",
            "section": null,
            "address": null,
            "size": null,
            "size_estimated": false
        })
    );
    // Each item names the section that holds it, so that scripts can tell
    // data in .bss from data in .data.
    let retained = elfq_json(&[SAMPLE, "retained", "process_frame"]);
    let sections: Vec<(&str, &str)> = retained["shares"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            let name = item["name"].as_str().unwrap();
            (name, item["section"].as_str().unwrap())
        })
        .collect();
    assert_eq!(
        sections,
        [
            ("__stack_chk_fail@plt", ".plt.sec"),
            ("dispatch_event", ".text"),
            ("frames_bad", ".bss"),
            ("frames_ok", ".bss"),
            ("log_msg", ".text"),
            ("stats", ".data")
        ]
    );
}

#[test]
fn each_row_is_a_line() {
    // The first line holds the answer up to its first list, and each row of
    // a list has a line of its own, so that grep finds a row.
    let output = elfq(&[SAMPLE, "callers", "log_msg", "--json"]);
    let text = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].starts_with(r#"{"symbol":{"name":"log_msg","#),
        "{text}"
    );
    assert!(lines[0].ends_with(r#""callers":["#), "{text}");
    assert_eq!(
        lines[4],
        r#"{"item":{"name":"process_frame","kind":"function","section":".text","#.to_string()
            + r#""address":"0x1700","size":532,"size_estimated":false},"how":"call","#
            + r#""sites":["0x18ee","0x1908"]},"#
    );
    for line in &lines[1..6] {
        let row: Value = serde_json::from_str(line.trim_end_matches(',')).unwrap();
        assert!(row["item"]["name"].is_string(), "{line}");
    }
    assert_eq!(lines[6..], [r#"],"complete":true}"#]);

    // An empty list takes no line.
    let output = elfq(&[SAMPLE, "retained", "debug_dump", "--json"]);
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
}

#[test]
fn a_shared_library_has_the_same_callers_as_the_program() {
    // Each caller and how it calls, and whether that is every caller.
    let callers = |sample: &str, function: &str| {
        let answer = elfq_json(&[sample, "callers", function]);
        let callers: Vec<(Value, Value)> = answer["callers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|caller| (caller["item"]["name"].clone(), caller["how"].clone()))
            .collect();
        (callers, answer["complete"].clone())
    };
    let library = load_sample("c-x86_64.so");
    let functions = library
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Function && item.source == Source::SymbolTable);
    let mut checked = 0;
    for function in functions {
        // In the program, `_start` hands `main` to the C library.
        if function.name == "main" {
            continue;
        }
        assert_eq!(
            callers(LIBRARY_SAMPLE, &function.name),
            callers(SAMPLE, &function.name),
            "{}",
            function.name
        );
        checked += 1;
    }
    assert!(checked > 20, "{checked}");
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
fn ranges_list_what_crosses_their_edge() {
    let output = elfq(&[TCM_SAMPLE, "callees", "0x0+256K", "--json"]);
    // Something leaves the range.
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let callees: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        callees["ranges"],
        json!([{ "start": "0x0", "end": "0x40000" }])
    );
    assert_eq!(
        names(&callees["callees"]),
        [
            "crc_table",
            "memcpy",
            "app_tick",
            "fir_coeffs",
            "systick_count"
        ]
    );
    let memcpy = &callees["callees"][1];
    assert_eq!(memcpy["caller"]["name"], "apply_filter");
    assert_eq!(memcpy["how"], "call");
    assert_eq!(memcpy["veneer"]["name"], "__memcpy_veneer");
    assert_eq!(memcpy["sites"], json!(["0xbc"]));
    assert_eq!(callees["callees"][0]["how"], "address taken");
    assert_eq!(callees["callees"][0]["veneer"], Value::Null);
    assert_eq!(callees["unresolved"], json!([]));
    assert_eq!(callees["unreadable"], json!([]));
    assert_eq!(callees["complete"], true);

    // Data that holds no pointers: nothing leaves, exit status 0.
    let dtcm = elfq_json(&[TCM_SAMPLE, "callees", "0x20000000+256K"]);
    assert_eq!(dtcm["callees"], json!([]));
    assert_eq!(dtcm["complete"], true);

    let output = elfq(&[SAMPLE, "callers", "0x1260+0x20", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let callers: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(names(&callers["callers"]), ["main", "main"]);
    assert_eq!(callers["callers"][0]["callee"]["name"], "on_timer");
    assert_eq!(callers["callers"][0]["how"], "address taken");
    assert_eq!(names(&callers["through_pointer"]), ["on_timer", "on_data"]);
    assert_eq!(callers["complete"], false);
}

/// The rows of a range check's JSON, by address rather than by name, so
/// that a stripped file gives the same: (item, caller, how, veneer, sites),
/// and for what elfq can't follow (target, caller, how, veneer, site).
fn rows_by_address(answer: &Value) -> Vec<[String; 5]> {
    let address = |item: &Value| item["address"].as_str().unwrap_or("").to_string();
    let mut rows: Vec<[String; 5]> = answer["callees"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            [
                address(&row["item"]),
                address(&row["caller"]),
                row["how"].to_string(),
                address(&row["veneer"]),
                row["sites"].to_string(),
            ]
        })
        .collect();
    rows.extend(answer["unresolved"].as_array().unwrap().iter().map(|row| {
        [
            row["target"].to_string(),
            address(&row["caller"]),
            row["how"].to_string(),
            address(&row["veneer"]),
            row["site"].to_string(),
        ]
    }));
    rows.sort();
    rows
}

#[test]
fn ranges_follow_calls_through_veneers() {
    // c/far.c, built with the veneers that GNU ld and LLD add, all but
    // two of LLD's (see testdata/README.md).
    // Cortex-M0 has no tail call this far, and code built for it with
    // -mpure-code makes counter's address a byte at a time, which elfq
    // doesn't follow.
    for name in [
        "gnu-m4",
        "gnu-m4-pure",
        "gnu-m4-pic",
        "gnu-m0",
        "lld-m4",
        "lld-m4-pic",
        "lld-m0",
        "lld-m0-pure",
    ] {
        let path = format!("testdata/bin/far-{name}-nodebug.elf");
        let output = elfq(&[&path, "callees", "0x0+64K", "--json"]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let callees: Value = serde_json::from_slice(&output.stdout).unwrap();
        let mut rows: Vec<(&str, &str, &str, bool)> = callees["callees"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["item"]["name"].as_str().unwrap(),
                    row["caller"]["name"].as_str().unwrap(),
                    row["how"].as_str().unwrap(),
                    !row["veneer"].is_null(),
                )
            })
            .collect();
        rows.retain(|row| row.0 != "counter");
        let tail_call = if name.contains("m0") {
            "call"
        } else {
            "tail call"
        };
        assert_eq!(
            rows,
            [
                ("flash_far", "ram_long_calls_flash", "call", false),
                ("flash_tail", "ram_tails_flash", tail_call, true),
                ("flash_work", "ram_calls_flash", "call", true),
            ],
            "{name}"
        );
        // The ROM routine has no item, so elfq can't follow the calls, and
        // can't tell whether the tail call goes to a function.
        let jump = if name.contains("m0") { "call" } else { "jump" };
        let unresolved: Vec<(&str, &str, &str, bool)> = callees["unresolved"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["caller"]["name"].as_str().unwrap(),
                    row["how"].as_str().unwrap(),
                    row["target"].as_str().unwrap(),
                    row["veneer"]["name"]
                        .as_str()
                        .unwrap()
                        .contains("rom_routine"),
                )
            })
            .collect();
        assert_eq!(
            unresolved,
            [
                ("ram_calls_rom", "call", "0x1fff0000", true),
                ("ram_tails_rom", jump, "0x1fff0000", true),
            ],
            "{name}"
        );
        assert_eq!(callees["complete"], false);

        // Without symbols, the veneers are known by their code, and the
        // rows are the same.
        let path = format!("testdata/bin/far-{name}-stripped.elf");
        let output = elfq(&[&path, "callees", "0x0+64K", "--json"]);
        let stripped: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            rows_by_address(&stripped),
            rows_by_address(&callees),
            "{name}"
        );
    }
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

    let tree = elfq_json(&["testdata/bin/rust-cortex-m4.elf", "inlined", "memcpy"]);
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
    assert_eq!(tree_names(&retained["keeps"]), ["crc_table"]);

    let retained = elfq_json(&[TCM_SAMPLE, "retained", "process_frame", "--range", "0+224"]);
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
    let answer = elfq_json(&[DEEP_SAMPLE, "retained", "chain1"]);
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
    let output = elfq(&[SAMPLE, "debug", "items", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: the debug commands print only text\n"
    );
}
