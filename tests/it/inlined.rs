//! The inlined copies that DWARF lists in the samples, checked against
//! testdata/README.md and against ddbug's view of the same files.

use elfq::binary::Binary;
use elfq::debuginfo::{InlinedCopy, inlined_copies};
use elfq::graph::Node;

use crate::support::{load_sample, node};

fn load(name: &str) -> (Binary, Vec<InlinedCopy>) {
    let binary = load_sample(name);
    let copies = inlined_copies(&binary).unwrap();
    (binary, copies)
}

fn item(binary: &Binary, name: &str) -> usize {
    match node(binary, name) {
        Node::Item(index) => index,
        Node::Import(_) => panic!("{name} is an import"),
    }
}

/// The copies inlined directly into `into`, as (name, size), in the order
/// DWARF lists them.
fn inlined_into(binary: &Binary, copies: &[InlinedCopy], into: &str) -> Vec<(String, u64)> {
    let into = item(binary, into);
    copies
        .iter()
        .filter(|copy| copy.into == into && copy.parent.is_none())
        .map(|copy| (copy.name.clone(), copy.size))
        .collect()
}

#[test]
fn process_frame_holds_three_inlined_functions() {
    let (binary, copies) = load("c-x86_64.elf");
    // load_le32 has no bytes of its own: the compiler merged it into the
    // code around it.
    assert_eq!(
        inlined_into(&binary, &copies, "process_frame"),
        [
            ("load_le32".to_string(), 0),
            ("unpack_samples".to_string(), 74),
            ("update_stats".to_string(), 164)
        ]
    );
    let (binary, copies) = load("c-cortex-m4.elf");
    let names: Vec<String> = inlined_into(&binary, &copies, "process_frame")
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, ["load_le32", "unpack_samples", "update_stats"]);
}

#[test]
fn append_is_inlined_six_times_into_report() {
    let (binary, copies) = load("c-x86_64.elf");
    let copies = inlined_into(&binary, &copies, "report");
    assert_eq!(copies.len(), 6);
    assert!(
        copies.iter().all(|(name, _)| name == "append"),
        "{copies:?}"
    );
}

#[test]
fn copies_inside_copies_count_once() {
    // memcpy's code is a copy of copy_forward, which holds copies of its
    // helpers in turn.
    let (binary, copies) = load("rust-cortex-m4.elf");
    let memcpy = item(&binary, "compiler_builtins::mem::memcpy");
    let (index, outer) = copies
        .iter()
        .enumerate()
        .find(|(_, copy)| copy.into == memcpy && copy.parent.is_none())
        .unwrap();
    assert_eq!(outer.name, "compiler_builtins::mem::impls::copy_forward");
    let inner: Vec<&InlinedCopy> = copies
        .iter()
        .filter(|copy| copy.parent == Some(index))
        .collect();
    assert!(
        inner
            .iter()
            .any(|copy| copy.name.ends_with("copy_forward_misaligned_words")),
        "{inner:?}"
    );
    let inner_bytes: u64 = inner.iter().map(|copy| copy.size).sum();
    assert_eq!(outer.own + inner_bytes, outer.size);
    // Own bytes don't overlap, so they add up to no more than the code.
    let own: u64 = copies
        .iter()
        .filter(|copy| copy.into == memcpy)
        .map(|copy| copy.own)
        .sum();
    assert_eq!(own, outer.size);
}

#[test]
fn a_file_without_dwarf_has_no_copies() {
    let (_, copies) = load("c-x86_64-nodebug.elf");
    assert!(copies.is_empty());
}
