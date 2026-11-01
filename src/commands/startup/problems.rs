//! What can go wrong in startup: the state of each section when `main`
//! starts, the problems, and the notes.
//!
//! A load image runs before its copy when code in it runs before the step
//! that copies it: the reset handler itself, or any function that a call
//! before the copy runs, at any depth, through calls and tail calls (not
//! through taken addresses, since a weak undefined function is at address
//! 0, which is in ITCM on many parts). The first such problem that no code
//! elfq can't follow comes before is certain, and nothing after it can be
//! counted on: the steps after it "may never run".

use std::collections::{HashMap, HashSet};

use object::SectionKind;

use super::Firmware;
use super::events::Events;
use super::ram_sections::RamSections;
use super::start::{Main, Reset};
use super::steps::{Does, Step};
use super::text;
use crate::arch::startup::{End, Event};
use crate::binary::ItemKind;
use crate::commands::text::names_text;

/// The state of a section when `main` starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Status {
    Copied,
    PartlyCopied,
    /// Copied from somewhere other than where the file stores it.
    CopiedFromElsewhere,
    Zeroed,
    PartlyZeroed,
    NotCopied,
    NotZeroed,
    /// Code elfq can't follow may set it up.
    Unknown,
    /// Only a step after a certain problem sets it up, which may never
    /// run.
    NotReached,
}

impl Status {
    pub fn field(self) -> &'static str {
        match self {
            Status::Copied => "copied",
            Status::PartlyCopied => "partly copied",
            Status::CopiedFromElsewhere => "copied from elsewhere",
            Status::Zeroed => "zeroed",
            Status::PartlyZeroed => "partly zeroed",
            Status::NotCopied => "not copied",
            Status::NotZeroed => "not zeroed",
            Status::Unknown => "unknown",
            Status::NotReached => "not reached",
        }
    }
}

/// What a problem is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// Code runs from a load image before startup code copies it.
    RunsBeforeCopy,
    NotCopied,
    /// `.bss` or `.sbss` isn't zeroed.
    NotZeroed,
    PartlyCopied,
    PartlyZeroed,
    CopiedFromElsewhere,
    /// Code elfq can't follow may set up a load image, `.bss` or `.sbss`.
    CantFollow,
}

impl Kind {
    pub fn field(self) -> &'static str {
        match self {
            Kind::RunsBeforeCopy => "runs before copy",
            Kind::NotCopied => "not copied",
            Kind::NotZeroed => "not zeroed",
            Kind::PartlyCopied => "partly copied",
            Kind::PartlyZeroed => "partly zeroed",
            Kind::CopiedFromElsewhere => "copied from elsewhere",
            Kind::CantFollow => "can't follow",
        }
    }
}

/// Where in the walk code runs from a load image: the reset handler's own
/// code, or the call at an event.
pub(super) const RESET: i64 = -1;

pub(super) struct Problem {
    pub kind: Kind,
    /// False for code elfq can't follow, which may be a problem or not, and
    /// for a use of a load image after such code.
    pub certain: bool,
    pub sections: Vec<usize>,
    /// The numbers of the steps it is about.
    pub steps: Vec<usize>,
    /// For code that runs before its copy: the functions in the load
    /// image.
    pub items: Vec<u64>,
    /// How the step gets to each of `items`: the functions from the one it
    /// calls to the item.
    pub chains: HashMap<u64, Vec<u64>>,
    /// The step that copies the section, as an index into the steps.
    pub before: Option<usize>,
    pub bytes: Option<u64>,
    pub from: Option<u64>,
    /// Where the code calls on into the load image.
    pub sites: Vec<u64>,
    /// Where in the walk the code runs from the load image: [`RESET`], or
    /// the event of each call.
    pub uses: Vec<i64>,
}

impl Problem {
    fn new(kind: Kind, sections: Vec<usize>) -> Problem {
        Problem {
            kind,
            certain: true,
            sections,
            steps: Vec::new(),
            items: Vec::new(),
            chains: HashMap::new(),
            before: None,
            bytes: None,
            from: None,
            sites: Vec::new(),
            uses: Vec::new(),
        }
    }

    /// The first place in the walk where the code runs from the load
    /// image.
    pub fn first(&self) -> i64 {
        self.uses.iter().copied().min().unwrap_or(RESET)
    }
}

pub(super) struct Note {
    pub kind: &'static str,
    pub sections: Vec<usize>,
    pub text: String,
}

/// The first step that sets up a section: as an index into the steps,
/// with how many of its bytes, and where it copies them from.
#[derive(Debug, Clone, Copy)]
pub(super) struct Setup {
    pub step: usize,
    pub bytes: u64,
    pub from: Option<u64>,
}

/// What startup's steps come to.
pub(super) struct Judged {
    /// The first step that sets up each section.
    pub init: HashMap<usize, Setup>,
    /// The steps of code elfq can't follow that may set up each section
    /// that no step does, as indexes into the steps: the unread steps.
    pub unread: HashMap<usize, Vec<usize>>,
    /// Whether elfq followed all the code before `main`.
    pub complete: bool,
    /// The event of the first certain problem, after which nothing can be
    /// counted on.
    pub fatal: Option<i64>,
    pub problems: Vec<Problem>,
    pub notes: Vec<Note>,
    /// For each load image that holds code, by section, the functions
    /// that must stay out of it, as indexes into the items (see
    /// [`EarlyRuns::keep_out`]).
    pub keep_out: Vec<(usize, Vec<usize>)>,
}

impl Judged {
    /// The state of `section` when `main` starts, with the number of the
    /// step that sets it up and how many of its bytes.
    pub fn status(
        &self,
        steps: &[Step],
        ram: &RamSections,
        section: usize,
    ) -> (Status, Option<usize>, Option<u64>) {
        if let Some(setup) = self.init.get(&section) {
            let step = &steps[setup.step];
            if self.fatal.is_some_and(|fatal| step.event as i64 >= fatal) {
                return (Status::NotReached, Some(step.number), Some(0));
            }
            let size = ram.section(section).size;
            let status = if ram.is_load(section) {
                if setup
                    .from
                    .is_some_and(|from| from != ram.load_address(section))
                {
                    Status::CopiedFromElsewhere
                } else if setup.bytes >= size {
                    Status::Copied
                } else {
                    Status::PartlyCopied
                }
            } else if setup.bytes >= size {
                Status::Zeroed
            } else {
                Status::PartlyZeroed
            };
            return (status, Some(step.number), Some(setup.bytes));
        }
        if let Some(unread) = self.unread.get(&section) {
            if (unread.iter()).any(|&s| {
                self.fatal
                    .is_none_or(|fatal| (steps[s].event as i64) < fatal)
            }) {
                return (Status::Unknown, None, None);
            }
            // Only code after the first certain problem may set it up.
            return (Status::NotReached, None, Some(0));
        }
        if ram.is_load(section) {
            (Status::NotCopied, None, Some(0))
        } else {
            (Status::NotZeroed, None, Some(0))
        }
    }
}

/// Works out what `steps` come to, from a walk that ended at `end`.
#[allow(clippy::too_many_arguments)]
pub(super) fn judge(
    firmware: &Firmware,
    ram: &RamSections,
    events: &Events,
    reset: &Reset,
    main: Option<Main>,
    steps: &[Step],
    end: End,
    all: bool,
) -> Judged {
    let mut init: HashMap<usize, Setup> = HashMap::new();
    for (index, step) in steps.iter().enumerate() {
        for cover in &step.covers {
            init.entry(cover.section).or_insert(Setup {
                step: index,
                bytes: cover.bytes,
                from: cover.from,
            });
        }
    }
    let mut unread: HashMap<usize, Vec<usize>> = HashMap::new();
    for (index, step) in steps.iter().enumerate() {
        if step.does == Does::Unread {
            for &section in &step.may {
                if !init.contains_key(&section) {
                    unread.entry(section).or_default().push(index);
                }
            }
        }
    }
    // A walk that ends where it can't read on, or stops short, didn't
    // follow what comes after, though that may set up nothing.
    let stopped = end.stops_short();
    let mut judged = Judged {
        init,
        unread,
        complete: !stopped && !steps.iter().any(|step| step.does == Does::Unread),
        fatal: None,
        problems: Vec::new(),
        notes: Vec::new(),
        keep_out: Vec::new(),
    };
    let mut early = EarlyRuns {
        firmware,
        main: main.and_then(Main::function),
        call_trees: HashMap::new(),
        callees: HashMap::new(),
        problems: Vec::new(),
    };
    early.find(ram, events, reset, steps, &judged);
    judged.keep_out = early.keep_out(ram, events, reset, steps, &judged);
    let mut problems = early.problems;
    for p in &mut problems {
        let section = p.sections[0];
        let plain: Vec<u64> = (p.items.iter().copied())
            .filter(|&a| firmware.veneer_target(a).is_none())
            .collect();
        if !plain.is_empty() {
            p.items = plain;
        }
        let first = p.first();
        p.certain = !steps.iter().any(|step| {
            step.does == Does::Unread && (step.event as i64) <= first && step.may.contains(&section)
        });
    }
    judged.fatal = (problems.iter())
        .filter(|p| p.certain)
        .map(Problem::first)
        .min();
    // Load images not copied, copied short or from elsewhere; .bss and
    // .sbss not zeroed in full.
    let ran_from: Vec<usize> = problems.iter().map(|p| p.sections[0]).collect();
    for &section in &ram.sections {
        let (status, _, _) = judged.status(steps, ram, section);
        let setup = judged.init.get(&section).copied();
        match (status, setup) {
            (Status::NotCopied, _) if !ran_from.contains(&section) => {
                problems.push(Problem::new(Kind::NotCopied, vec![section]));
            }
            (Status::NotZeroed, _) if ram.zeroed_by_rule(section) => {
                problems.push(Problem::new(Kind::NotZeroed, vec![section]));
            }
            (
                Status::PartlyCopied
                | Status::PartlyZeroed
                | Status::CopiedFromElsewhere
                | Status::NotReached,
                Some(setup),
            ) => {
                let size = ram.section(section).size;
                let mut p = if ram.is_load(section)
                    && setup
                        .from
                        .is_some_and(|from| from != ram.load_address(section))
                {
                    let mut p = Problem::new(Kind::CopiedFromElsewhere, vec![section]);
                    p.from = setup.from;
                    p
                } else if setup.bytes < size && ram.required(section) {
                    let kind = if ram.is_load(section) {
                        Kind::PartlyCopied
                    } else {
                        Kind::PartlyZeroed
                    };
                    let mut p = Problem::new(kind, vec![section]);
                    p.bytes = Some(setup.bytes);
                    p
                } else {
                    continue;
                };
                p.steps = vec![steps[setup.step].number];
                problems.push(p);
            }
            _ => {}
        }
    }
    // Code elfq can't follow that may set up a load image, .bss or .sbss.
    let unknown: Vec<usize> = (ram.sections.iter().copied())
        .filter(|&section| judged.status(steps, ram, section).0 == Status::Unknown)
        .collect();
    if unknown.iter().any(|&section| ram.required(section)) {
        let mut numbers: Vec<usize> = (unknown.iter())
            .filter(|&&section| ram.required(section))
            .flat_map(|section| &judged.unread[section])
            .filter(|&&s| {
                judged
                    .fatal
                    .is_none_or(|fatal| (steps[s].event as i64) < fatal)
            })
            .map(|&s| steps[s].number)
            .collect();
        numbers.sort_unstable();
        numbers.dedup();
        // The problem names only what it is about; the rest gets a note.
        let sections = (unknown.iter().copied())
            .filter(|&section| ram.required(section))
            .collect();
        let mut p = Problem::new(Kind::CantFollow, sections);
        p.certain = false;
        p.steps = numbers;
        problems.push(p);
    }
    judged.problems = problems;
    judged.notes = notes(firmware, ram, events, reset, steps, &judged, all);
    judged
}

/// Finds the code that runs from load images before startup code copies
/// them.
struct EarlyRuns<'a> {
    firmware: &'a Firmware<'a>,
    /// The function that is `main`, which the call trees leave out.
    main: Option<u64>,
    /// The call tree of each function (see [`EarlyRuns::call_tree`]).
    call_trees: HashMap<u64, CallTree>,
    /// The functions that each function calls or jumps to.
    callees: HashMap<u64, Vec<u64>>,
    problems: Vec<Problem>,
}

/// The functions that run when a function runs, each with the function
/// that calls it on a shortest chain from the first (see
/// [`EarlyRuns::call_tree`]).
struct CallTree {
    /// Each function, in the order a breadth-first search finds them, with
    /// the index of the function that calls it on the shortest chain from
    /// the first, whose own index it has.
    found: Vec<(u64, usize)>,
    /// The indexes into `found`, by address.
    by_address: Vec<usize>,
}

impl CallTree {
    /// The functions from `start` to `end`, in the order they were found,
    /// each with the chain of functions from the first to it.
    fn within(&self, start: u64, end: u64) -> Vec<(u64, Vec<u64>)> {
        let first = (self.by_address).partition_point(|&i| self.found[i].0 < start);
        let mut indexes: Vec<usize> = (self.by_address[first..].iter().copied())
            .take_while(|&i| self.found[i].0 < end)
            .collect();
        indexes.sort_unstable();
        (indexes.into_iter())
            .map(|i| (self.found[i].0, self.chain(i)))
            .collect()
    }

    /// The chain of functions from the first to the one at index `i`.
    fn chain(&self, mut i: usize) -> Vec<u64> {
        let mut chain = vec![self.found[i].0];
        while self.found[i].1 != i {
            i = self.found[i].1;
            chain.push(self.found[i].0);
        }
        chain.reverse();
        chain
    }
}

impl EarlyRuns<'_> {
    fn find(
        &mut self,
        ram: &RamSections,
        events: &Events,
        reset: &Reset,
        steps: &[Step],
        judged: &Judged,
    ) {
        let boot = reset.address;
        for &section in &ram.loads {
            if ram.section(section).address <= boot && boot < ram.end(section) {
                let hits = [(boot, vec![boot])];
                self.add(judged, section, RESET, None, &hits, None, &[]);
            }
        }
        for &section in &ram.loads {
            let (start, stop) = (ram.section(section).address, ram.end(section));
            for (event, target, holds) in self.calls_before(events, steps, judged, section) {
                let Some(call) = events.call(event) else {
                    continue;
                };
                let mut hits = if holds {
                    (start..stop)
                        .contains(&target)
                        .then(|| (target, vec![target]))
                        .into_iter()
                        .collect()
                } else {
                    self.call_tree(target).within(start, stop)
                };
                if let Some(veneer) = events
                    .veneer(event)
                    .filter(|veneer| (start..stop).contains(veneer))
                {
                    match hits.iter_mut().find(|(a, _)| *a == veneer) {
                        Some(hit) => hit.1 = vec![veneer],
                        None => hits.push((veneer, vec![veneer])),
                    }
                }
                if !hits.is_empty() {
                    let step = step_at(steps, event);
                    let callers = enclosing(events, event);
                    self.add(
                        judged,
                        section,
                        event as i64,
                        step,
                        &hits,
                        Some(call.site),
                        &callers,
                    );
                }
            }
        }
    }

    /// The calls that run before the step that copies the load image
    /// `section`, or before `main` when no step does, the copy's own call
    /// among them: each as its event, the function it goes to, and whether
    /// the call holds the copy, so that only the function's own code runs
    /// before it. Calls to `main` don't count.
    fn calls_before(
        &self,
        events: &Events,
        steps: &[Step],
        judged: &Judged,
        section: usize,
    ) -> Vec<(usize, u64, bool)> {
        let end =
            (judged.init.get(&section)).map_or(events.list.len(), |setup| steps[setup.step].event);
        (0..events.list.len().min(end + 1))
            .filter_map(|event| {
                events.call(event)?;
                let target = events
                    .target(event)
                    .filter(|&target| Some(target) != self.main)?;
                Some((event, target, event < end && end < events.span(event)))
            })
            .collect()
    }

    /// For each load image that holds code, by address, the functions
    /// that startup runs before it copies it, which must stay out of it:
    /// those that would run from it too early if they were in it. The
    /// reset handler comes first, then each call's function and what it
    /// calls, breadth first, each once, but no veneer.
    fn keep_out(
        &mut self,
        ram: &RamSections,
        events: &Events,
        reset: &Reset,
        steps: &[Step],
        judged: &Judged,
    ) -> Vec<(usize, Vec<usize>)> {
        let firmware = self.firmware;
        let mut rows = Vec::new();
        for &section in &ram.sections {
            if !ram.is_load(section) || ram.section(section).kind != SectionKind::Text {
                continue;
            }
            let mut functions: Vec<usize> = Vec::new();
            let (mut addresses, mut indexes) = (HashSet::new(), HashSet::new());
            let mut add = |address: u64| {
                if !addresses.insert(address) || firmware.veneer_target(address).is_some() {
                    return;
                }
                let Some(index) = firmware.item(address) else {
                    return;
                };
                if firmware.binary.items[index].kind == ItemKind::Function && indexes.insert(index)
                {
                    functions.push(index);
                }
            };
            add(reset.address);
            // A function called again adds nothing new, and going through
            // what it calls each time would take as long as the calls
            // times all they call.
            let mut expanded = HashSet::new();
            for (_, target, holds) in self.calls_before(events, steps, judged, section) {
                if holds {
                    add(target);
                } else if expanded.insert(target) {
                    for &(address, _) in &self.call_tree(target).found {
                        add(address);
                    }
                }
            }
            rows.push((section, functions));
        }
        rows
    }

    /// Counts the functions of `hits`, each with its chain, as code that
    /// runs from `section` at `event`, in `step`, from the call at `site`,
    /// inside the calls to `callers`, which come before each chain.
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        judged: &Judged,
        section: usize,
        event: i64,
        step: Option<&Step>,
        hits: &[(u64, Vec<u64>)],
        site: Option<u64>,
        callers: &[u64],
    ) {
        let index = match self.problems.iter().position(|p| p.sections[0] == section) {
            Some(index) => index,
            None => {
                let mut p = Problem::new(Kind::RunsBeforeCopy, vec![section]);
                p.before = judged.init.get(&section).map(|setup| setup.step);
                self.problems.push(p);
                self.problems.len() - 1
            }
        };
        let mut sites = Vec::new();
        for (a, chain) in hits {
            if !self.problems[index].items.contains(a) {
                self.problems[index].items.push(*a);
                let full = callers.iter().chain(chain).copied().collect();
                self.problems[index].chains.insert(*a, full);
            }
            match site {
                Some(site) => sites.extend(self.use_sites(chain, site)),
                None => sites.push(*a),
            }
        }
        let p = &mut self.problems[index];
        p.uses.push(event);
        if let Some(step) = step
            && !p.steps.contains(&step.number)
        {
            p.steps.push(step.number);
        }
        for site in sites {
            if !p.sites.contains(&site) {
                p.sites.push(site);
            }
        }
    }

    /// The functions that run when the function at `address` runs: what it
    /// calls and jumps to, at any depth, through veneers, but never `main`.
    fn call_tree(&mut self, address: u64) -> &CallTree {
        if !self.call_trees.contains_key(&address) {
            let mut found: Vec<(u64, usize)> = vec![(address, 0)];
            let mut seen: HashSet<u64> = HashSet::from([address]);
            let mut next = 0;
            while next < found.len() {
                let from = found[next].0;
                let firmware = self.firmware;
                let callees = (self.callees.entry(from)).or_insert_with(|| {
                    (firmware.callees(from).into_iter())
                        .map(|(to, _)| to)
                        .collect()
                });
                for &to in callees.iter() {
                    if Some(to) != self.main && seen.insert(to) {
                        found.push((to, next));
                    }
                }
                next += 1;
            }
            let mut by_address: Vec<usize> = (0..found.len()).collect();
            by_address.sort_unstable_by_key(|&i| found[i].0);
            self.call_trees
                .insert(address, CallTree { found, by_address });
        }
        &self.call_trees[&address]
    }

    /// Where the last function of `chain` that isn't a veneer calls on
    /// toward the function at its end: `site` when the chain is that
    /// function alone.
    fn use_sites(&self, chain: &[u64], site: u64) -> Vec<u64> {
        if chain.len() < 2 {
            return vec![site];
        }
        let mut last = chain.len() - 2;
        while last > 0 && self.firmware.veneer_target(chain[last]).is_some() {
            last -= 1;
        }
        (self.firmware.callees(chain[last]).into_iter())
            .find(|(to, _)| *to == chain[last + 1])
            .map_or(Vec::new(), |(_, sites)| sites)
    }
}

/// The functions that the calls holding `event` go to, from the reset
/// handler's own call down.
fn enclosing(events: &Events, event: usize) -> Vec<u64> {
    let mut callers = Vec::new();
    let mut level = events.level(event);
    for earlier in (0..event).rev() {
        if level == 0 {
            break;
        }
        if events.level(earlier) == level - 1
            && events.call(earlier).is_some()
            && event < events.span(earlier)
            && let Some(target) = events.target(earlier)
        {
            callers.push(target);
            level -= 1;
        }
    }
    callers.reverse();
    callers
}

/// The step of the reset handler's own code whose run holds `event`.
fn step_at(steps: &[Step], event: usize) -> Option<&Step> {
    (steps.iter())
        .rev()
        .find(|step| step.begins <= event && step.level == 0)
}

/// The notes under the steps: where the walk starts, branches it took,
/// steps that may never run, and sections without contents that may be
/// meant to stay as they are.
fn notes(
    firmware: &Firmware,
    ram: &RamSections,
    events: &Events,
    reset: &Reset,
    steps: &[Step],
    judged: &Judged,
    all: bool,
) -> Vec<Note> {
    let mut notes = Vec::new();
    let note = |kind, sections, text| Note {
        kind,
        sections,
        text,
    };
    let boot = firmware.name(reset.address);
    let entry = firmware.binary.entry;
    match (reset.table, reset.outside) {
        (None, _) => notes.push(note(
            "no vector table",
            Vec::new(),
            format!("elfq finds no vector table, so it reads from the entry point, {boot}."),
        )),
        (Some(table), Some(outside)) => notes.push(note(
            "reset in no function",
            Vec::new(),
            format!(
                "The vector table at {table:#x} gives {outside:#x} for reset, which is in no \
                 function, so elfq reads from the entry point, {boot}."
            ),
        )),
        (Some(table), None) if entry != reset.address => notes.push(note(
            "entry differs",
            Vec::new(),
            format!(
                "The vector table at {table:#x} gives {boot} for reset, not the entry point {}.",
                firmware.name(entry)
            ),
        )),
        (Some(_), None) => {}
    }
    for event in events.list {
        if let Event::Backtrack { site, because, .. } = event {
            notes.push(note(
                "branch",
                Vec::new(),
                format!(
                    "At {site:#x} elfq takes the branch, since the other way {}.",
                    text::other_way(*because)
                ),
            ));
        }
    }
    let status = |section| judged.status(steps, ram, section).0;
    let named: Vec<usize> = (judged.problems.iter())
        .filter(|p| p.kind != Kind::CantFollow)
        .flat_map(|p| p.sections.iter().copied())
        .collect();
    if let Some(fatal) = judged.fatal {
        let later: Vec<usize> = (steps.iter())
            .filter(|step| {
                let event = step.event as i64;
                event > fatal || (event == fatal && matches!(step.does, Does::Copy | Does::Zero))
            })
            .map(|step| step.number)
            .collect();
        if !later.is_empty() {
            let lost: Vec<usize> = (ram.sections.iter().copied())
                .filter(|&section| status(section) == Status::NotReached)
                .collect();
            let rest: Vec<&str> = (lost.iter())
                .filter(|section| !named.contains(section))
                .map(|&section| ram.name(section))
                .collect();
            let so = if rest.is_empty() {
                String::new()
            } else {
                format!(", so {} may not be set up", names_text(&rest, all))
            };
            notes.push(note(
                "not reached",
                lost,
                format!("{} may never run{so}.", text::steps_text(&later, true)),
            ));
        }
    }
    let not_zeroed: Vec<usize> = (ram.zeros.iter().copied())
        .filter(|&section| status(section) == Status::NotZeroed && !named.contains(&section))
        .collect();
    if !not_zeroed.is_empty() {
        let parts: Vec<String> = (not_zeroed.iter())
            .map(|&section| {
                let held = data_in(firmware, ram, section);
                if held.is_empty() {
                    ram.name(section).to_string()
                } else {
                    format!("{} ({})", ram.name(section), names_text(&held, all))
                }
            })
            .collect();
        let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
        notes.push(note(
            "not zeroed",
            not_zeroed,
            format!(
                "Not zeroed, which may be intended: {}.",
                names_text(&parts, true)
            ),
        ));
    }
    for &section in &ram.zeros {
        let (state, step, bytes) = judged.status(steps, ram, section);
        if state == Status::PartlyZeroed && !named.contains(&section) {
            notes.push(note(
                "partly zeroed",
                vec![section],
                format!(
                    "Step {} zeroes {} of {}'s {} bytes, which may be intended.",
                    step.unwrap_or(0),
                    bytes.unwrap_or(0),
                    ram.name(section),
                    ram.section(section).size
                ),
            ));
        }
    }
    // Sections that the can't-follow problem doesn't name, which code elfq
    // can't follow may set up too.
    let in_problem: Vec<usize> = (judged.problems.iter())
        .filter(|p| p.kind == Kind::CantFollow)
        .flat_map(|p| p.sections.iter().copied())
        .collect();
    let unknown: Vec<usize> = (ram.sections.iter().copied())
        .filter(|&section| status(section) == Status::Unknown && !in_problem.contains(&section))
        .collect();
    if !unknown.is_empty() {
        let mut numbers: Vec<usize> = (unknown.iter())
            .flat_map(|section| &judged.unread[section])
            .map(|&s| steps[s].number)
            .collect();
        numbers.sort_unstable();
        numbers.dedup();
        let text = format!(
            "elfq can't follow {}, which may {}.",
            text::unread_text(steps, &numbers),
            text::may_text(ram, &unknown)
        );
        notes.push(note("can't tell", unknown, text));
    }
    notes
}

/// The names of the data objects in `section`, by address, but for those
/// named after where they are.
fn data_in<'a>(firmware: &'a Firmware, ram: &RamSections, section: usize) -> Vec<&'a str> {
    let start = ram.section(section).address;
    (firmware.binary.items.iter())
        .filter(|item| {
            item.kind == ItemKind::Data
                && start <= item.address
                && item.address < ram.end(section)
                && !item.name.contains('@')
        })
        .map(|item| item.name.as_str())
        .collect()
}
