//! The JSON of `startup`: the reset handler and `main`, then the steps,
//! the sections, the problems and the notes, one per line.

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::Answer;
use super::problems::Kind;
use super::start::MainFrom;
use super::steps::Step;
use super::text;
use crate::arch::startup::End;
use crate::commands::json::{self as house, Item, address};
use crate::graph::Node;

#[derive(Serialize)]
struct Startup<'a> {
    file: String,
    /// The reset handler, or without a vector table, the function at the
    /// entry point.
    entry: Option<Item<'a>>,
    /// The function that is `main`, when the walk got to it; `null` for a
    /// `main` inlined into the reset handler.
    main: Option<Item<'a>>,
    /// Where startup ends: `main`, or the head of the reset handler's
    /// endless loop.
    main_at: Option<String>,
    /// How elfq knows where `main` is: `symbol`, `debug info`, `guess`, or
    /// `loop`.
    main_from: Option<&'static str>,
    /// Whether elfq followed all the code before `main`.
    complete: bool,
    /// How many of the problems are certain.
    problems_certain: usize,
    steps: Vec<StepRow<'a>>,
    sections: Vec<Section<'a>>,
    problems: Vec<ProblemRow<'a>>,
    notes: Vec<Note<'a>>,
}

#[derive(Serialize)]
struct StepRow<'a> {
    step: usize,
    site: String,
    /// `copy`, `zero`, `call` or `unread`.
    does: &'static str,
    /// `call`, `tail call`, `inlined`, `loop`, `register call`,
    /// `supervisor call` or `stop`.
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
    items: Vec<Item<'a>>,
    text: String,
}

pub(super) fn write(answer: &Answer, out: &mut dyn Write) -> Result<()> {
    let firmware = answer.firmware;
    let binary = firmware.binary;
    let memory = answer.memory;
    let item =
        |address: u64| (firmware.item(address)).map(|index| Item::new(binary, Node::Item(index)));
    let reached = answer.reached_main();
    let (main, main_at, main_from) = match answer.main {
        Some(main) if reached => (
            (main.from != MainFrom::DebugInfo)
                .then(|| item(main.address))
                .flatten(),
            Some(address(main.address)),
            Some(main.from.field()),
        ),
        Some(main) if main.from == MainFrom::Loop && matches!(answer.walk.end, End::Main(_)) => {
            (None, Some(address(main.address)), Some(main.from.field()))
        }
        _ => (None, None, None),
    };
    let names =
        |sections: &[usize]| -> Vec<&str> { sections.iter().map(|&x| memory.name(x)).collect() };
    let steps = (answer.steps.iter())
        .map(|step: &Step| StepRow {
            step: step.number,
            site: address(step.site),
            does: step.does.word(),
            how: step.way.field(),
            register: match step.way {
                super::steps::Way::RegisterCall(register) => Some(text::register_name(register)),
                _ => None,
            },
            function: step.function.and_then(item),
            veneer: step.veneer.map(address),
            inside: item(step.inside),
            from: step.from.map(address),
            to: step.to.map(address),
            size: step.size,
            sections: (step.covers.iter())
                .map(|cover| memory.name(cover.section))
                .collect(),
            table: None,
            may_set_up: names(&step.may),
        })
        .collect();
    let sections = (memory.ram.iter().copied())
        .map(|x| {
            let section = memory.section(x);
            let (status, step, bytes) = answer.judged.status(answer.steps, memory, x);
            Section {
                name: &section.name,
                start: address(section.address),
                end: address(memory.end(x)),
                size: section.size,
                load_address: memory.is_load(x).then(|| address(memory.load_address(x))),
                status: status.field(),
                step,
                bytes,
            }
        })
        .collect();
    let problems = (answer.judged.problems.iter())
        .map(|p| {
            let chain = match (p.kind, p.items.first()) {
                (Kind::RunsBeforeCopy, Some(first)) if !p.steps.is_empty() => {
                    let chain = &p.chains[first];
                    (chain[..chain.len() - 1].iter())
                        .filter(|&&a| firmware.veneer_target(a).is_none())
                        .map(|&a| firmware.name(a))
                        .collect()
                }
                _ => Vec::new(),
            };
            ProblemRow {
                kind: p.kind.field(),
                certain: p.certain,
                steps: p.steps.clone(),
                sections: names(&p.sections),
                items: p.items.iter().filter_map(|&a| item(a)).collect(),
                chain,
                before_step: p.before.map(|before| answer.steps[before].number),
                bytes: p.bytes,
                from: p.from.map(address),
                sites: house::addresses(&p.sites),
                text: text::sentence(answer, p),
            }
        })
        .collect();
    let notes = (answer.judged.notes.iter())
        .map(|note| Note {
            kind: note.kind,
            sections: names(&note.sections),
            items: Vec::new(),
            text: note.text.clone(),
        })
        .collect();
    let value = Startup {
        file: answer.path.display().to_string(),
        entry: item(answer.reset.address),
        main,
        main_at,
        main_from,
        complete: answer.judged.complete,
        problems_certain: answer.judged.problems.iter().filter(|p| p.certain).count(),
        steps,
        sections,
        problems,
        notes,
    };
    house::write(&value, out)
}
