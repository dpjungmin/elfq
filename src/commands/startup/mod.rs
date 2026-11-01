//! `elfq FILE startup`: what the reset code of Cortex-M firmware does
//! before `main`, in the order it runs, and whether that can go wrong:
//! code that runs from a load image before the startup code copies it
//! there, a load image that it doesn't copy, copies short or from
//! elsewhere, and a `.bss` that it doesn't zero.
//!
//! The answer is worked out in this order:
//!
//! - `start.rs` finds the reset handler.
//! - `ram_sections.rs` lists the sections that startup code sets up: the
//!   load images and the sections without contents.
//! - `start.rs` then finds `main`: its symbol, where debug info says a
//!   compiler inlined it, or without either, what the reset handler calls
//!   last. For that it walks the code itself, up to three times: from the
//!   reset handler following no calls (`guess`), through the function
//!   that the reset handler calls last (`sets_up`), and from the reset
//!   handler as deep as the walk below (`c_runtime`).
//! - `arch::startup` walks the code from the reset handler to `main`, and
//!   `events.rs` works out what the steps read of each of its events.
//! - `steps.rs` reads the walk's events as steps: each copy of a load
//!   image, each zeroing of a section without contents, matched by the
//!   numbers a call passes or by the bytes that loops and stores copy or
//!   zero, which `pieces.rs` joins, the calls around them, and the code
//!   that elfq can't follow, which may set up any section not set up yet
//!   (`unread.rs`).
//! - `problems.rs` works out the state of each section when `main` starts,
//!   what runs from a load image before it is copied, the problems and the
//!   notes.
//! - `text.rs` and `json.rs` write the answer.
//!
//! The unit tests build their walks by hand, with `fixtures.rs`.
//!
//! Names mean the same across these files. `ram` is the sections that
//! startup code sets up (`RamSections`). `section` is a section, as an
//! index into `Binary::sections`; `event` is an event, as an index into
//! the walk's events, and `loop_event` a loop's; `l` is a loop that the
//! walk found (`arch::startup::Loop`). A step goes by its place in the
//! steps, as `s` or in fields such as `Setup::step` and `Problem::before`,
//! and by its `number`, from 1, which the text and the JSON print and
//! `Problem::steps` holds.
//!
//! docs/design.md's "Startup, end to end" follows one run through this
//! module, `arch::startup`, `arch::thumb::startup` and `binary::startup`.

mod events;
#[cfg(test)]
mod fixtures;
mod json;
mod pieces;
mod problems;
mod ram_sections;
mod start;
mod steps;
mod text;
mod unread;

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};
use object::elf::{EM_386, EM_AARCH64, EM_ARM, EM_X86_64, ET_DYN, ET_REL};

use super::Outcome;
use super::checks::check_code_is_readable;
use super::kept::Kept;
use crate::arch::startup::{Code, DEPTH, End, Walk};
use crate::binary::{Binary, ElfHeader, ItemKind};
use crate::cli::Format;
use crate::graph::{Graph, Node, VeneerTarget, Veneers};
use events::Events;
use problems::Judged;
use ram_sections::RamSections;
use start::{Main, Reset};
use steps::Step;

/// What startup refuses to read, and why: the end of each refusal.
const READS: &str = "startup reads the reset code of Cortex-M firmware";

/// Refuses a file that the ELF header and the program headers say isn't
/// Cortex-M firmware, before the file is loaded, so that a large program
/// for another processor costs nothing. A file that isn't an ELF file, or
/// that can't be read, gets the error that loading it gives, and so does
/// a pipe, which can be read only once.
pub(super) fn check_header(path: &Path) -> Result<()> {
    if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return Ok(());
    }
    let Ok(Some(header)) = ElfHeader::read(path) else {
        return Ok(());
    };
    let file = path.display();
    let what = match header.machine {
        EM_ARM => None,
        EM_X86_64 => Some("x86-64 code"),
        EM_386 => Some("32-bit x86 code"),
        EM_AARCH64 => Some("64-bit Arm code"),
        _ => Some("code for another processor"),
    };
    let what = what.or(match header.file_type {
        ET_REL => Some("an object file, not a linked program"),
        _ if header.has_interpreter => Some("a program that an operating system loads"),
        ET_DYN => Some("a shared library"),
        _ if header.has_dynamic => Some("a program that an operating system loads"),
        _ => None,
    });
    if let Some(what) = what {
        bail!("{file} is {what}; {READS}");
    }
    if !header.has_sections {
        bail!("{file} has no section headers, so startup can't tell what to copy and zero");
    }
    Ok(())
}

/// Whether `elfq FILE startup` reads the file at `path`, loaded as
/// `binary`, and has something to say about it, for a Try line: Cortex-M
/// firmware with a load image or a section without contents, whose reset
/// handler elfq finds.
pub(super) fn reads(path: &Path, binary: &Binary) -> bool {
    reset_handler(path, binary).is_ok()
        && !RamSections::new(binary).sections.is_empty()
        && check_header(path).is_ok()
}

/// Where the processor starts in the firmware at `path`, loaded as
/// `binary`, and its code as the walk reads it, or why startup refuses the
/// file once it is loaded: its code can't be read, it isn't Arm code, or
/// elfq finds no reset handler.
fn reset_handler<'a>(path: &Path, binary: &'a Binary) -> Result<(Reset, Code<'a>)> {
    check_code_is_readable(binary)?;
    let code = binary
        .instruction_set()
        .and_then(|set| set.startup_code(binary));
    let Some(code) = code else {
        bail!("{} isn't Arm code; {READS}", path.display());
    };
    Ok((Reset::find(path, binary)?, code))
}

/// Answers `elfq FILE startup`: [`Outcome::Problems`] when it finds a
/// problem, or code it can't follow that may be one.
pub(super) fn run(
    path: &Path,
    binary: &Binary,
    all: bool,
    format: Format,
    out: &mut dyn Write,
) -> Result<Outcome> {
    let (reset, code) = reset_handler(path, binary)?;
    let graph = Kept::new(Graph::build(binary));
    let firmware = Firmware::new(binary, code, &graph);
    let ram = RamSections::new(binary);
    let main = start::find_main(&firmware, &ram, &reset);
    let walk = reset.walk(code, main.map(|main| main.address), DEPTH);
    let events = Events::new(&firmware, &walk.events);
    let steps = steps::build(&firmware, &ram, &events, &reset, main, walk.end);
    let judged = problems::judge(
        &firmware, &ram, &events, &reset, main, &steps, walk.end, all,
    );
    let answer = Answer {
        path,
        firmware: &firmware,
        ram: &ram,
        reset: &reset,
        main,
        walk: &walk,
        events: &events,
        steps: &steps,
        judged: &judged,
        all,
    };
    match format {
        Format::Json => json::write(&answer, out)?,
        // `check_tree` refuses --tree for startup.
        Format::Text | Format::Tree => text::write(&answer, out)?,
    }
    Ok(if judged.problems.is_empty() {
        Outcome::Answered
    } else {
        Outcome::Problems
    })
}

/// Everything startup found, for the text and the JSON to write.
struct Answer<'a> {
    path: &'a Path,
    firmware: &'a Firmware<'a>,
    ram: &'a RamSections<'a>,
    reset: &'a Reset,
    main: Option<Main>,
    walk: &'a Walk,
    events: &'a Events<'a>,
    steps: &'a [Step],
    judged: &'a Judged,
    all: bool,
}

impl Answer<'_> {
    /// Whether the walk got to `main`, as against a loop that the reset
    /// handler runs for good, taken as where its startup ends.
    fn reached_main(&self) -> bool {
        matches!(self.walk.end, End::Main(_))
            && self
                .main
                .is_some_and(|main| main.from != start::MainFrom::Loop)
    }
}

/// The file, with its code as the walk reads it, and what tells where its
/// calls go: its graph, and its veneers.
struct Firmware<'a> {
    binary: &'a Binary,
    code: Code<'a>,
    graph: &'a Graph,
    veneers: Veneers<'a>,
}

impl<'a> Firmware<'a> {
    fn new(binary: &'a Binary, code: Code<'a>, graph: &'a Graph) -> Firmware<'a> {
        Firmware {
            binary,
            code,
            graph,
            veneers: Veneers::new(binary, graph),
        }
    }

    /// The item whose bytes include `address`.
    fn item(&self, address: u64) -> Option<usize> {
        self.binary.item_at(address)
    }

    /// The function that starts at `address`.
    fn function_at(&self, address: u64) -> Option<usize> {
        self.item(address).filter(|&index| {
            let item = &self.binary.items[index];
            item.address == address && item.kind == ItemKind::Function
        })
    }

    /// The name of the item whose bytes include `address`, or the address.
    fn name(&self, address: u64) -> String {
        match self.item(address) {
            Some(index) => self.binary.items[index].name.clone(),
            None => format!("{address:#x}"),
        }
    }

    /// Where a veneer at `address` jumps on to, when there is one.
    fn veneer_target(&self, address: u64) -> Option<u64> {
        let veneer = self.function_at(address)?;
        match self.veneers.target(veneer)? {
            VeneerTarget::Node(Node::Item(index)) => Some(self.binary.items[index].address),
            VeneerTarget::Node(Node::Import(_)) => None,
            VeneerTarget::Outside(address) => Some(address),
        }
    }

    /// Where a call to `target` goes: past a veneer there, to the function
    /// it jumps on to, with the veneer's address.
    fn resolve(&self, target: u64) -> (u64, Option<u64>) {
        match self.veneer_target(target) {
            Some(function) => (function, Some(target)),
            None => (target, None),
        }
    }

    /// The functions that the function at `address` calls, or jumps to,
    /// in the order it first does, each with where it does: as `callees`
    /// lists them.
    fn callees(&self, address: u64) -> Vec<(u64, Vec<u64>)> {
        let Some(item) = self.function_at(address) else {
            return Vec::new();
        };
        let mut callees: Vec<(u64, Vec<u64>)> = Vec::new();
        let mut index: HashMap<u64, usize> = HashMap::new();
        for reference in self.graph.references_from(item) {
            let Node::Item(to) = reference.to else {
                continue;
            };
            let to = &self.binary.items[to];
            if !reference.kind.is_call() || to.kind != ItemKind::Function {
                continue;
            }
            let row = *index.entry(to.address).or_insert_with(|| {
                callees.push((to.address, Vec::new()));
                callees.len() - 1
            });
            callees[row].1.push(reference.site);
        }
        callees
    }
}
