//! The checks that commands make before they answer: that elfq can read
//! the file's code, that SYMBOL's item is in the file and its code can be
//! read, that there are bytes to count, and that a range holds an item.
//! Each fails with an error that says what is missing.
//!
//! None of them needs the reference graph, so a command makes them before
//! it builds the graph where it can, and a mistyped argument fails at
//! once.

use anyhow::{Result, bail};

use super::text::{self, Detail};
use crate::binary::{AddressRange, Binary, Item, Source, memory_blocks, total};
use crate::graph::Node;

/// Explains why elfq can't build the reference graph yet, if it can't.
pub(super) fn check_code_is_readable(binary: &Binary) -> Result<()> {
    if binary.instruction_set().is_none() {
        bail!("{}", text::cannot_read_code(binary));
    }
    Ok(())
}

/// The index of `node`'s item, or an error that says it is an import,
/// whose code is in another file.
pub(super) fn item_in_file(binary: &Binary, node: Node) -> Result<usize> {
    match node {
        Node::Item(index) => Ok(index),
        Node::Import(_) => bail!(
            "{} is imported from a shared library, so its code isn't in this file",
            node.name(binary)
        ),
    }
}

/// The index of `node`'s item, or why elfq can't read its code: it is an
/// import, or a function whose end elfq doesn't know.
pub(super) fn readable_item(binary: &Binary, node: Node) -> Result<usize> {
    let index = item_in_file(binary, node)?;
    let item = &binary.items[index];
    if item.end_unknown() {
        bail!("{}", text::ends_unknown(&[&item.name], Detail::All));
    }
    Ok(index)
}

/// Explains why there are no bytes to count in the file, or with a range,
/// in it, if there aren't: a range that no section or load image is in is
/// most likely mistyped, in some stripped files elfq can't find the
/// functions yet, and hand-written assembly often leaves out the sizes.
pub(super) fn check_has_bytes(binary: &Binary, range: Option<AddressRange>) -> Result<()> {
    if total(binary.items.iter().map(|item| binary.bytes_in(item, range))) > 0 {
        return Ok(());
    }
    let place = empty_place(binary, range)?;
    let starts_in_place = |item: &&Item| {
        range.is_none_or(|range| {
            [Some(item.address), binary.load_address(item)]
                .into_iter()
                .flatten()
                .any(|start| range.contains(start))
        })
    };
    if let Some(item) = binary.items.iter().find(starts_in_place) {
        bail!(
            "elfq doesn't know where {} ends, so it can't count the bytes in {place}",
            item.name
        );
    }
    bail!("elfq found no functions or data in {place}")
}

/// Explains that no item is in `range`, as for a mistyped range, unless
/// one is. `callers` and `callees` refuse such a range with it, given in
/// place of SYMBOL or to `--from` or `--to`, and so does `retained`, given
/// in place of SYMBOL.
pub(super) fn check_holds_items(binary: &Binary, range: AddressRange) -> Result<()> {
    if binary.items.iter().any(|item| item.is_in(&[range])) {
        return Ok(());
    }
    let place = empty_place(binary, Some(range))?;
    // Where the program uses what is stored in the range.
    let used_at: Vec<String> = binary
        .load_images_in(range)
        .into_iter()
        .map(|section| {
            let used_at = AddressRange {
                start: section.address,
                end: section.address + section.size,
            };
            format!("{} at {}", section.name, text::range_argument(used_at))
        })
        .collect();
    if !used_at.is_empty() {
        let used_at: Vec<&str> = used_at.iter().map(String::as_str).collect();
        bail!(
            "elfq found no functions or data in {place}; the program uses {}",
            text::names_text(&used_at, Detail::Brief)
        );
    }
    bail!("elfq found no functions or data in {place}");
}

/// Where an answer that found nothing looked, for its error: `range`, such
/// as `0x00000000..0x000000e0 (.itcm)`, or without one, "the file". Fails
/// instead when no section or load image is in `range`, which is then most
/// likely mistyped, saying where to look: the file's one block of
/// addresses, or the overview, which lists them when there are several. It
/// also fails when elfq can't find the file's functions yet.
pub(super) fn empty_place(binary: &Binary, range: Option<AddressRange>) -> Result<String> {
    let place = match range {
        Some(range)
            if binary.sections_in(range).is_empty() && binary.load_images_in(range).is_empty() =>
        {
            let range = text::range_argument(range);
            match &memory_blocks(&binary.sections)[..] {
                [] => bail!("the file has nothing in {range}"),
                [block] => bail!(
                    "the file has nothing in {range}, only in {}",
                    text::range_argument(block.range)
                ),
                _ => bail!("the file has nothing in {range}; the overview lists its ranges"),
            }
        }
        Some(range) => text::range_text(binary, range, Detail::Brief),
        None => "the file".to_string(),
    };
    if !found_functions(binary) {
        bail!("{}", text::NO_FUNCTIONS);
    }
    Ok(place)
}

/// Whether elfq found the file's functions: a symbol table or an unwind
/// table lists them, or in firmware elfq followed the code (see
/// `unnamed_functions.rs`). Otherwise it has only the entry point, and
/// doesn't know its size.
pub(super) fn found_functions(binary: &Binary) -> bool {
    binary.items.iter().any(|item| match item.source {
        Source::EntryPoint | Source::Reached => item.size > 0,
        source => source.lists_functions(),
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::{overview, top};
    use crate::cli::Format;

    use super::*;

    #[test]
    fn a_file_whose_functions_elfq_cant_find_says_so() {
        // Such as stripped firmware for a processor whose code elfq can
        // read but not follow yet: it has only its entry point.
        let path = Path::new("testdata/bin/c-cortex-m4-stripped.elf");
        let mut binary = Binary::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap();
        binary
            .items
            .retain(|item| item.source == Source::EntryPoint);
        binary.items[0].size = 0;
        let mut out = Vec::new();
        overview::run(path, &binary, None, Format::Text, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
testdata/bin/c-cortex-m4-stripped.elf: 32-bit Arm executable, stripped
1 function
elfq can't yet find functions without symbols or an unwind table.

MEMORY                  SIZE  SECTIONS
0x08000000..0x08000e0c  3596  .isr_vector .text .rodata .ARM.exidx, load image of .data
0x20000000..0x20000048    72  .data .bss

Try:
  elfq testdata/bin/c-cortex-m4-stripped.elf startup
"
        );
        // startup reads the code from the reset handler all the same.
        let error =
            top::run(&binary, None, Detail::Brief, Format::Text, &mut Vec::new()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "elfq can't yet find functions without symbols or an unwind table"
        );
    }
}
