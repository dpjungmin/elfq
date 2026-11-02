//! `--json`: the same answers as the text, in a shape scripts can read.

use serde_json::{Value, json};

use crate::support::{SAMPLE, elfq_json, names, run, text_rows};

#[test]
fn items_have_the_same_keys_everywhere() {
    // An item, and an answer with nothing in it. SYMBOL's item also has its
    // other names.
    assert_eq!(
        elfq_json(&[SAMPLE, "retained", "debug_dump"]),
        json!({
            "symbol": {
                "name": "debug_dump",
                "kind": "function",
                "section": ".text",
                "address": "0x1aa0",
                "size": 29,
                "size_estimated": false,
                "aliases": []
            },
            "reachable": false,
            "range": null,
            "own": 29,
            "load_image": 0,
            "retained": 29,
            "items": [],
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
    let text = run(&[SAMPLE, "callers", "log_msg", "--json"]).printed(0);
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
    let text = run(&[SAMPLE, "retained", "debug_dump", "--json"]).printed(0);
    assert_eq!(text.lines().count(), 1, "{text}");
}

#[test]
fn symbol_lists_its_other_names() {
    let m4 = "testdata/bin/c-cortex-m4.elf";
    let handlers = [
        "BusFault_Handler",
        "DebugMon_Handler",
        "HardFault_Handler",
        "MemManage_Handler",
        "NMI_Handler",
        "PendSV_Handler",
        "SVC_Handler",
        "UsageFault_Handler",
    ];
    for command in ["callers", "callees", "retained", "why"] {
        let answer = elfq_json(&[m4, command, "NMI_Handler"]);
        assert_eq!(answer["symbol"]["name"], "Default_Handler", "{command}");
        assert_eq!(answer["symbol"]["aliases"], json!(handlers), "{command}");
    }
    // A mangled symbol is the other name of its readable one.
    let inlined = elfq_json(&["testdata/bin/cpp-x86_64.elf", "inlined", "Gain::run"]);
    assert_eq!(
        inlined["function"]["aliases"],
        json!(["_ZN3app4Gain3runEPsm"])
    );
    // Items elsewhere in the answer don't list theirs.
    let callers = elfq_json(&[m4, "callers", "NMI_Handler"]);
    assert_eq!(callers["callers"][0]["item"].get("aliases"), None);
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
    // The rows of the tree, each with the row it was inlined into.
    let copy_forward = &tree["inlined"][0];
    assert_eq!(
        copy_forward["name"],
        "compiler_builtins::mem::impls::copy_forward"
    );
    assert_eq!(copy_forward["id"], 0);
    assert_eq!(copy_forward["parent"], Value::Null);
    let misaligned = &tree["inlined"][1];
    assert_eq!(
        misaligned["name"],
        "compiler_builtins::mem::impls::copy_forward::copy_forward_misaligned_words"
    );
    assert_eq!(misaligned["parent"], 0);
    let rows = tree["inlined"].as_array().unwrap();
    for (id, row) in rows.iter().enumerate() {
        assert_eq!(row["id"], id);
        assert!(
            row["parent"]
                .as_u64()
                .is_none_or(|parent| parent < id as u64)
        );
    }
    // Only copy_forward was inlined into memcpy itself.
    let top = rows.iter().filter(|row| row["parent"].is_null()).count();
    assert_eq!(top, 1);
    // The rows come in the order of the text's.
    assert_eq!(
        names(&tree["inlined"]),
        text_rows(&["testdata/bin/rust-cortex-m4.elf", "inlined", "memcpy"])
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

#[test]
fn debug_commands_print_only_text() {
    assert_eq!(
        run(&[SAMPLE, "debug", "items", "--json"]).error(),
        "error: the debug commands print only text\n"
    );
}
