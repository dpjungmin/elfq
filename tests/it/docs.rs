//! The docs, checked against elfq: each code block whose first line is
//! `$ elfq ...` must show what elfq prints for it, and design.md's JSON
//! section must name every key that elfq's JSON has.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::support::{
    SAMPLE, TCM_INLINED_SAMPLE, TCM_MOVED_SAMPLE, TCM_SAMPLE, assert_output, elfq,
};

/// The docs that can hold examples, relative to the crate's root.
const DOCS: [&str; 3] = ["README.md", "docs/design.md", "docs/examples.md"];

#[test]
fn the_examples_in_the_docs_show_what_elfq_prints() {
    let mut examples = 0;
    for doc in DOCS {
        let path = format!("{}/{doc}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap();
        // Every other piece between fences is a code block, which starts
        // with the rest of its opening fence's line, such as `json`.
        for block in text.split("```").skip(1).step_by(2) {
            let (_, block) = block.split_once('\n').unwrap();
            let Some(example) = block.strip_prefix("$ elfq ") else {
                continue;
            };
            let (command, expected) = example.split_once('\n').unwrap();
            // Shown with the output of a test that fails.
            println!("{doc}: $ elfq {command}");
            let args: Vec<&str> = command.split_whitespace().collect();
            let output = elfq(&args);
            assert!(output.stderr.is_empty(), "{output:?}");
            assert_output(&String::from_utf8(output.stdout).unwrap(), expected);
            examples += 1;
        }
    }
    // Well under the number of examples in the docs, so that a change in
    // how blocks are written can't make this test check nothing.
    assert!(examples >= 10, "found {examples} examples");
}

/// Answers that between them have every key that elfq's JSON has. A key
/// shows only in an object that is there, so for each key that holds a
/// list, one of them has an entry in it, which the test checks.
const JSON_ANSWERS: [&[&str]; 22] = [
    &[TCM_SAMPLE],
    &[SAMPLE, "top"],
    &[SAMPLE, "retained", "process_frame"],
    &[SAMPLE, "why", "log_msg"],
    &[SAMPLE, "why", "parse_value"],
    &[SAMPLE, "inlined"],
    &[SAMPLE, "inlined", "process_frame"],
    &[SAMPLE, "callers", "log_msg"],
    &[TCM_SAMPLE, "callers", "Default_Handler"],
    &[SAMPLE, "callees", "dispatch_event"],
    &[SAMPLE, "callers", "0x1000+4K"],
    &[SAMPLE, "callees", "0x1000+4K"],
    &[TCM_SAMPLE, "callers", "0x0+256K"],
    &[TCM_SAMPLE, "callees", "0x0+256K"],
    &[
        TCM_SAMPLE,
        "callers",
        "0x20000000+256K",
        "--from",
        "0x0+256K",
    ],
    &[TCM_SAMPLE, "callees", "0x0+256K", "--to", "0x30000000+16M"],
    &[TCM_SAMPLE, "reach", "SysTick_Handler"],
    &[SAMPLE, "reach"],
    &[TCM_SAMPLE, "reach"],
    &[
        TCM_SAMPLE,
        "reach",
        "SysTick_Handler",
        "--to",
        "0x30000000+16M",
    ],
    &[TCM_SAMPLE, "diff", TCM_MOVED_SAMPLE],
    &[TCM_SAMPLE, "diff", TCM_INLINED_SAMPLE],
];

#[test]
fn design_md_names_every_json_key() {
    let path = format!("{}/docs/design.md", env!("CARGO_MANIFEST_DIR"));
    let design = std::fs::read_to_string(&path).unwrap();
    let (_, section) = design.split_once("\n### JSON\n").unwrap();
    let (section, _) = section.split_once("\n### ").unwrap();
    // Whether each list had an entry in any answer.
    let mut lists = BTreeMap::new();
    for args in JSON_ANSWERS {
        let output = elfq(&[args, &["--json"]].concat());
        // Range checks exit with 1 when something crosses the ranges, and
        // diff when the memory differs.
        assert!(matches!(output.status.code(), Some(0 | 1)), "{output:?}");
        let mut keys = BTreeSet::new();
        add_keys(
            &serde_json::from_slice(&output.stdout).unwrap(),
            &mut keys,
            &mut lists,
        );
        for key in keys {
            assert!(
                section.contains(&format!("`{key}`")),
                "design.md's JSON section doesn't name `{key}`, from elfq {}",
                args.join(" ")
            );
        }
    }
    for (key, had_entries) in lists {
        assert!(had_entries, "no answer has an entry in `{key}`");
    }
}

/// Adds the keys of every object in `value` to `keys`, and notes in `lists`
/// whether each list it holds has entries.
fn add_keys(value: &Value, keys: &mut BTreeSet<String>, lists: &mut BTreeMap<String, bool>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                keys.insert(key.clone());
                if let Value::Array(values) = value {
                    *lists.entry(key.clone()).or_default() |= !values.is_empty();
                }
                add_keys(value, keys, lists);
            }
        }
        Value::Array(values) => values.iter().for_each(|value| add_keys(value, keys, lists)),
        _ => {}
    }
}
