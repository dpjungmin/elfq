//! `elfq OLD diff NEW`: whether two builds of a program load the same
//! memory, and if not, in which blocks of memory the bytes differ, and
//! which functions and data objects moved to another block, are new or
//! gone, changed size, or kept their size but changed, each with what keeps
//! it.
//!
//! diff first reads only the files' headers and the bytes that their
//! sections put in memory (see [`LoadedMemory`]), so it answers at once
//! when the memory is the same, even for a large file. The blocks are
//! worked out once over both files (see `blocks.rs`). When the memory
//! differs, and both files have a symbol table, it loads both, each with
//! its graph and, unless either is a shared library, its dominator tree,
//! pairs their items (`pairing.rs`) and works out what changed
//! (`changes.rs`), with which items of the same size are edited
//! (`edited.rs`), and which calls and other uses between blocks are new or
//! gone (`crossings.rs`). `rows.rs` picks the rows of the MOVED and CHANGE
//! tables from what changed, for the text, and `text.rs` and `json.rs`
//! write the answer.
//!
//! Names mean the same across these files. The two files are the old and
//! the new *build* ([`Build`]), and `old` and `new` name what is each
//! build's. What a build loads ([`LoadedMemory`]) is its `old_memory` or
//! `new_memory` in [`Answer`] and in `run`, `answer_bytes_only` and
//! `answer_items`, which make the answer, and `old` or `new` elsewhere, as
//! in `check` and `blocks.rs`. Two items that are paired, one in each
//! build, are *partners*. A *change* is an item that changed ([`Change`]),
//! and `at` is its place in `Items::changes`. A *delta* is how many bytes
//! something grew by, less than 0 when it shrank: a change's item, a block,
//! a section or a row of CHANGE. An item's place in its build's items is
//! its `index`, or in `edited.rs`, `old_item` or `new_item`, and a block's
//! place in `Answer::blocks` is its `block`.

mod blocks;
mod changes;
mod crossings;
mod edited;
mod json;
mod pairing;
mod rows;
mod text;

use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::thread;

use anyhow::{Context, Result, bail};
use object::Architecture;

use super::kept::Kept;
use super::symbol::Names;
use super::text::architecture_name;
use super::{Outcome, startup};
use crate::STACK_SIZE;
use crate::binary::{Binary, FileKind, LoadedMemory, same_memory};
use crate::cli::Format;
use crate::graph::{DominatorTree, Graph};
use blocks::{Block, SectionDiffers};
use changes::{Change, Items, Status};
use crossings::Crossing;

/// What diff found, for the text and the JSON to write.
struct Answer<'a> {
    old_path: &'a Path,
    new_path: &'a Path,
    /// What the old build loads, as its headers say. `compared` holds the
    /// builds themselves, when diff loads them.
    old_memory: &'a LoadedMemory<'a>,
    /// What the new build loads.
    new_memory: &'a LoadedMemory<'a>,
    /// Whether both builds load the same bytes at the same addresses.
    same_memory: bool,
    blocks: Vec<Block>,
    /// The sections whose address, size, load image or bytes differ, or
    /// that only one build has, in address order: none when the memory is
    /// the same.
    sections_that_differ: Vec<SectionDiffers<'a>>,
    /// The builds' items, paired, when the memory differs and both builds
    /// have a symbol table.
    compared: Option<Compared<'a>>,
    /// Whether the Try lines suggest `elfq NEW startup` (see
    /// [`suggest_startup`]).
    try_startup: bool,
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

/// What `elfq OLD diff NEW` asks for: the two files, as given, whether to
/// show every row and name, and in which format.
#[derive(Clone, Copy)]
struct Request<'a> {
    old_path: &'a Path,
    new_path: &'a Path,
    all: bool,
    format: Format,
}

/// Answers `elfq OLD diff NEW`: [`Outcome::Answered`] when both load the
/// same memory, and [`Outcome::Differs`] when they don't. It reads both
/// files, warns about them and refuses those it can't compare, and loads
/// both builds when it compares their items too.
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
    let old_memory = read_memory(old_path, &old_data)?;
    let new_memory = read_memory(new_path, &new_data)?;
    for (path, memory) in [(old_path, &old_memory), (new_path, &new_memory)] {
        write_warnings(path, &memory.warnings, &[]);
    }
    check(old_path, &old_memory, new_path, &new_memory)?;
    let request = Request {
        old_path,
        new_path,
        all,
        format,
    };
    let same_memory = same_memory(&old_memory, &new_memory);
    if same_memory || !old_memory.has_symbols || !new_memory.has_symbols {
        return answer_bytes_only(
            request,
            &old_memory,
            &new_memory,
            &new_data,
            same_memory,
            out,
        );
    }
    // elfq can't yet tell what keeps items in a shared library, and the
    // rows say what keeps items in both builds or in neither.
    let trees = ![old_memory.kind, new_memory.kind].contains(&FileKind::SharedLibrary);
    // The builds take the files' bytes, which the headers read above
    // borrow, and the headers are read again from them.
    let warned = [old_memory.warnings, new_memory.warnings];
    let (old_build, new_build) = on_both(
        (old_path, old_data),
        (new_path, new_data),
        |(path, data)| Build::load(path, data, trees),
    );
    let (old_build, new_build) = (Kept::new(old_build?), Kept::new(new_build?));
    let old_memory = read_memory(old_path, old_build.binary.file_data())?;
    let new_memory = read_memory(new_path, new_build.binary.file_data())?;
    for ((path, build), warned) in [(old_path, &old_build), (new_path, &new_build)]
        .into_iter()
        .zip(&warned)
    {
        write_warnings(path, &build.binary.warnings, warned);
    }
    answer_items(
        request,
        &old_memory,
        &new_memory,
        &old_build,
        &new_build,
        out,
    )
}

/// Answers from the builds' bytes alone, as `old_memory` and `new_memory`
/// read them, when both load the same memory, as `same_memory` says, or
/// either has no symbol table. The new build is loaded from `new_data`,
/// its file's bytes, only to ask whether startup reads it.
fn answer_bytes_only(
    request: Request,
    old_memory: &LoadedMemory,
    new_memory: &LoadedMemory,
    new_data: &[u8],
    same_memory: bool,
    out: &mut dyn Write,
) -> Result<Outcome> {
    let blocks = blocks::shared_blocks(old_memory, new_memory, same_memory);
    let sections_that_differ = if same_memory {
        Vec::new()
    } else {
        blocks::sections_that_differ(old_memory, new_memory)
    };
    // The new build is loaded only when its headers say that startup may
    // read it.
    let try_startup = suggest_startup(request.format, &sections_that_differ, &[], || {
        new_memory.architecture == Architecture::Arm
            && startup::check_header(request.new_path).is_ok()
            && Binary::parse(new_data).is_ok_and(|binary| startup::reads(request.new_path, &binary))
    });
    let answer = Answer {
        old_path: request.old_path,
        new_path: request.new_path,
        old_memory,
        new_memory,
        same_memory,
        blocks,
        sections_that_differ,
        compared: None,
        try_startup,
    };
    write(&answer, request.all, request.format, out)
}

/// Answers from the items of `old_build` and `new_build` as well as their
/// bytes, as `old_memory` and `new_memory` read them, when the builds load
/// different memory and both have a symbol table.
fn answer_items(
    request: Request,
    old_memory: &LoadedMemory,
    new_memory: &LoadedMemory,
    old_build: &Build,
    new_build: &Build,
    out: &mut dyn Write,
) -> Result<Outcome> {
    let old_names = Kept::new(Names::new(&old_build.binary));
    let new_names = Kept::new(Names::new(&new_build.binary));
    let blocks = blocks::shared_blocks(old_memory, new_memory, false);
    let sections_that_differ = blocks::sections_that_differ(old_memory, new_memory);
    let items = changes::compare(old_build, new_build, &blocks);
    let crossings = if blocks.len() > 1 {
        crossings::compare(&items, &blocks)
    } else {
        Vec::new()
    };
    let try_startup = suggest_startup(
        request.format,
        &sections_that_differ,
        &items.changes,
        || startup::reads(request.new_path, &new_build.binary),
    );
    let answer = Answer {
        old_path: request.old_path,
        new_path: request.new_path,
        old_memory,
        new_memory,
        same_memory: false,
        blocks,
        sections_that_differ,
        compared: Some(Compared {
            items,
            crossings,
            old_names,
            new_names,
        }),
        try_startup,
    };
    write(&answer, request.all, request.format, out)
}

/// Whether the Try lines suggest `elfq NEW startup`: when a section with a
/// load image in either build differs, or one of `changes` moved an item
/// to another block, either of which may change what the new build's
/// startup code must copy before it runs what, and `reads` says that
/// startup reads the new build. The JSON has no Try lines, and `reads`,
/// which may load the new build, runs only when the rest holds.
fn suggest_startup(
    format: Format,
    sections_that_differ: &[SectionDiffers],
    changes: &[Change],
    reads: impl FnOnce() -> bool,
) -> bool {
    format != Format::Json
        && ((sections_that_differ.iter()).any(|differs| differs.has_load_image)
            || (changes.iter()).any(|change| change.status() == Status::Moved))
        && reads()
}

impl Build {
    /// Loads the file at `path`, whose bytes are `data`, and builds its
    /// graph, and with `tree`, its dominator tree, when elfq can read its
    /// code.
    fn load(path: &Path, data: Vec<u8>, tree: bool) -> Result<Build> {
        let binary =
            Binary::from_vec(data).with_context(|| format!("cannot load {}", path.display()))?;
        let graph = Graph::build(&binary);
        let tree = (tree && binary.instruction_set().is_some())
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

#[cfg(test)]
mod tests {
    use super::changes::PartnerStatus;
    use super::pairing::PairedBy;
    use super::*;

    #[test]
    fn startup_is_suggested_by_one_rule_that_asks_last_whether_it_reads() {
        let data = |has_load_image| SectionDiffers {
            name: ".data",
            only_load_address: false,
            has_load_image,
        };
        let partners = |status| Change::Partners {
            old: 0,
            new: 0,
            status,
            paired_by: PairedBy::Name,
        };
        let (moved, grew) = (
            partners(PartnerStatus::Moved),
            partners(PartnerStatus::Grew),
        );
        // Whether the rule suggests startup when `reads` says yes, and
        // whether it asked.
        let suggests = |format, differ: &[SectionDiffers], changes: &[Change]| {
            let mut asked = false;
            let suggested = suggest_startup(format, differ, changes, || {
                asked = true;
                true
            });
            (suggested, asked)
        };
        assert_eq!(suggests(Format::Text, &[data(true)], &[]), (true, true));
        assert_eq!(
            suggests(Format::Tree, &[data(false)], &[moved]),
            (true, true)
        );
        assert_eq!(
            suggests(Format::Text, &[data(false)], &[grew]),
            (false, false)
        );
        assert_eq!(suggests(Format::Text, &[], &[]), (false, false));
        assert_eq!(
            suggests(Format::Json, &[data(true)], &[moved]),
            (false, false)
        );
        // A build that startup doesn't read gets no suggestion.
        assert!(!suggest_startup(Format::Text, &[data(true)], &[], || false));
    }
}
