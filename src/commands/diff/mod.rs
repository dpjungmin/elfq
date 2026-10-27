//! `elfq OLD diff NEW`: whether two builds of a program load the same
//! memory, and if not, in which blocks of memory the bytes differ, which
//! sections changed size there, and which sections differ.
//!
//! diff reads only the files' headers and the bytes that their sections put
//! in memory (see [`LoadedMemory`]), not their items, so it answers at once
//! even for a large file. The blocks are worked out once over both files
//! (see `blocks.rs`); `text.rs` and `json.rs` write the answer.

mod blocks;
mod json;
mod text;

use std::io::Write;
use std::path::Path;
use std::thread;

use anyhow::{Context, Result, bail};

use super::Outcome;
use super::text::architecture_name;
use crate::binary::{LoadedMemory, same_memory};
use crate::cli::Format;
use blocks::Block;

/// What diff found, for the text and the JSON to write.
struct Answer<'a> {
    old_path: &'a Path,
    new_path: &'a Path,
    old: &'a LoadedMemory<'a>,
    new: &'a LoadedMemory<'a>,
    /// Whether both builds load the same bytes at the same addresses.
    same_memory: bool,
    blocks: Vec<Block>,
    /// The sections whose address, size, load image or bytes differ, or
    /// that only one build has, in address order: none when the memory is
    /// the same.
    sections_that_differ: Vec<&'a str>,
}

/// Answers `elfq OLD diff NEW`: [`Outcome::Answered`] when both load the
/// same memory, and [`Outcome::Differs`] when they don't.
pub fn run(
    old_path: &Path,
    new_path: &Path,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    let (old_data, new_data) = read_both(old_path, new_path)?;
    let old = read_memory(old_path, &old_data)?;
    let new = read_memory(new_path, &new_data)?;
    for (path, memory) in [(old_path, &old), (new_path, &new)] {
        for warning in &memory.warnings {
            // As `super::write_warnings`, with the file's name, since
            // there are two.
            let _ = writeln!(std::io::stderr(), "warning: {}: {warning}", path.display());
        }
    }
    for (path, memory) in [(old_path, &old), (new_path, &new)] {
        let path = path.display();
        if memory.sections.is_empty() {
            bail!("{path} has no section headers, which diff needs to find its memory");
        }
        if memory.sections_in_memory().next().is_none() {
            bail!("{path} puts nothing in memory, so diff has nothing to compare");
        }
        if !memory.stores_code {
            bail!(
                "{path} stores none of its code, as a separate debug file doesn't; \
                 diff needs the program itself"
            );
        }
    }
    if old.architecture != new.architecture {
        bail!(
            "{} is {} and {} is {}; diff compares two builds of one program",
            old_path.display(),
            architecture_name(old.architecture),
            new_path.display(),
            architecture_name(new.architecture)
        );
    }
    let same_memory = same_memory(&old, &new);
    let answer = Answer {
        old_path,
        new_path,
        old: &old,
        new: &new,
        same_memory,
        blocks: blocks::shared_blocks(&old, &new, same_memory),
        sections_that_differ: if same_memory {
            Vec::new()
        } else {
            blocks::sections_that_differ(&old, &new)
        },
    };
    match format {
        Format::Json => json::write(&answer, out)?,
        Format::Text | Format::Tree => text::write(&answer, all, out)?,
    }
    Ok(if same_memory {
        Outcome::Answered
    } else {
        Outcome::Differs
    })
}

/// Reads both files, the second on a thread of its own, since a large
/// file takes a while even from the system's cache.
fn read_both(old_path: &Path, new_path: &Path) -> Result<(Vec<u8>, Vec<u8>)> {
    let read = |path: &Path| {
        std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))
    };
    thread::scope(|scope| {
        let new = thread::Builder::new().spawn_scoped(scope, || read(new_path));
        let old = read(old_path);
        let new = match new {
            Ok(thread) => thread
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            // When no thread can start, read it here.
            Err(_) => read(new_path),
        };
        Ok((old?, new?))
    })
}

/// Reads the headers of the file at `path`, whose bytes are `data`.
fn read_memory<'data>(path: &Path, data: &'data [u8]) -> Result<LoadedMemory<'data>> {
    LoadedMemory::read(data).with_context(|| format!("cannot load {}", path.display()))
}
