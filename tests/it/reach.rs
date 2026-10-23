//! What `reach` lists, checked for every item of the Cortex-M samples
//! against `retained`, `callees` and the tree that `--tree` draws, with
//! `--to` against `reach` without it and `callees --to`, and for every
//! function of each stripped one against its twin with symbols.

use std::collections::{BTreeSet, HashMap};

use clap::Parser;
use serde_json::Value;

use elfq::binary::{AddressRange, Binary, ItemKind};
use elfq::cli::Cli;
use elfq::commands::Outcome;

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
    run(sample, args).1
}

/// What `elfq SAMPLE ARGS` prints, as [`output`] runs it, and the exit
/// status it means.
fn run(sample: &str, args: &[&str]) -> (Outcome, String) {
    let path = sample_path(sample);
    let command_line = [&["elfq", path.to_str().unwrap()], args].concat();
    let cli = Cli::try_parse_from(command_line).unwrap();
    let mut out = Vec::new();
    let outcome = elfq::commands::run(&cli, &mut out)
        .unwrap_or_else(|err| panic!("{sample} {args:?}: {err:#}"));
    (outcome, String::from_utf8(out).unwrap())
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
/// the order of the lines. The tree must be no more than 9 levels deep, so
/// that no part of it is continued below the table.
fn drawn(text: &str) -> Vec<Drawn> {
    assert!(!text.contains(" (continued below)\n"), "{text}");
    let mut lines = text.lines();
    let Some(header) = lines.find(|line| line.starts_with("ADDRESS")) else {
        return Vec::new();
    };
    // The tree is in the last column, after cells that are all ASCII. Each
    // level is 3 columns.
    let column = header.rfind(' ').unwrap() + 1;
    lines
        .take_while(|line| !line.is_empty())
        .map(|line| {
            let tree = &line[column..];
            let name = tree.trim_start_matches(['│', '├', '└', '─', ' ']);
            Drawn {
                level: (tree.chars().count() - name.chars().count()) / 3,
                address: line.split(' ').next().unwrap().to_string(),
                name: name.to_string(),
            }
        })
        .collect()
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
/// in the same order; without, its rows that sum up others, such as "4
/// more", count the rest, and none sums up a single item.
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
    let expected: Vec<(usize, String, String)> = items.iter().zip(levels).map(expected).collect();
    let found: Vec<(usize, String, String)> = rows
        .into_iter()
        .map(|row| (row.level, row.address, row.name))
        .collect();
    assert_eq!(found, expected, "{context} --all");

    let capped = output(sample, &["reach", symbol, "--tree"]);
    let counted: usize = drawn(&capped)
        .iter()
        .filter(|row| row.name != "(outside any function)")
        .map(|row| match row.name.strip_suffix(" more") {
            Some(count) if row.address.is_empty() => {
                let count = count.parse().unwrap();
                assert!(count > 1, "{context}: {capped}");
                count
            }
            _ => 1,
        })
        .sum();
    assert_eq!(counted, items.len(), "{context}: {capped}");
}

#[test]
fn reach_draws_plt_stubs_and_imports_in_the_tree() {
    // Every function of x86-64 files, whose PLT stubs jump to imports.
    let check = |sample: &str| {
        let binary = load_sample(sample);
        for symbol in symbols(&binary, ItemKind::Function) {
            let reach = answer(sample, &["reach", &symbol]);
            check_drawn(sample, &symbol, reach["items"].as_array().unwrap());
        }
    };
    std::thread::scope(|scope| {
        for sample in ["c-x86_64.elf", "cpp-x86_64.elf", "c-plt.so"] {
            scope.spawn(move || check(sample));
        }
    });
}

#[test]
fn reach_to_lists_the_rows_that_callees_to_lists() {
    let checked = on_each_sample(check_to);
    assert!(checked >= 800, "{checked}");
}

/// Checks `reach SYMBOL --to RANGE` for every item of `sample`, with each
/// block of memory that the overview lists as RANGE, the range of a chip's
/// ROM that the far samples call, and the range of a function that code
/// reaches only through a pointer, which the code that takes its address
/// is outside of: against `reach SYMBOL`, whose items in RANGE it
/// must list, with the calls and jumps that elfq can't follow that may go
/// there, and `callees USER --to RANGE`, which must list each of its rows
/// whose USER is outside RANGE. Returns how many such rows it checked.
///
/// With these ranges, a veneer and its target are never both in RANGE when
/// the code that calls the veneer isn't; then reach lists the veneer too,
/// which `callees` doesn't (see `reach_to_lists_a_veneer_and_its_target`).
fn check_to(sample: &str) -> usize {
    let binary = load_sample(sample);
    let overview = answer(sample, &[]);
    let mut ranges: Vec<String> = overview["memory"]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| {
            format!(
                "{}..{}",
                block["start"].as_str().unwrap(),
                block["end"].as_str().unwrap()
            )
        })
        .collect();
    if sample.starts_with("far-") {
        ranges.push("0x1fff0000+64K".to_string());
    }
    let symbols = [ItemKind::Function, ItemKind::Data].map(|kind| symbols(&binary, kind));
    let wholes: Vec<(String, Value)> = symbols
        .concat()
        .into_iter()
        .map(|symbol| {
            let whole = answer(sample, &["reach", &symbol]);
            (symbol, whole)
        })
        .collect();
    let pointed: BTreeSet<String> = wholes
        .iter()
        .flat_map(|(_, whole)| whole["items"].as_array().unwrap())
        .filter(|item| item["through_pointer"] == true)
        .map(|item| {
            let size = item["size"].as_u64().unwrap().max(1);
            format!("{}+{size}", item["address"].as_str().unwrap())
        })
        .collect();
    ranges.extend(pointed.into_iter().take(1));
    // Whether a call through a pointer may go to each range: when it holds
    // a function whose address is taken or stored, as `callers` lists them.
    let pointer: HashMap<&String, bool> = ranges
        .iter()
        .map(|range| {
            let callers = answer(sample, &["callers", range]);
            (
                range,
                !callers["through_pointer"].as_array().unwrap().is_empty(),
            )
        })
        .collect();

    let mut callees: HashMap<(usize, String), Vec<Row>> = HashMap::new();
    let mut checked = 0;
    for (symbol, whole) in &wholes {
        for range in &ranges {
            let context = format!("{sample} reach {symbol} --to {range}");
            let to: AddressRange = range.parse().unwrap();
            let (outcome, text) = run(sample, &["reach", symbol, "--to", range]);
            let reach = answer(sample, &["reach", symbol, "--to", range]);

            // The calls and jumps that elfq can't follow in what SYMBOL
            // reaches that may go to RANGE: those to an address in it, and
            // those through a pointer, when one may go there.
            let unresolved = |answer: &Value| -> Vec<((String, String), Value, Value)> {
                let unresolved = answer["unresolved"].as_array().unwrap();
                unresolved
                    .iter()
                    .map(|jump| {
                        let caller = item(&jump["caller"]);
                        (caller, jump["site"].clone(), jump["target"].clone())
                    })
                    .collect()
            };
            let expected: Vec<_> = unresolved(whole)
                .into_iter()
                .filter(|(_, _, target)| match target.as_str() {
                    Some(target) => to.contains(u64::from_str_radix(&target[2..], 16).unwrap()),
                    None => pointer[range],
                })
                .collect();
            let kept = unresolved(&reach);
            assert_eq!(kept, expected, "{context}: {text}");
            assert_eq!(reach["unreadable"], whole["unreadable"], "{context}");
            let complete = kept.iter().all(|(_, _, target)| !target.is_null())
                && whole["unreadable"].as_array().unwrap().is_empty();
            assert_eq!(reach["complete"], complete, "{context}");

            // The JSON: the items of `reach SYMBOL` in RANGE, those with
            // the calls and jumps it keeps, those on their chains, and
            // SYMBOL.
            let items = whole["items"].as_array().unwrap();
            let callers: BTreeSet<&(String, String)> =
                kept.iter().map(|(caller, _, _)| caller).collect();
            let in_to: Vec<bool> = items.iter().map(|item| is_in(item, to)).collect();
            let mut on_chain = in_to.clone();
            on_chain[0] = true;
            for (id, row) in items.iter().enumerate().rev() {
                on_chain[id] |= callers.contains(&item(row));
                if on_chain[id]
                    && let Some(parent) = row["parent"].as_u64()
                {
                    on_chain[parent as usize] = true;
                }
            }
            // Renumbered, so each row is compared by its parent's address.
            let parent = |items: &[Value], row: &Value| {
                let parent = row["parent"].as_u64()?;
                Some(items[parent as usize]["address"].to_string())
            };
            let expected: Vec<_> = items
                .iter()
                .zip(&in_to)
                .zip(&on_chain)
                .filter(|(_, on_chain)| **on_chain)
                .map(|((row, &in_to), _)| (item(row), Value::Bool(in_to), parent(items, row)))
                .collect();
            let listed = reach["items"].as_array().unwrap();
            let found: Vec<_> = listed
                .iter()
                .map(|row| (item(row), row["in_to"].clone(), parent(listed, row)))
                .collect();
            assert_eq!(found, expected, "{context}");
            let bytes = |kind: &str| -> u64 {
                listed
                    .iter()
                    .filter(|row| row["kind"] == kind && row["in_to"] == true)
                    .map(|row| row["size"].as_u64().unwrap())
                    .sum()
            };
            assert_eq!(reach["code_bytes"], bytes("function"), "{context}");
            assert_eq!(reach["data_bytes"], bytes("data"), "{context}");

            // The exit status, and the text: a row for each item in RANGE,
            // and one for each call or jump that elfq can't follow that may
            // go there.
            let found = in_to.iter().any(|&in_to| in_to)
                || !kept.is_empty()
                || !whole["unreadable"].as_array().unwrap().is_empty();
            let expected = if found {
                Outcome::Crossed
            } else {
                Outcome::Answered
            };
            assert_eq!(outcome, expected, "{context}: {text}");
            let shown = table(&text);
            let named: BTreeSet<(String, String)> = shown
                .iter()
                .filter(|row| !row.address.is_empty() && !row.item.starts_with('('))
                .map(|row| (row.item.clone(), row.address.clone()))
                .collect();
            let in_range: BTreeSet<(String, String)> = items
                .iter()
                .zip(&in_to)
                .filter(|(_, in_to)| **in_to)
                .map(|(row, _)| {
                    (
                        row["name"].as_str().unwrap().to_string(),
                        row["address"].as_str().unwrap().to_string(),
                    )
                })
                .collect();
            assert_eq!(named, in_range, "{context}: {text}");
            // The rows of the calls and jumps it keeps, each named by the
            // item it is in, or for a veneer's jump, by the item that calls
            // the veneer, and each of these rows by one of them.
            let users = |caller: &(String, String)| -> Vec<String> {
                let row = items.iter().find(|row| item(row) == *caller).unwrap();
                let parent = row["parent"].as_u64().map(|parent| &items[parent as usize]);
                [Some(row), parent]
                    .into_iter()
                    .flatten()
                    .map(|row| row["name"].as_str().unwrap().to_string())
                    .collect()
            };
            let unfollowed: Vec<&Row> = shown
                .iter()
                .filter(|row| row.item.starts_with('('))
                .collect();
            let same = |row: &Row, (caller, _, target): &((String, String), Value, Value)| {
                let (address, what) = match target.as_str() {
                    Some(target) => (target, "(outside any function)"),
                    None => ("", "(through a pointer)"),
                };
                (row.address.as_str(), row.item.as_str()) == (address, what)
                    && users(caller).contains(row.user.as_ref().unwrap())
            };
            for jump in &kept {
                let listed = unfollowed.iter().any(|row| same(row, jump));
                assert!(listed, "{context}: no row for {jump:?}: {text}");
            }
            for row in &unfollowed {
                let kept = kept.iter().any(|jump| same(row, jump));
                assert!(kept, "{context}: {row:?} is for nothing kept");
            }

            // Each row whose user is outside RANGE is a row of `callees USER
            // --to RANGE`, which names the user by its whole name, as the
            // row does: one of the items of that name, if there are several.
            for row in &shown {
                let Some(user) = &row.user else {
                    continue;
                };
                let users: Vec<usize> = (0..binary.items.len())
                    .filter(|&index| binary.items[index].name == *user)
                    .collect();
                assert!(!users.is_empty(), "{context}: no item is named {user}");
                let outside: Vec<usize> = users
                    .into_iter()
                    .filter(|&index| !binary.items[index].is_in(&[to]))
                    .collect();
                if outside.is_empty() {
                    continue;
                }
                let listed = outside.iter().any(|&index| {
                    let rows = callees.entry((index, range.clone())).or_insert_with(|| {
                        let address = format!("{:#x}", binary.items[index].address);
                        table(&output(sample, &["callees", &address, "--to", range]))
                    });
                    rows.contains(row)
                });
                assert!(listed, "{context}: {row:?} isn't in callees --to {range}");
                checked += 1;
            }
        }
    }
    checked
}

/// Whether `item`, from an answer's JSON, has a byte in `range`, where an
/// item of unknown size counts as its first byte.
fn is_in(item: &Value, range: AddressRange) -> bool {
    let Some(address) = item["address"].as_str() else {
        return false;
    };
    let address = u64::from_str_radix(address.trim_start_matches("0x"), 16).unwrap();
    range.overlap(address, item["size"].as_u64().unwrap().max(1)) > 0
}

/// A row of the table of `reach --to` or `callees --to`: its cells, with
/// the name split from the user after "used by", if there is one.
#[derive(Debug, PartialEq)]
struct Row {
    address: String,
    size: String,
    how: String,
    at: String,
    item: String,
    user: Option<String>,
}

/// The rows of the table in `text`, whose columns are ADDRESS, SIZE, HOW,
/// AT, and the names.
fn table(text: &str) -> Vec<Row> {
    let mut lines = text.lines();
    let Some(header) = lines.find(|line| line.starts_with("ADDRESS")) else {
        return Vec::new();
    };
    // Every cell but the last is ASCII, SIZE is aligned right, and the
    // others left.
    let size_end = header.find("SIZE").unwrap() + "SIZE".len();
    let how = header.find("HOW").unwrap();
    let at = header.find("AT ").unwrap();
    let names = header.rfind(' ').unwrap() + 1;
    lines
        .take_while(|line| !line.is_empty())
        .map(|line| {
            let (item, user) = match line[names..].rsplit_once(", used by ") {
                Some((item, user)) => (item.to_string(), Some(user.to_string())),
                None => (line[names..].to_string(), None),
            };
            Row {
                address: line[..size_end].split(' ').next().unwrap().to_string(),
                size: line[..size_end].rsplit(' ').next().unwrap().to_string(),
                how: line[how..at].trim().to_string(),
                at: line[at..names].trim().to_string(),
                item,
                user,
            }
        })
        .collect()
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
