//! diff's answer as text: a line that says whether the builds load the same
//! memory, then for several blocks of memory, the MEMORY table, or for one,
//! a line on how its size changed, and last, a line that names the
//! sections that differ.

use std::cmp::Reverse;
use std::io::Write;

use anyhow::Result;

use super::Answer;
use super::blocks::{Block, SectionChange};
use crate::commands::overview::sections_text;
use crate::commands::text::{names_text, plural, range_argument, shown_count, signed};
use crate::render::{Align, Table};

/// How many sections a cell of SECTIONS, or the line on a block's size,
/// names before "and 5 more", unless `--all` asks for every one.
const CHANGES_NAMED: usize = 3;

pub(super) fn write(answer: &Answer, all: bool, out: &mut dyn Write) -> Result<()> {
    let (old_path, new_path) = (answer.old_path.display(), answer.new_path.display());
    if answer.same_memory {
        writeln!(
            out,
            "{old_path} and {new_path} hold the same bytes at the same addresses."
        )?;
        return Ok(());
    }
    let blocks = &answer.blocks;
    let differ = blocks.iter().filter(|block| !block.same_bytes).count();
    if blocks.len() > 1 && differ > 0 {
        writeln!(
            out,
            "From {old_path} to {new_path}: the bytes differ in {differ} of {}.",
            plural(blocks.len(), "block", "blocks")
        )?;
    } else {
        // In one block, or only in .tbss, which is in none.
        writeln!(out, "From {old_path} to {new_path}: the bytes differ.")?;
    }
    match &blocks[..] {
        // One block that both builds have gets a line, as the overview
        // gives one block no table.
        [block] if !block.old.is_empty() && !block.new.is_empty() => {
            writeln!(out, "{}", size_line(answer, block, all))?;
        }
        [] => {}
        _ => {
            writeln!(out)?;
            memory_table(answer, all).write(out)?;
            writeln!(out)?;
        }
    }
    // The table and the line on the size name the sections that changed
    // size, but not those that changed only their bytes or address, as a
    // new build ID does, nor .tbss, which is in no block.
    writeln!(
        out,
        "Sections that differ: {}.",
        names_text(&answer.sections_that_differ, all)
    )?;
    Ok(())
}

/// "The memory stays at 15728 bytes (.text +16, .eh_frame +8)." or "The
/// memory went from 224 to 208 bytes (.itcm -16)."
fn size_line(answer: &Answer, block: &Block, all: bool) -> String {
    let changes = changes_text(&block.sections(answer.old, answer.new), all);
    let changes = if changes.is_empty() {
        String::new()
    } else {
        format!(" ({changes})")
    };
    let (old, new) = (block.old_size(), block.new_size());
    if old == new {
        format!("The memory stays at {} bytes{changes}.", new.unwrap_or(0))
    } else {
        format!(
            "The memory went from {} to {} bytes{changes}.",
            old.unwrap_or(0),
            new.unwrap_or(0)
        )
    }
}

/// The MEMORY table: a row for each block, with its size in each build and
/// what changed in it.
fn memory_table(answer: &Answer, all: bool) -> Table {
    let mut table = Table::new(&[
        ("MEMORY", Align::Left),
        ("OLD", Align::Right),
        ("NEW", Align::Right),
        ("CHANGE", Align::Right),
        ("SECTIONS", Align::Left),
    ]);
    let size = |size: Option<u64>| size.map_or_else(|| "-".to_string(), |size| size.to_string());
    for block in &answer.blocks {
        let mut memory = range_argument(block.range);
        if block.old.is_empty() {
            memory += " (new)";
        } else if block.new.is_empty() {
            memory += " (gone)";
        }
        table.row(vec![
            memory,
            size(block.old_size()),
            size(block.new_size()),
            signed(block.change()),
            sections_cell(answer, block, all),
        ]);
    }
    table
}

/// What changed in a block: the sections whose size changed, or "bytes
/// differ" when none did but some bytes did, or nothing. A block that only
/// one build has names what is in it, as the overview's table does.
fn sections_cell(answer: &Answer, block: &Block, all: bool) -> String {
    let (memory, places) = match (block.old.is_empty(), block.new.is_empty()) {
        (true, _) => (answer.new, &block.new),
        (_, true) => (answer.old, &block.old),
        _ => return changes_cell(answer, block, all),
    };
    let names = |load_image: bool| -> Vec<&str> {
        places
            .iter()
            .filter(|place| place.load_image == load_image)
            .map(|place| memory.sections[place.section].name.as_str())
            .collect()
    };
    sections_text(&names(false), &names(true))
}

/// The sections whose size changed in a block that both builds have, or
/// "bytes differ", or nothing.
fn changes_cell(answer: &Answer, block: &Block, all: bool) -> String {
    let sections = block.sections(answer.old, answer.new);
    match changes_text(&sections, all) {
        changes if !changes.is_empty() => changes,
        _ if block.same_bytes => String::new(),
        _ => "bytes differ".to_string(),
    }
}

/// The sections whose size changed, each with the change, in address
/// order: those that changed the most, ".itcm -16, .rodata +16", and "and 5
/// more" for the rest, unless `all`.
fn changes_text(sections: &[SectionChange], all: bool) -> String {
    let changed: Vec<&SectionChange> = sections
        .iter()
        .filter(|section| section.change() != 0)
        .collect();
    let shown = shown_count(changed.len(), room(all));
    let mut largest: Vec<usize> = (0..changed.len()).collect();
    largest.sort_by_key(|&index| Reverse(changed[index].change().unsigned_abs()));
    largest.truncate(shown);
    largest.sort_unstable();
    let mut text = largest
        .iter()
        .map(|&index| {
            let section = changed[index];
            format!("{} {}", section_name(section), signed(section.change()))
        })
        .collect::<Vec<_>>()
        .join(", ");
    if shown < changed.len() {
        text += &format!(" and {} more", changed.len() - shown);
    }
    text
}

/// A section's name, or "load image of .data" for its load image.
fn section_name(section: &SectionChange) -> String {
    if section.load_image {
        format!("load image of {}", section.name)
    } else {
        section.name.to_string()
    }
}

/// How many sections to name: [`CHANGES_NAMED`], or with `--all`, every
/// one.
fn room(all: bool) -> usize {
    if all { usize::MAX } else { CHANGES_NAMED }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use object::SectionKind;

    use super::*;
    use crate::binary::{LoadedMemory, Section, same_memory};
    use crate::commands::diff::blocks::{sections_that_differ, shared_blocks};

    /// What diff writes for `old` and `new`, both read from `path`.
    fn text(path: &Path, old: &LoadedMemory, new: &LoadedMemory) -> String {
        let same_memory = same_memory(old, new);
        let answer = Answer {
            old_path: path,
            new_path: path,
            old,
            new,
            same_memory,
            blocks: shared_blocks(old, new, same_memory),
            sections_that_differ: sections_that_differ(old, new),
        };
        let mut out = Vec::new();
        write(&answer, false, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn read(path: &Path) -> Vec<u8> {
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
    }

    #[test]
    fn a_change_in_tbss_alone_is_named() {
        // .tbss puts no bytes in memory, so only its name, address and size
        // count, and the line that names the sections says why the exit
        // status is 1.
        let path = Path::new("testdata/bin/rust-x86_64-nodebug.elf");
        let data = read(path);
        let old = LoadedMemory::read(&data).unwrap();
        let mut new = LoadedMemory::read(&data).unwrap();
        let tbss = new
            .sections
            .iter()
            .position(|section| section.name == ".tbss")
            .unwrap();
        new.sections[tbss].size += 8;
        let size = shared_blocks(&old, &new, false)[0].new_size().unwrap();
        assert_eq!(
            text(path, &old, &new),
            format!(
                "From {0} to {0}: the bytes differ.\n\
                 The memory stays at {size} bytes.\n\
                 Sections that differ: .tbss.\n",
                path.display()
            )
        );
    }

    #[test]
    fn a_change_in_tbss_is_named_under_the_table() {
        // Firmware has several blocks, none of which holds .tbss.
        let path = Path::new("testdata/bin/c-cortex-m7.elf");
        let data = read(path);
        let tbss = |size| Section {
            name: ".tbss".to_string(),
            address: 0x2000_0000,
            size,
            kind: SectionKind::UninitializedTls,
            allocated: true,
            file_range: None,
            entry_size: 0,
            load_address: None,
        };
        let mut old = LoadedMemory::read(&data).unwrap();
        old.sections.push(tbss(8));
        let mut new = LoadedMemory::read(&data).unwrap();
        new.sections.push(tbss(16));
        assert_eq!(
            text(path, &old, &new),
            format!(
                "From {0} to {0}: the bytes differ.\n\
                 \n\
                 MEMORY                   OLD   NEW  CHANGE  SECTIONS\n\
                 0x00000000..0x000000e0   224   224       0\n\
                 0x20000000..0x20000450  1104  1104       0\n\
                 0x20240000..0x20241000  4096  4096       0\n\
                 0x30000000..0x30000edc  3804  3804       0\n\
                 \n\
                 Sections that differ: .tbss.\n",
                path.display()
            )
        );
    }
}
