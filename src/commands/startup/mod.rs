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
//! walk found (`arch::startup::Loop`). A step goes by its index in the
//! steps, as in `Setup::step`, `Problem::steps` and `Problem::before`,
//! and where elfq writes it out, by its number, from 1 (`steps::number`).
//! What every stage reads once the walk is done, from the file to how the
//! walk ended, is in `Facts`, and where startup ends, which the text and
//! the JSON say, is its `Ending`.
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

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};
use object::elf::{EM_386, EM_AARCH64, EM_ARM, EM_X86_64, ET_DYN, ET_REL};

use super::Outcome;
use super::checks::check_code_is_readable;
use super::kept::Kept;
use crate::arch::startup::{Code, DEPTH, End};
use crate::binary::{Binary, ElfHeader, ItemKind};
use crate::cli::Format;
use crate::graph::{Graph, Node, VeneerTarget, Veneers};
use events::Events;
use problems::Judged;
use ram_sections::RamSections;
use start::{Main, MainFrom, Reset};
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
    let events = Events::new(&firmware, reset.address, &walk.events);
    let facts = Facts {
        firmware: &firmware,
        ram: &ram,
        reset: &reset,
        main,
        events: &events,
        end: walk.end,
    };
    let steps = steps::build(&facts);
    let judged = problems::judge(&facts, &steps);
    let answer = Answer {
        path,
        facts: &facts,
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

/// What startup knows once it has walked the code, which the steps, the
/// problems and the answer read.
#[derive(Clone, Copy)]
struct Facts<'a> {
    firmware: &'a Firmware<'a>,
    ram: &'a RamSections<'a>,
    reset: &'a Reset,
    main: Option<Main>,
    events: &'a Events<'a>,
    /// How the walk ended.
    end: End,
}

impl Facts<'_> {
    /// Where startup ends, from how the walk ended and where `main` is.
    fn ending(&self) -> Ending {
        match (self.end, self.main) {
            (End::Main(_), Some(main)) if main.from == MainFrom::Loop => {
                Ending::EndlessLoop(main.address)
            }
            (End::Main(_), Some(main)) if main.address == self.reset.address => {
                Ending::EntryIsMain(main)
            }
            (End::Main(_), Some(main)) => Ending::Main(main),
            (end, _) => Ending::Walk(end),
        }
    }
}

/// Where startup ends (see [`Facts::ending`]), as the lead and the JSON's
/// `main` keys say.
#[derive(Debug, Clone, Copy)]
enum Ending {
    /// The processor starts at `main`, so no startup code runs before it.
    EntryIsMain(Main),
    /// The walk got to `main`: a call to it, or the code of a `main` that a
    /// compiler inlined into the reset handler.
    Main(Main),
    /// The head of the reset handler's endless loop, where its startup
    /// ends: the reset handler is the program ([`MainFrom::Loop`]).
    EndlessLoop(u64),
    /// The walk ended before `main`, as its end says.
    Walk(End),
}

/// Everything startup found, for the text and the JSON to write.
struct Answer<'a> {
    path: &'a Path,
    facts: &'a Facts<'a>,
    steps: &'a [Step],
    judged: &'a Judged,
    all: bool,
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

    /// Whether a veneer starts at `address`.
    fn is_veneer(&self, address: u64) -> bool {
        self.veneer_target(address).is_some()
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
    fn resolve(&self, target: u64) -> CallTarget {
        match self.veneer_target(target) {
            Some(function) => CallTarget {
                function,
                veneer: Some(target),
            },
            None => CallTarget {
                function: target,
                veneer: None,
            },
        }
    }

    /// The functions that the function at `address` calls, or jumps to,
    /// each once, in the order it first does.
    fn callees(&self, address: u64) -> Vec<u64> {
        let mut seen = HashSet::new();
        (self.calls_from(address))
            .map(|call| call.callee)
            .filter(|&callee| seen.insert(callee))
            .collect()
    }

    /// Where the function at `address` calls, or jumps to, the function at
    /// `callee`.
    fn call_sites(&self, address: u64, callee: u64) -> Vec<u64> {
        (self.calls_from(address))
            .filter(|call| call.callee == callee)
            .map(|call| call.site)
            .collect()
    }

    /// The calls and jumps from the function at `address` to functions, in
    /// the order of their sites.
    fn calls_from(&self, address: u64) -> impl Iterator<Item = Call> + 'a {
        let references = match self.function_at(address) {
            Some(item) => self.graph.references_from(item),
            None => &[],
        };
        let binary = self.binary;
        references
            .iter()
            .filter_map(move |reference| match reference.to {
                Node::Item(to)
                    if reference.kind.is_call() && binary.items[to].kind == ItemKind::Function =>
                {
                    Some(Call {
                        callee: binary.items[to].address,
                        site: reference.site,
                    })
                }
                Node::Item(_) | Node::Import(_) => None,
            })
    }
}

/// The address of the function whose code holds `address`, when a
/// function's does.
fn function_holding(binary: &Binary, address: u64) -> Option<u64> {
    let index = binary.item_at(address)?;
    let item = &binary.items[index];
    (item.kind == ItemKind::Function).then_some(item.address)
}

/// A call or jump from one function to another (see
/// [`Firmware::calls_from`]).
struct Call {
    /// The address of the function it goes to.
    callee: u64,
    /// Where the call is.
    site: u64,
}

/// Where a call goes (see [`Firmware::resolve`]).
#[derive(Debug, Clone, Copy)]
struct CallTarget {
    /// The function that runs: the one that a veneer at the call's target
    /// jumps on to, or else the target.
    function: u64,
    /// The veneer at the call's target, when there is one.
    veneer: Option<u64>,
}
