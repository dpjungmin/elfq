//! `inlined`: the functions that the compiler copied into others.
//!
//! DWARF lists them. The tests check them ranked and as a tree, as text
//! and as JSON, and the error for a file without DWARF.

use serde_json::{Value, json};

use crate::support::{SAMPLE, assert_output, elfq_json, names, stderr, stdout, text_rows};

#[test]
fn inlined_ranks_the_functions_the_compiler_copied() {
    assert_output(
        &stdout(&[SAMPLE, "inlined"]),
        "\
Inlined copies of 5 functions take 420 of the 2557 bytes of code:

BYTES  COPIES  FUNCTION
  164       1  update_stats
  150       6  append
   74       1  unpack_samples
   32       1  saturate
    0       1  load_le32

A copy of 0 bytes is one the compiler merged into the code around it.
",
    );
    assert_output(
        &stdout(&[SAMPLE, "inlined", "process_frame"]),
        "\
Of process_frame's 532 bytes, 238 are inlined copies of 3 functions:

BYTES  COPIES  FUNCTION
  164       1  update_stats
   74       1  unpack_samples
    0       1  load_le32

A copy of 0 bytes is one the compiler merged into the code around it.
",
    );
    // The six calls to `append` in `report` make one row.
    assert_output(
        &stdout(&[SAMPLE, "inlined", "report"]),
        "\
Of report's 370 bytes, 150 are inlined copies of 1 function:

BYTES  COPIES  FUNCTION
  150       6  append
",
    );
    assert_output(
        &stdout(&[SAMPLE, "inlined", "main"]),
        "The debug info lists no inlined code in main.\n",
    );
}

#[test]
fn inlined_shows_copies_inside_copies() {
    let rows = stdout(&["testdata/bin/rust-cortex-m4.elf", "inlined", "memcpy"]);
    assert!(
        rows.starts_with(
            "\
Of compiler_builtins::mem::memcpy's 730 bytes, 712 are inlined copies of 17 functions:

BYTES  COPIES  FUNCTION
  712       1  compiler_builtins::mem::impls::copy_forward
  392       1  compiler_builtins::mem::impls::copy_forward::copy_forward_misaligned_words, inlined into copy_forward
   74       1  compiler_builtins::mem::impls::load_aligned_partial, inlined into copy_forward_misaligned_words
"
        ),
        "{rows}"
    );
    // A function inlined in several places names where its copies are.
    let rows = stdout(&[
        "testdata/bin/rust-cortex-m4.elf",
        "inlined",
        "process_frame",
    ]);
    for row in [
        "   14       1  core::sync::atomic::atomic_add::<u32, u32>, inlined into fetch_add in \
         update_stats\n",
        "   42       3  core::sync::atomic::atomic_add::<u32, u32>, inlined into fetch_add in \
         process_frame\n",
    ] {
        assert!(rows.contains(row), "{rows}");
    }
    let tree = stdout(&[
        "testdata/bin/rust-cortex-m4.elf",
        "inlined",
        "memcpy",
        "--tree",
    ]);
    assert!(
        tree.contains(
            "\
  712       1  compiler_builtins::mem::impls::copy_forward
  392       1  ├─ compiler_builtins::mem::impls::copy_forward::copy_forward_misaligned_words
   74       1  │  ├─ compiler_builtins::mem::impls::load_aligned_partial
"
        ),
        "{tree}"
    );
}

#[test]
fn inlined_needs_dwarf() {
    assert_output(
        &stderr(&["testdata/bin/c-x86_64-nodebug.elf", "inlined"]),
        "error: testdata/bin/c-x86_64-nodebug.elf has no DWARF debug info; build it with debug \
         info and don't strip it\n",
    );
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
