//! The JSON of `startup`: the reset handler and `main`, then the steps,
//! the sections, the problems, the notes, and what must stay out of each
//! load image that holds code, one per line.

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::problems::{EarlyRun, Problem, SectionStatus};
use super::ram_sections::RamSections;
use super::start::MainFrom;
use super::steps::number;
use super::text;
use super::{Answer, Ending, Facts, Firmware};
use crate::commands::json::{self as house, Item, address};
use crate::graph::Node;

#[derive(Serialize)]
struct Startup<'a> {
    file: String,
    /// The reset handler, or without a vector table, or one whose reset
    /// handler is in no function, the function at the entry point.
    entry: Option<Item<'a>>,
    /// The function that is `main`, when the walk got to it; `null` for a
    /// `main` inlined into the reset handler, when startup ends at the
    /// reset handler's endless loop, or when the walk ends before.
    main: Option<Item<'a>>,
    /// Where startup ends: `main`, where the code of an inlined `main`
    /// starts, or the head of the reset handler's endless loop; `null` when
    /// the walk ends before.
    main_at: Option<String>,
    /// How elfq knows where `main` is: `symbol`, `debug info`, `guess` or
    /// `loop`; `null` when the walk ends before.
    main_from: Option<&'static str>,
    /// Whether elfq followed all the code before `main`.
    complete: bool,
    /// How many of the problems are certain.
    problems_certain: usize,
    steps: Vec<StepRow<'a>>,
    sections: Vec<Section<'a>>,
    problems: Vec<ProblemRow<'a>>,
    notes: Vec<Note<'a>>,
    keep_out: Vec<KeepOut<'a>>,
}

#[derive(Serialize)]
struct StepRow<'a> {
    step: usize,
    site: String,
    /// `copy`, `zero`, `call` or `unread`.
    does: &'static str,
    /// `call`, `tail call`, `inlined`, `loop`, `store`, `register call`,
    /// `supervisor call`, `conditional call`, `deep call` or `stop`.
    how: &'static str,
    register: Option<String>,
    function: Option<Item<'a>>,
    veneer: Option<String>,
    inside: Option<Item<'a>>,
    from: Option<String>,
    to: Option<String>,
    size: Option<u64>,
    sections: Vec<&'a str>,
    table: Option<String>,
    may_set_up: Vec<&'a str>,
}

#[derive(Serialize)]
struct Section<'a> {
    name: &'a str,
    start: String,
    end: String,
    size: u64,
    load_address: Option<String>,
    status: &'static str,
    step: Option<usize>,
    bytes: Option<u64>,
}

#[derive(Serialize)]
struct ProblemRow<'a> {
    kind: &'static str,
    certain: bool,
    steps: Vec<usize>,
    sections: Vec<&'a str>,
    items: Vec<Item<'a>>,
    chain: Vec<String>,
    before_step: Option<usize>,
    bytes: Option<u64>,
    from: Option<String>,
    sites: Vec<String>,
    text: String,
}

#[derive(Serialize)]
struct Note<'a> {
    kind: &'static str,
    sections: Vec<&'a str>,
    /// Always empty: the key keeps a note's shape as docs/design.md gives
    /// it.
    items: Vec<Item<'a>>,
    text: String,
}

/// A load image that holds code, with the functions that startup runs
/// before it copies it, which must stay out of it.
#[derive(Serialize)]
struct KeepOut<'a> {
    section: &'a str,
    items: Vec<Item<'a>>,
}

pub(super) fn write(answer: &Answer, out: &mut dyn Write) -> Result<()> {
    let firmware = answer.facts.firmware;
    let ram = answer.facts.ram;
    let (main, main_at, main_from) = match answer.facts.ending() {
        Ending::EntryIsMain(main) | Ending::Main(main) => (
            (main.function()).and_then(|function| item(firmware, function)),
            Some(address(main.address)),
            Some(main.from.field()),
        ),
        Ending::EndlessLoop(head) => (None, Some(address(head)), Some(MainFrom::Loop.field())),
        Ending::Walk(_) => (None, None, None),
    };
    let notes = (answer.judged.notes.iter())
        .map(|note| Note {
            kind: note.field(),
            sections: names(ram, note.sections()),
            items: Vec::new(),
            text: text::note_text(answer, note),
        })
        .collect();
    let keep_out = (answer.judged.keep_out.iter())
        .map(|keep_out| KeepOut {
            section: ram.name(keep_out.section),
            items: (keep_out.items.iter())
                .map(|&index| Item::new(firmware.binary, Node::Item(index)))
                .collect(),
        })
        .collect();
    let value = Startup {
        file: answer.path.display().to_string(),
        entry: item(firmware, answer.facts.reset.address),
        main,
        main_at,
        main_from,
        complete: answer.judged.complete,
        problems_certain: (answer.judged.problems.iter())
            .filter(|p| p.certain())
            .count(),
        steps: step_rows(answer),
        sections: section_rows(answer),
        problems: problem_rows(answer),
        notes,
        keep_out,
    };
    house::write(&value, out)
}

/// The item whose bytes include `address`.
fn item<'a>(firmware: &Firmware<'a>, address: u64) -> Option<Item<'a>> {
    (firmware.item(address)).map(|index| Item::new(firmware.binary, Node::Item(index)))
}

/// The names of `sections`.
fn names<'a>(ram: &RamSections<'a>, sections: &[usize]) -> Vec<&'a str> {
    sections.iter().map(|&section| ram.name(section)).collect()
}

/// A row for each step.
fn step_rows<'a>(answer: &Answer<'a>) -> Vec<StepRow<'a>> {
    let Facts { firmware, ram, .. } = *answer.facts;
    (answer.steps.iter().enumerate())
        .map(|(index, step)| {
            let region = step.region.as_ref();
            StepRow {
                step: number(index),
                site: address(step.site),
                does: step.does.word(),
                how: step.way.field(),
                register: step.way.register().map(text::register_name),
                function: step.function.and_then(|function| item(firmware, function)),
                veneer: step.veneer.map(address),
                inside: item(firmware, step.inside),
                from: region.and_then(|region| region.from).map(address),
                to: region.map(|region| address(region.to)),
                size: region.map(|region| region.size),
                sections: (step.covers().iter())
                    .map(|cover| ram.name(cover.section))
                    .collect(),
                table: step.table.map(address),
                may_set_up: names(ram, &step.may),
            }
        })
        .collect()
}

/// A row for each section that startup code sets up, with its status.
fn section_rows<'a>(answer: &Answer<'a>) -> Vec<Section<'a>> {
    let ram = answer.facts.ram;
    (ram.sections.iter().copied())
        .map(|section| {
            let SectionStatus {
                status,
                step,
                bytes,
            } = answer.judged.status(section);
            Section {
                name: ram.name(section),
                start: address(ram.section(section).address),
                end: address(ram.end(section)),
                size: ram.section(section).size,
                load_address: ram
                    .is_load(section)
                    .then(|| address(ram.load_address(section))),
                status: status.field(),
                step: step.map(number),
                bytes,
            }
        })
        .collect()
}

/// A row for each problem, with its sentence.
fn problem_rows<'a>(answer: &Answer<'a>) -> Vec<ProblemRow<'a>> {
    let Facts { firmware, ram, .. } = *answer.facts;
    (answer.judged.problems.iter())
        .map(|p| {
            let mut row = ProblemRow {
                kind: p.field(),
                certain: p.certain(),
                steps: p.steps().iter().map(|&index| number(index)).collect(),
                sections: names(ram, p.sections()),
                items: Vec::new(),
                chain: Vec::new(),
                before_step: None,
                bytes: None,
                from: None,
                sites: Vec::new(),
                text: text::sentence(answer, p),
            };
            // The keys of each kind; the rest stay empty or null.
            match p {
                Problem::RunsBeforeCopy(run) => {
                    row.items = (run.functions.iter())
                        .filter_map(|early| item(firmware, early.function))
                        .collect();
                    row.chain = chain(firmware, run);
                    row.before_step = run.before.map(number);
                    row.sites = house::addresses(&run.sites);
                }
                Problem::PartlyCopied { bytes, .. } | Problem::PartlyZeroed { bytes, .. } => {
                    row.bytes = Some(*bytes);
                }
                Problem::CopiedFromElsewhere { from, .. } => row.from = Some(address(*from)),
                Problem::NotCopied { .. }
                | Problem::NotZeroed { .. }
                | Problem::CantFollow { .. } => {}
            }
            row
        })
        .collect()
}

/// For `run`, code that runs before its copy in a step, the names of the
/// functions that the step's call goes through to the first of its
/// functions, but for veneers.
fn chain(firmware: &Firmware, run: &EarlyRun) -> Vec<String> {
    let Some(first) = run.functions.first() else {
        return Vec::new();
    };
    if run.steps.is_empty() {
        return Vec::new();
    }
    let chain = &first.chain;
    (chain[..chain.len() - 1].iter())
        .filter(|&&a| !firmware.is_veneer(a))
        .map(|&a| firmware.name(a))
        .collect()
}
