//! What `reach` lists, checked for every function of the firmware samples
//! against `retained` and `callees`, and between each stripped file and its
//! twin with symbols.

use std::collections::BTreeSet;

use clap::Parser;
use serde_json::Value;

use elfq::binary::{Binary, ItemKind};
use elfq::cli::Cli;

use crate::support::{load_sample, sample_path};

/// The samples with symbols whose every function the checks run on: the
/// Cortex-M7 sample, and each build with veneers to and from RAM and ROM.
const SAMPLES: [&str; 9] = [
    "c-cortex-m7.elf",
    "far-gnu-m0-nodebug.elf",
    "far-gnu-m4-nodebug.elf",
    "far-gnu-m4-pic-nodebug.elf",
    "far-gnu-m4-pure-nodebug.elf",
    "far-lld-m0-nodebug.elf",
    "far-lld-m0-pure-nodebug.elf",
    "far-lld-m4-nodebug.elf",
    "far-lld-m4-pic-nodebug.elf",
];

/// The answer to `elfq SAMPLE ARGS --json`, run in this process, which is
/// much quicker than starting elfq for each function.
fn answer(sample: &str, args: &[&str]) -> Value {
    let path = sample_path(sample);
    let command_line = [&["elfq", path.to_str().unwrap()], args, &["--json"]].concat();
    let cli = Cli::try_parse_from(command_line).unwrap();
    let mut out = Vec::new();
    elfq::commands::run(&cli, &mut out).unwrap_or_else(|err| panic!("{sample} {args:?}: {err:#}"));
    serde_json::from_slice(&out).unwrap()
}

/// The functions in `binary` whose code elfq can read, by address, as
/// SYMBOL takes them, so that a stripped file's functions are asked about
/// the same way.
fn functions(binary: &Binary) -> Vec<String> {
    binary
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Function && item.size > 0)
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
fn reach_lists_a_tree_of_what_callees_and_retained_list() {
    let mut checked = 0;
    for sample in SAMPLES {
        for function in functions(&load_sample(sample)) {
            let reach = answer(sample, &["reach", &function]);
            let context = format!("{sample} reach {function}");
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
            let callees = answer(sample, &["callees", &function]);
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
            let retained = answer(sample, &["retained", &function]);
            for kept in retained["items"].as_array().unwrap() {
                assert!(reached.contains(&item(kept)), "{context}: {kept}");
            }
            checked += 1;
        }
    }
    assert!(checked >= 200, "{checked}");
}

#[test]
fn a_stripped_file_reaches_the_same_functions() {
    let mut checked = 0;
    for sample in SAMPLES {
        // The twins differ only in their symbols, as the stripped file
        // has none: the C sample's has no DWARF debug info either.
        let with_symbols = match sample.contains("-nodebug") {
            true => sample.to_string(),
            false => sample.replace(".elf", "-nodebug.elf"),
        };
        let stripped = with_symbols.replace("-nodebug", "-stripped");
        // The functions as addresses and sizes, each with its parent's
        // address, since the stripped file names its items by address. Its
        // data may differ: without symbols, elfq can take data objects next
        // to each other for one.
        let tree = |sample: &str, function: &str| -> BTreeSet<[String; 3]> {
            let reach = answer(sample, &["reach", function]);
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
        let found: BTreeSet<String> = functions(&load_sample(&stripped)).into_iter().collect();
        for function in functions(&load_sample(&with_symbols)) {
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
    }
    assert!(checked >= 200, "{checked}");
}
