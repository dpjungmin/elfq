//! `elfq OLD diff NEW`: whether two builds of a program load the same
//! memory, and if not, in which blocks of memory the bytes differ, and
//! which functions and data objects moved to another block, are new or
//! gone, or changed size, each with what keeps it.
//!
//! diff first reads only the files' headers and the bytes that their
//! sections put in memory (see [`LoadedMemory`]), so it answers at once
//! when the memory is the same, even for a large file. The blocks are
//! worked out once over both files (see `blocks.rs`). When the memory
//! differs, and both files have a symbol table, it loads both, each with
//! its graph and, unless either is a shared library, its dominator tree,
//! pairs their items (`pairing.rs`) and works out what changed (`rows.rs`),
//! and which calls and other uses between blocks are new or gone
//! (`crossings.rs`). `text.rs` and `json.rs` write the answer.

mod blocks;
mod crossings;
mod json;
mod pairing;
mod rows;
mod text;

use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::thread;

use anyhow::{Context, Result, bail};

use super::symbol::Names;
use super::text::architecture_name;
use super::{Kept, Outcome};
use crate::STACK_SIZE;
use crate::arch;
use crate::binary::{Binary, FileKind, LoadedMemory, same_memory};
use crate::cli::Format;
use crate::graph::{DominatorTree, Graph};
use blocks::{Block, Differs};
use crossings::Crossing;
use rows::Items;

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
    sections_that_differ: Vec<Differs<'a>>,
    /// The builds' items, paired, when the memory differs and both builds
    /// have a symbol table.
    items: Option<Compared<'a>>,
}

/// The items that diff compared, the uses between blocks that are new or
/// gone, and the names that the text calls the items by.
struct Compared<'a> {
    items: Items<'a>,
    /// The uses between blocks that are new or gone: none with one block.
    crossings: Vec<Crossing>,
    old_names: Kept<Names<'a>>,
    new_names: Kept<Names<'a>>,
}

/// A build whose items diff compares: the file, its reference graph, and
/// its dominator tree, which neither build has when either is a shared
/// library, nor a file whose code elfq can't read.
struct Build {
    binary: Binary,
    graph: Graph,
    tree: Option<DominatorTree>,
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
    let read = |path: &Path| {
        std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))
    };
    let (old_data, new_data) = on_both(old_path, new_path, read);
    let (old_data, new_data) = (old_data?, new_data?);
    let old = read_memory(old_path, &old_data)?;
    let new = read_memory(new_path, &new_data)?;
    for (path, memory) in [(old_path, &old), (new_path, &new)] {
        write_warnings(path, &memory.warnings, &[]);
    }
    check(old_path, &old, new_path, &new)?;
    let same_memory = same_memory(&old, &new);
    if same_memory || !old.has_symbols || !new.has_symbols {
        let answer = Answer::new(old_path, new_path, &old, &new, same_memory);
        return write(&answer, all, format, out);
    }
    // elfq can't yet tell what keeps items in a shared library, and the
    // rows say what keeps items in both builds or in neither.
    let trees = ![old.kind, new.kind].contains(&FileKind::SharedLibrary);
    // The builds take the files' bytes, which the headers read above
    // borrow, and the headers are read again from them.
    let warned = [old.warnings, new.warnings];
    let (old_build, new_build) = on_both(
        (old_path, old_data),
        (new_path, new_data),
        |(path, data)| Build::load(path, data, trees),
    );
    let (old_build, new_build) = (Kept::new(old_build?), Kept::new(new_build?));
    let old = read_memory(old_path, old_build.binary.file_data())?;
    let new = read_memory(new_path, new_build.binary.file_data())?;
    for ((path, build), warned) in [(old_path, &old_build), (new_path, &new_build)]
        .into_iter()
        .zip(&warned)
    {
        write_warnings(path, &build.binary.warnings, warned);
    }
    let old_names = Kept::new(Names::new(&old_build.binary));
    let new_names = Kept::new(Names::new(&new_build.binary));
    let mut answer = Answer::new(old_path, new_path, &old, &new, false);
    let items = rows::compare(&old_build, &new_build, &answer.blocks);
    let crossings = if answer.blocks.len() > 1 {
        crossings::compare(&items, &answer.blocks)
    } else {
        Vec::new()
    };
    answer.items = Some(Compared {
        items,
        crossings,
        old_names,
        new_names,
    });
    write(&answer, all, format, out)
}

impl<'a> Answer<'a> {
    /// The answer for builds whose headers `old` and `new` read, before
    /// their items are compared.
    fn new(
        old_path: &'a Path,
        new_path: &'a Path,
        old: &'a LoadedMemory<'a>,
        new: &'a LoadedMemory<'a>,
        same_memory: bool,
    ) -> Answer<'a> {
        Answer {
            old_path,
            new_path,
            old,
            new,
            same_memory,
            blocks: blocks::shared_blocks(old, new, same_memory),
            sections_that_differ: if same_memory {
                Vec::new()
            } else {
                blocks::sections_that_differ(old, new)
            },
            items: None,
        }
    }
}

impl Build {
    /// Loads the file at `path`, whose bytes are `data`, and builds its
    /// graph, and with `tree`, its dominator tree, when elfq can read its
    /// code.
    fn load(path: &Path, data: Vec<u8>, tree: bool) -> Result<Build> {
        let binary =
            Binary::from_vec(data).with_context(|| format!("cannot load {}", path.display()))?;
        let graph = Graph::build(&binary);
        let tree = (tree && arch::is_supported(binary.architecture, binary.thumb))
            .then(|| DominatorTree::build(&binary, &graph, None));
        Ok(Build {
            binary,
            graph,
            tree,
        })
    }
}

/// Writes the answer, and says what it means for the exit status.
fn write(answer: &Answer, all: bool, format: Format, out: &mut dyn Write) -> Result<Outcome> {
    match format {
        Format::Json => json::write(answer, out)?,
        Format::Text | Format::Tree => text::write(answer, all, out)?,
    }
    Ok(if answer.same_memory {
        Outcome::Answered
    } else {
        Outcome::Differs
    })
}

/// Refuses files that diff can't compare: one without section headers, one
/// that puts nothing in memory, a separate debug file, and files for two
/// architectures.
fn check(old_path: &Path, old: &LoadedMemory, new_path: &Path, new: &LoadedMemory) -> Result<()> {
    for (path, memory) in [(old_path, old), (new_path, new)] {
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
    Ok(())
}

/// Prints `warnings` about the file at `path`, as `super::write_warnings`
/// does, but with the file's name, since there are two, and leaving out
/// those already printed, in `warned`.
fn write_warnings(path: &Path, warnings: &[String], warned: &[String]) {
    for warning in warnings.iter().filter(|warning| !warned.contains(warning)) {
        let _ = writeln!(std::io::stderr(), "warning: {}: {warning}", path.display());
    }
}

/// `work(old)` and `work(new)`, the second on a thread of its own, since
/// each takes a while for a large file, or when no thread can start, after
/// the first.
fn on_both<A: Send, T: Send>(old: A, new: A, work: impl Fn(A) -> T + Sync) -> (T, T) {
    // The thread takes `new` from here, where it stays if none starts.
    let new = Mutex::new(Some(new));
    let take = || {
        new.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .expect("only one of the thread and the fallback takes it")
    };
    thread::scope(|scope| {
        let thread = thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn_scoped(scope, || work(take()));
        let old = work(old);
        let new = match thread {
            Ok(thread) => thread
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(_) => work(take()),
        };
        (old, new)
    })
}

/// Reads the headers of the file at `path`, whose bytes are `data`.
fn read_memory<'data>(path: &Path, data: &'data [u8]) -> Result<LoadedMemory<'data>> {
    LoadedMemory::read(data).with_context(|| format!("cannot load {}", path.display()))
}
