//! What `reach` lists, checked for every item of the Cortex-M samples
//! against `retained`, `callees` and the tree that `--tree` draws, and for
//! every function of each stripped one against its twin with symbols.

use std::collections::BTreeSet;

use clap::Parser;
use serde_json::Value;

use elfq::binary::{Binary, ItemKind};
use elfq::cli::Cli;

use crate::support::{load_sample, sample_path};

/// The Cortex-M samples with symbols, whose every item the checks run on:
/// the C and Rust samples, and each build with veneers to and from RAM and
/// ROM.
const SAMPLES: [&str; 13] = [
    "c-cortex-m4.elf",
    "c-cortex-m4-cmsis-nodebug.elf",
    "c-cortex-m7.elf",
    "c-softfloat-nodebug.elf",
    "rust-cortex-m4-nodebug.elf",
    "far-gnu-m0-nodebug.elf",
    "far-gnu-m4-nodebug.elf",
    "far-gnu-m4-pic-nodebug.elf",
    "far-gnu-m4-pure-nodebug.elf",
    "far-lld-m0-nodebug.elf",
    "far-lld-m0-pure-nodebug.elf",
    "far-lld-m4-nodebug.elf",
    "far-lld-m4-pic-nodebug.elf",
];

/// What `elfq SAMPLE ARGS` prints, run in this process, which is much
/// quicker than starting elfq for each function.
fn output(sample: &str, args: &[&str]) -> String {
    let path = sample_path(sample);
    let command_line = [&["elfq", path.to_str().unwrap()], args].concat();
    let cli = Cli::try_parse_from(command_line).unwrap();
    let mut out = Vec::new();
    elfq::commands::run(&cli, &mut out).unwrap_or_else(|err| panic!("{sample} {args:?}: {err:#}"));
    String::from_utf8(out).unwrap()
}

/// The answer to `elfq SAMPLE ARGS --json`.
fn answer(sample: &str, args: &[&str]) -> Value {
    serde_json::from_str(&output(sample, &[args, &["--json"]].concat())).unwrap()
}

/// A row of the tree that `--tree` draws: how deep it is, counting from 0,
/// its ADDRESS cell, and the name after the branches.
struct Drawn {
    level: usize,
    address: String,
    name: String,
}

/// The rows of the tree in the table of `text`, as `--tree` draws it, in
/// the order of the lines, without the rows that start a part continued
/// below.
fn drawn(text: &str) -> Vec<Drawn> {
    let mut lines = text.lines();
    let Some(header) = lines.find(|line| line.starts_with("ADDRESS")) else {
        return Vec::new();
    };
    // The tree is in the last column, after cells that are all ASCII. Each
    // level is 3 columns.
    let column = header.rfind(' ').unwrap() + 1;
    let mut rows = Vec::new();
    let mut lines = lines.peekable();
    while let Some(line) = lines.next() {
        if line.is_empty() {
            // A part continued below starts with an empty line, then a row
            // with only a name.
            match lines.peek() {
                Some(next) if next.len() > column && next[..column].trim().is_empty() => continue,
                _ => break,
            }
        }
        let tree = &line[column..];
        if line[..column].trim().is_empty() && tree.ends_with(" (continued)") {
            continue;
        }
        let name = tree.trim_start_matches(['│', '├', '└', '─', ' ']);
        rows.push(Drawn {
            level: (tree.chars().count() - name.chars().count()) / 3,
            address: line.split(' ').next().unwrap().to_string(),
            name: name.trim_end_matches(" (continued below)").to_string(),
        });
    }
    rows
}

/// The items of `kind` in `binary` that reach takes, by address, as SYMBOL
/// takes them, so that a stripped file's items are asked about the same
/// way: every data object, and every function whose code elfq can read.
fn symbols(binary: &Binary, kind: ItemKind) -> Vec<String> {
    binary
        .items
        .iter()
        .filter(|item| item.kind == kind && (kind == ItemKind::Data || item.size > 0))
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
    let checked = on_each_sample(check_tree);
    assert!(checked >= 450, "{checked}");
}

/// Checks `reach` on every item of `sample` against `callees` and
/// `retained`, and returns how many items it checked.
fn check_tree(sample: &str) -> usize {
    let mut checked = 0;
    let binary = load_sample(sample);
    let symbols = [ItemKind::Function, ItemKind::Data].map(|kind| symbols(&binary, kind));
    for symbol in symbols.concat() {
        let reach = answer(sample, &["reach", &symbol]);
        let context = format!("{sample} reach {symbol}");
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
        let callees = answer(sample, &["callees", &symbol]);
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
        let retained = answer(sample, &["retained", &symbol]);
        for kept in retained["items"].as_array().unwrap() {
            assert!(reached.contains(&item(kept)), "{context}: {kept}");
        }

        check_drawn(sample, &symbol, items);
        checked += 1;
    }
    checked
}

/// Checks the tree that `reach SYMBOL --tree` draws against `items`, the
/// rows of the JSON: with `--all`, it has every one, each under its parent,
/// in the same order unless part of it is continued below; without, its
/// rows that sum up others, such as "4 more", count the rest.
fn check_drawn(sample: &str, symbol: &str, items: &[Value]) {
    let context = format!("{sample} reach {symbol} --tree");
    let all = output(sample, &["reach", symbol, "--tree", "--all"]);
    let rows: Vec<Drawn> = drawn(&all)
        .into_iter()
        .filter(|row| row.name != "(outside any function)")
        .collect();
    if rows.is_empty() {
        assert_eq!(items.len(), 1, "{context}: {all}");
        return;
    }
    let mut levels: Vec<usize> = Vec::new();
    for row in items {
        let level = row["parent"]
            .as_u64()
            .map_or(0, |parent| levels[parent as usize] + 1);
        levels.push(level);
    }
    let expected = |(item, level): (&Value, usize)| {
        let address = item["address"].as_str().unwrap_or_default();
        let mark = if item["through_pointer"] == true {
            " (through a pointer)"
        } else {
            ""
        };
        (
            level,
            address.to_string(),
            format!("{}{mark}", item["name"].as_str().unwrap()),
        )
    };
    let mut expected: Vec<(usize, String, String)> =
        items.iter().zip(levels).map(expected).collect();
    let mut found: Vec<(usize, String, String)> = rows
        .into_iter()
        .map(|row| (row.level, row.address, row.name))
        .collect();
    if all.contains(" (continued below)\n") {
        // A part continued below starts again on level 0.
        expected = expected
            .into_iter()
            .map(|(_, address, name)| (0, address, name))
            .collect();
        found = found
            .into_iter()
            .map(|(_, address, name)| (0, address, name))
            .collect();
        expected.sort();
        found.sort();
    }
    assert_eq!(found, expected, "{context} --all");

    let capped = output(sample, &["reach", symbol, "--tree"]);
    let counted: usize = drawn(&capped)
        .iter()
        .filter(|row| row.name != "(outside any function)")
        .map(|row| match row.name.strip_suffix(" more") {
            Some(count) if row.address.is_empty() => count.parse().unwrap(),
            _ => 1,
        })
        .sum();
    assert_eq!(counted, items.len(), "{context}: {capped}");
}

#[test]
fn a_stripped_file_reaches_the_same_functions() {
    let checked = on_each_sample(check_twin);
    assert!(checked >= 280, "{checked}");
}

/// Checks that `reach` gives the same functions for every function of
/// `sample`'s stripped twin, if it has one, and returns how many functions
/// it checked.
fn check_twin(sample: &str) -> usize {
    // A stripped file's twin is the build without DWARF debug info, which
    // differs from it only in its symbols.
    let with_symbols = match sample.contains("-nodebug") {
        true => sample.to_string(),
        false => sample.replace(".elf", "-nodebug.elf"),
    };
    let stripped = with_symbols.replace("-nodebug", "-stripped");
    if !sample_path(&stripped).exists() {
        return 0;
    }
    // The functions as addresses and sizes, each with its parent's address,
    // since the stripped file names its items by address. Its data may
    // differ: without symbols, elfq can take data objects next to each
    // other for one.
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
    let found: BTreeSet<String> = symbols(&load_sample(&stripped), ItemKind::Function)
        .into_iter()
        .collect();
    let mut checked = 0;
    for function in symbols(&load_sample(&with_symbols), ItemKind::Function) {
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
    checked
}

/// Runs `check` on each of the samples, each on a thread of its own, and
/// adds up what they return.
fn on_each_sample(check: fn(&str) -> usize) -> usize {
    std::thread::scope(|scope| {
        let threads = SAMPLES.map(|sample| scope.spawn(move || check(sample)));
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .sum()
    })
}
