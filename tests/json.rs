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
        elfq_json(&[SAMPLE, "exclusive", "debug_dump"]),
        json!({
            "symbol": {
                "name": "debug_dump",
                "kind": "function",
                "address": "0x1aa0",
                "size": 29,
                "size_estimated": false
            },
            "reachable": false,
            "bytes": 0,
            "items": []
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

    // The jump through the handler table goes where only the running
    // program knows.
    let callees = elfq_json(&[SAMPLE, "callees", "dispatch_event"]);
    assert_eq!(names(&callees["callees"]), ["handlers"]);
    assert_eq!(
        callees["unresolved"],
        json!([{ "site": "0x16ad", "how": "jump", "target": null }])
    );
}

#[test]
fn exclusive_lists_every_item_largest_first() {
    let exclusive = elfq_json(&[SAMPLE, "exclusive", "process_frame"]);
    assert_eq!(exclusive["reachable"], true);
    assert_eq!(exclusive["bytes"], 1536);
    assert_eq!(
        names(&exclusive["items"]),
        [
            "crc_table",
            "apply_filter",
            "filter_tap",
            "crc32",
            "decode_header",
            "fir_coeffs",
            ".rodata@0x2012",
            ".rodata@0x201e"
        ]
    );
    // A string literal without a symbol has an estimated size.
    assert_eq!(exclusive["items"][6]["size_estimated"], true);
    assert_eq!(exclusive["items"][6]["through_pointer"], false);
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
fn commands_without_json_say_so() {
    let output = Command::new(env!("CARGO_BIN_EXE_elfq"))
        .args([SAMPLE, "top", "--format", "json"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: `--format json` works with callers, callees, exclusive and inlined so far, not \
         with top\n"
    );
}
