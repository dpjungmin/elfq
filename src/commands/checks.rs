//! The checks that commands make before they answer: that elfq can read
//! the file's code, that SYMBOL's item is in the file and its code can be
//! read, and that there are bytes to count. Each fails with an error that
//! says what is missing.

use anyhow::{Result, bail};

use super::text;
use crate::arch;
use crate::binary::{AddressRange, Binary, Item, ItemKind, Source, memory_blocks, total};
use crate::graph::{Graph, Node};

/// Builds the reference graph, or explains why elfq can't yet.
pub(super) fn reference_graph(binary: &Binary) -> Result<Graph> {
    check_code_is_readable(binary)?;
    Ok(Graph::build(binary))
}

/// Explains why elfq can't build the reference graph yet, if it can't.
pub(super) fn check_code_is_readable(binary: &Binary) -> Result<()> {
    if !arch::is_supported(binary.architecture, binary.thumb) {
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
    if item.kind == ItemKind::Function && item.size == 0 {
        bail!(
            "elfq doesn't know where {} ends, so it can't read its code",
            item.name
        );
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

/// Where an answer that found nothing looked, for its error: `range`, such
/// as `0x00000000..0x000000e0 (.itcm)`, or without one, "the file". Fails
/// instead when no section or load image is in `range`, which is then most
/// likely mistyped, saying where to look: the file's one block of
/// addresses, or the overview, which lists them when there are several. It
/// also fails when elfq can't find the file's functions yet.
pub(super) fn empty_place(binary: &Binary, range: Option<AddressRange>) -> Result<String> {
    let place = match range {
        Some(range)
            if text::sections_in(binary, range).is_empty()
                && text::load_images_in(binary, range).is_empty() =>
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
        Some(range) => text::range_text(binary, range, false),
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
        let error = top::run(&binary, None, false, Format::Text, &mut Vec::new()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "elfq can't yet find functions without symbols or an unwind table"
        );
    }
}
