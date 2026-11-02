//! What the commands' tables share.
//!
//! The tests check the tree that `--tree` draws, at most nine levels deep,
//! the lines and lists that name the first few items and sum up the rest,
//! and `--all`, which shows every item instead. Each command's own table
//! is tested in its file in commands/.

use crate::support::{
    DEEP_SAMPLE, SAMPLE, TCM_SAMPLE, assert_output, crossing, samples, stderr, stdout,
};

#[test]
fn tree_draws_the_rows_as_a_tree() {
    let tree = stdout(&[SAMPLE, "top", "--tree"]);
    assert!(
        tree.contains(
            "\
RETAINED   OWN  ITEM
    3635    38  _start
    3597   195  └─ main (through a pointer)
    2068   532     ├─ process_frame
    1083    59     │  ├─ crc32
    1024  1024     │  │  └─ crc_table
     378   271     │  ├─ apply_filter
     107    75     │  │  └─ filter_tap
      32    32     │  │     └─ fir_coeffs
      54    54     │  ├─ decode_header
      21           │  └─ 2 more
     705   370     ├─ report
     162   162     │  ├─ format_u32
      80    16     │  ├─ root
      48    16     │  │  ├─ inner
      32           │  │  │  └─ 2 more
      16    16     │  │  └─ leaf_a
      46    46     │  ├─ count_nodes
      47           │  └─ 6 more
     187    84     ├─ parse_value
     103   103     │  └─ parse_list
     103    51     ├─ log_msg
      36    20     │  ├─ platform_write
      16    16     │  │  └─ write@plt
      16    16     │  └─ strlen@plt
      57    51     ├─ app_tick
       6    ~6     │  └─ .rodata@0x200c
      34   ~34     ├─ .rodata@0x205e
      33    33     ├─ dispatch_event
      32    32     ├─ stats
      31    19     ├─ on_button (through a pointer)
      12           │  └─ 2 more
      25    25     ├─ frame_a
     127           └─ 12 more
     152   ~64  __do_global_dtors_aux
      48   ~48  ├─ deregister_tm_clones
      40        └─ 2 more
      80   ~16  frame_dummy
      64   ~64  └─ register_tm_clones
      64    64  .plt
      32    32  __abi_tag
      17        5 more
"
        ),
        "{tree}"
    );
    // The same rows in the same order, each named the same way.
    let rows = stdout(&[SAMPLE, "top"]);
    let names = |text: &str| -> Vec<String> {
        text.lines()
            .map(|line| {
                let line = line.split(", kept by ").next().unwrap();
                line.replace(['│', '├', '└', '─'], " ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    };
    assert_eq!(names(&tree), names(&rows));

    // The overview draws its largest items the same way, for the whole
    // file and for a range.
    let tree = stdout(&[SAMPLE, "--tree"]);
    assert!(
        tree.contains(
            "\
RETAINED   OWN  ITEM
    3635    38  _start
    3597   195  └─ main (through a pointer)
    2068   532     ├─ process_frame
    1083    59     │  ├─ crc32
    1024  1024     │  │  └─ crc_table
     453           │  └─ 6 more
    1334           └─ 41 more
     345        13 more
"
        ),
        "{tree}"
    );
    let tree = stdout(&[TCM_SAMPLE, "--range", "0x0..0xe0", "--tree"]);
    assert!(tree.contains("     200    0  └─ main\n"), "{tree}");

    // why's chains too.
    let tree = stdout(&[SAMPLE, "why", "log_msg", "--tree"]);
    assert!(
        tree.contains(
            "\
HOW            ITEM
entry point    _start
address taken  └─ main
call              ├─ app_tick
call              │  └─ log_msg
address taken     ├─ on_button
tail call         │  └─ log_msg
call              ├─ process_frame
call              │  └─ log_msg
call              └─ report
call                 └─ log_msg
"
        ),
        "{tree}"
    );
}

#[test]
fn no_line_sums_up_a_single_item() {
    // A row such as "12 more, kept by main" counts every item it sums up,
    // so one that would sum up a single item shows the item instead, and so
    // do lists of names.
    for sample in &samples() {
        let overview = stdout(&[sample]);
        let mut runs = vec![vec![sample.as_str()]];
        // `top` refuses shared libraries.
        if !sample.ends_with(".so") {
            runs.push(vec![sample, "top"]);
        }
        // The ranges in the overview's Try list.
        for (_, range) in overview
            .lines()
            .filter_map(|line| line.split_once(" --range "))
        {
            runs.push(vec![sample, "--range", range]);
            runs.push(vec![sample, "top", "--range", range]);
        }
        // What the item that the Try list asks about reaches.
        for line in overview.lines() {
            if let Some((_, symbol)) = line
                .split_once(" retained ")
                .or_else(|| line.split_once(" callees "))
            {
                runs.push(vec![sample, "reach", symbol]);
                runs.push(vec![sample, "reach", symbol, "--tree"]);
            }
        }
        for args in &runs {
            let text = stdout(args);
            assert!(
                !text.lines().any(|line| line.contains(" 1 more")),
                "{args:?}\n{text}"
            );
        }
    }
}

#[test]
fn long_lists_name_the_first_few() {
    let sample = "testdata/bin/rust-x86_64-nodebug.elf";
    let choices = stderr(&[sample, "callers", "fmt"]);
    assert!(
        choices.starts_with("error: `fmt` could mean 53 functions;"),
        "{choices}"
    );
    assert_eq!(choices.lines().count(), 12, "{choices}");
    assert!(choices.ends_with("\n  and 43 more\n"), "{choices}");
    let retained = stdout(&[sample, "retained", "main"]);
    assert!(retained.contains(" and 48 more.\n"), "{retained}");
    let inlined = stdout(&["testdata/bin/rust-cortex-m4.elf", "inlined"]);
    assert!(inlined.contains("     231  133 more\n"), "{inlined}");
}

#[test]
fn every_tree_is_at_most_nine_levels_deep() {
    // Each command, and the rows whose part of the tree is continued below.
    let trees = [
        (
            &[DEEP_SAMPLE, "top", "--tree"][..],
            &["stage7", "stage15"][..],
        ),
        (
            &[DEEP_SAMPLE, "retained", "stage1", "--tree"],
            &["stage9", "stage17"],
        ),
        (
            &[DEEP_SAMPLE, "why", "chain_table", "--tree"],
            &["chain7", "chain15"],
        ),
        (
            &[DEEP_SAMPLE, "why", "stage_table", "--tree"],
            &["stage7", "stage15"],
        ),
        (
            &[DEEP_SAMPLE, "inlined", "run_steps", "--tree"],
            &["step9", "step17"],
        ),
        (
            &[DEEP_SAMPLE, "reach", "chain1", "--tree"],
            &["chain9", "chain17"],
        ),
        (
            &[
                "testdata/bin/rust-x86_64-nodebug.elf",
                "why",
                "memset",
                "--tree",
            ],
            &[
                "std::panicking::default_hook",
                "rustc_demangle::try_demangle",
                "__rustc::__rust_alloc_zeroed",
            ],
        ),
    ];
    for (args, cuts) in trees {
        let text = stdout(args);
        // The tree is in the last column, which starts where its header
        // does. Each level is 3 columns.
        let header = text
            .lines()
            .find(|line| line.ends_with("ITEM") || line.ends_with("FUNCTION"));
        let column = header.unwrap().rfind(' ').unwrap() + 1;
        let deepest = text
            .lines()
            .filter(|line| line.contains("─ "))
            .map(|line| {
                line.chars()
                    .skip(column)
                    .take_while(|c| "│├└─ ".contains(*c))
                    .count()
            })
            .max();
        assert!(deepest.unwrap() <= 9 * 3, "{args:?}\n{text}");
        // Each part cut from the tree is continued below, in order.
        let cut: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_suffix(" (continued below)"))
            .map(|line| line.rsplit("─ ").next().unwrap())
            .collect();
        let continued: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_suffix(" (continued)"))
            .map(str::trim)
            .collect();
        assert_eq!(cut, cuts, "{args:?}\n{text}");
        assert_eq!(continued, cuts, "{args:?}\n{text}");
    }
}

/// Whether `text` sums up what it leaves out: a row such as "6 more, kept by
/// main" or "70 more", or a list in a sentence such as "a, b and 48 more.".
/// The AT column keeps its cut, as in "0x3a249 and 3 more  .rodata@0x6044".
fn sums_up(text: &str) -> bool {
    text.lines()
        .any(|line| line.contains(" more") && !line.contains(" more  "))
}

#[test]
fn all_shows_every_item() {
    let sample = "testdata/bin/rust-x86_64-nodebug.elf";
    let firmware = "testdata/bin/rust-cortex-m4.elf";
    for args in [
        vec![sample, "top"],
        vec![sample, "retained", "main"],
        vec![firmware, "inlined"],
        vec![sample, "callers", "memcpy"],
        vec![sample, "callees", "std::rt::lang_start_internal"],
    ] {
        let largest = stdout(&args);
        let all = stdout(&[&args[..], &["--all"]].concat());
        assert!(sums_up(&largest), "{args:?}\n{largest}");
        assert!(!sums_up(&all), "{args:?}\n{all}");
        assert!(all.lines().count() > largest.lines().count());
    }
    // A range check lists every crossing either way, and --all names every
    // function in the sentence under the table.
    let range = [sample, "callers", "0x20000+0x10000"];
    let largest = crossing(&range);
    let all = crossing(&[&range[..], &["--all"]].concat());
    assert!(largest.contains(" and 9 more aren't listed"), "{largest}");
    assert!(!sums_up(&all), "{all}");
    assert_eq!(all.lines().count(), largest.lines().count());
    // why draws the chains through the nearest 10 users, or every one.
    let chains = |why: &str| why.lines().filter(|line| line.ends_with(" memcpy")).count();
    let why = stdout(&[sample, "why", "memcpy"]);
    assert!(
        why.starts_with(
            "memcpy is used by 57 functions; these are the chains through the nearest 10:\n"
        ),
        "{why}"
    );
    assert_eq!(chains(&why), 10, "{why}");
    let why = stdout(&[sample, "why", "memcpy", "--all"]);
    assert!(
        why.starts_with("memcpy is used by 57 functions, through these chains:\n"),
        "{why}"
    );
    assert_eq!(chains(&why), 57, "{why}");
    // A tree of fewer than 30 items is the same either way.
    assert_output(
        &stdout(&[SAMPLE, "retained", "process_frame", "--all"]),
        &stdout(&[SAMPLE, "retained", "process_frame"]),
    );
}
