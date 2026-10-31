//! `elfq FILE startup`: what the reset code of Cortex-M firmware does
//! before `main`, in the order it runs, and whether that can go wrong:
//! code that runs from a load image before the startup code copies it
//! there, a load image that it doesn't copy, copies short or from
//! elsewhere, and a `.bss` that it doesn't zero.
//!
//! The answer is worked out in this order:
//!
//! - `start.rs` finds the reset handler, and `main`: its symbol, where
//!   debug info says a compiler inlined it, or in a stripped file, what the
//!   reset handler calls last.
//! - `arch::startup` walks the code from the reset handler to `main`.
//! - `steps.rs` reads the walk's events as steps: each copy of a load
//!   image, each zeroing of a section without contents, matched by the
//!   numbers a call passes, the calls around them, and the code that elfq
//!   can't follow, which may set up any section not set up yet.
//! - `problems.rs` works out the state of each section when `main` starts,
//!   what runs from a load image before it is copied, the problems and the
//!   notes.
//! - `text.rs` and `json.rs` write the answer.

mod json;
mod problems;
mod start;
mod steps;
mod text;

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Result, bail};
use object::Architecture;

use super::{Kept, Outcome, check_code_is_readable};
use crate::arch::startup::{DEPTH, End, Walk};
use crate::binary::{Binary, ItemKind};
use crate::cli::Format;
use crate::graph::{Graph, Node, VeneerTarget, Veneers};
use problems::Judged;
use start::{Main, Reset};
use steps::{Events, Memory, Step};

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
    let Ok(Some(header)) = Header::read(path) else {
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
    let what = what.or(match header.kind {
        ET_REL => Some("an object file, not a linked program"),
        _ if header.interpreter => Some("a program that an operating system loads"),
        ET_DYN => Some("a shared library"),
        _ if header.dynamic => Some("a program that an operating system loads"),
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

// The ELF header's and program headers' numbers that `check_header` reads.
const EM_386: u16 = 3;
const EM_ARM: u16 = 40;
const EM_X86_64: u16 = 62;
const EM_AARCH64: u16 = 183;
const ET_REL: u16 = 1;
const ET_DYN: u16 = 3;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;

/// What [`check_header`] reads of a file.
struct Header {
    /// `e_machine`.
    machine: u16,
    /// `e_type`.
    kind: u16,
    /// Whether a program header names a program interpreter, as a
    /// program that an operating system loads has.
    interpreter: bool,
    /// Whether a program header gives a dynamic section.
    dynamic: bool,
    has_sections: bool,
}

impl Header {
    /// The header of the ELF file at `path`, or `None` when it isn't one.
    fn read(path: &Path) -> std::io::Result<Option<Header>> {
        let mut file = File::open(path)?;
        let mut ident = [0; 64];
        let read = file.read(&mut ident)?;
        if read < 52 || ident[..4] != *b"\x7fELF" {
            return Ok(None);
        }
        let wide = match ident[4] {
            1 => false,
            2 => true,
            _ => return Ok(None),
        };
        let little = match ident[5] {
            1 => true,
            2 => false,
            _ => return Ok(None),
        };
        if wide && read < 64 {
            return Ok(None);
        }
        let number = |bytes: &[u8]| -> u64 {
            let mut value = 0;
            for (n, &byte) in bytes.iter().enumerate() {
                let shift = if little { n } else { bytes.len() - 1 - n };
                value |= u64::from(byte) << (8 * shift);
            }
            value
        };
        let half = |at: usize| number(&ident[at..at + 2]) as u16;
        let (program_headers, section_headers, sizes) = if wide {
            (number(&ident[32..40]), number(&ident[40..48]), 54)
        } else {
            (number(&ident[28..32]), number(&ident[32..36]), 42)
        };
        let (program_header_size, programs) = (half(sizes), half(sizes + 2));
        let (section_header_size, sections) = (half(sizes + 4), half(sizes + 6));
        let mut header = Header {
            machine: half(18),
            kind: half(16),
            interpreter: false,
            dynamic: false,
            has_sections: section_headers != 0,
        };
        let mut word = [0; 8];
        // With more sections than e_shnum holds, it is 0, and the first
        // section header's size gives the number.
        if sections == 0 && header.has_sections {
            let size = if wide { 8 } else { 4 };
            let at = section_headers.checked_add(if wide { 32 } else { 20 });
            header.has_sections = section_header_size > 0
                && at.is_some_and(|at| file.seek(SeekFrom::Start(at)).is_ok())
                && file.read_exact(&mut word[..size]).is_ok()
                && number(&word[..size]) != 0;
        }
        // 0xffff is a number of program headers too large for e_phnum.
        if programs != 0xffff {
            for n in 0..u64::from(programs) {
                let Some(at) = program_headers.checked_add(n * u64::from(program_header_size))
                else {
                    break;
                };
                if file.seek(SeekFrom::Start(at)).is_err()
                    || file.read_exact(&mut word[..4]).is_err()
                {
                    break;
                }
                match number(&word[..4]) as u32 {
                    PT_INTERP => header.interpreter = true,
                    PT_DYNAMIC => header.dynamic = true,
                    _ => {}
                }
            }
        }
        Ok(Some(header))
    }
}

/// Whether `elfq FILE startup` reads the file at `path`, loaded as
/// `binary`, and has something to say about it, for a Try line: Cortex-M
/// firmware with a load image or a section without contents, whose reset
/// handler elfq finds.
pub(super) fn reads(path: &Path, binary: &Binary) -> bool {
    binary.architecture == Architecture::Arm
        && check_code_is_readable(binary).is_ok()
        && !Memory::new(binary).ram.is_empty()
        && check_header(path).is_ok()
        && Reset::find(path, binary).is_ok()
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
    check_code_is_readable(binary)?;
    if binary.architecture != Architecture::Arm {
        bail!("{} isn't Arm code; {READS}", path.display());
    }
    let reset = Reset::find(path, binary)?;
    let graph = Kept::new(Graph::build(binary));
    let firmware = Firmware::new(binary, &graph);
    let memory = Memory::new(binary);
    let main = start::find_main(&firmware, &memory, &reset);
    let walk = reset.walk(binary, main.map(|main| main.address), DEPTH);
    let events = Events::new(&firmware, &walk.events);
    let steps = steps::build(&firmware, &memory, &events, &reset, main, walk.end);
    let judged = problems::judge(
        &firmware, &memory, &events, &reset, main, &steps, walk.end, all,
    );
    let answer = Answer {
        path,
        firmware: &firmware,
        memory: &memory,
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
    memory: &'a Memory<'a>,
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

/// The file, with what tells where its calls go: its graph, and its
/// veneers.
struct Firmware<'a> {
    binary: &'a Binary,
    graph: &'a Graph,
    veneers: Veneers<'a>,
}

impl<'a> Firmware<'a> {
    fn new(binary: &'a Binary, graph: &'a Graph) -> Firmware<'a> {
        Firmware {
            binary,
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
