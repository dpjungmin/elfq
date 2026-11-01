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

use super::events::Events;
use super::ram_sections::RamSections;
use super::start::Main;
use super::steps::{Does, Step, number};
use super::text;
use super::{Facts, Firmware};
use crate::arch::startup::Event;
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
/// code, which comes before every event, or the call at an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum RunsAt {
    ResetHandler,
    Event(usize),
}

impl RunsAt {
    /// The event of the call, when it is one.
    pub fn event(self) -> Option<usize> {
        match self {
            RunsAt::ResetHandler => None,
            RunsAt::Event(event) => Some(event),
        }
    }
}

pub(super) struct Problem {
    pub kind: Kind,
    pub sections: Vec<usize>,
    /// The steps it is about, as indexes into the steps.
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
    /// Where in the walk the code runs from the load image.
    pub uses: Vec<RunsAt>,
    /// For code that runs before its copy, the unread steps up to where it
    /// first runs that may copy the load image first, as indexes into the
    /// steps.
    pub unless: Vec<usize>,
}

impl Problem {
    fn new(kind: Kind, sections: Vec<usize>) -> Problem {
        Problem {
            kind,
            sections,
            steps: Vec::new(),
            items: Vec::new(),
            chains: HashMap::new(),
            before: None,
            bytes: None,
            from: None,
            sites: Vec::new(),
            uses: Vec::new(),
            unless: Vec::new(),
        }
    }

    /// Whether the problem is certain: not for code elfq can't follow,
    /// which may be a problem or not, nor for code that runs from a load
    /// image after unread steps that may copy it first.
    pub fn certain(&self) -> bool {
        self.kind != Kind::CantFollow && self.unless.is_empty()
    }

    /// The first place in the walk where the code runs from the load
    /// image.
    pub fn first(&self) -> RunsAt {
        self.uses
            .iter()
            .copied()
            .min()
            .unwrap_or(RunsAt::ResetHandler)
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

/// The state of a section when `main` starts (see [`Judged::status`]).
#[derive(Debug, Clone, Copy)]
pub(super) struct SectionStatus {
    pub status: Status,
    /// The step that sets the section up, as an index into the steps.
    pub step: Option<usize>,
    /// How many of its bytes are set up: none when elfq can't tell.
    pub bytes: Option<u64>,
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
    /// Where the first certain problem is, after which nothing can be
    /// counted on.
    pub fatal: Option<RunsAt>,
    /// The state of each section that startup code sets up when `main`
    /// starts (see [`Judged::status`]).
    statuses: HashMap<usize, SectionStatus>,
    pub problems: Vec<Problem>,
    pub notes: Vec<Note>,
    /// For each load image that holds code, the functions that must stay
    /// out of it (see [`EarlyRuns::keep_out`]).
    pub keep_out: Vec<KeepOut>,
}

/// The functions that must stay out of a load image that holds code.
pub(super) struct KeepOut {
    pub section: usize,
    /// The functions that must stay out of it, as item indexes.
    pub items: Vec<usize>,
}

impl Judged {
    /// Whether `step` comes before the first certain problem, so that it
    /// can be counted on to run.
    pub fn reached(&self, step: &Step) -> bool {
        self.fatal
            .is_none_or(|fatal| RunsAt::Event(step.event) < fatal)
    }

    /// Whether `step` may never run: it comes after the first certain
    /// problem, or copies or zeroes at its event, after the code that runs
    /// from a load image there.
    pub fn may_never_run(&self, step: &Step) -> bool {
        self.fatal.is_some_and(|fatal| {
            let at = RunsAt::Event(step.event);
            at > fatal || (at == fatal && matches!(step.does, Does::Copy | Does::Zero))
        })
    }

    /// The state of `section`, one of the RAM sections, when `main` starts,
    /// with the step that sets it up and how many of its bytes.
    pub fn status(&self, section: usize) -> SectionStatus {
        self.statuses[&section]
    }

    /// Works out the state of `section` when `main` starts, which
    /// [`Judged::status`] gives: once the first certain problem is known.
    fn work_out_status(&self, steps: &[Step], ram: &RamSections, section: usize) -> SectionStatus {
        let unset = |status| SectionStatus {
            status,
            step: None,
            bytes: Some(0),
        };
        if let Some(setup) = self.init.get(&section) {
            if !self.reached(&steps[setup.step]) {
                return SectionStatus {
                    status: Status::NotReached,
                    step: Some(setup.step),
                    bytes: Some(0),
                };
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
            return SectionStatus {
                status,
                step: Some(setup.step),
                bytes: Some(setup.bytes),
            };
        }
        if let Some(unread) = self.unread.get(&section) {
            if (unread.iter()).any(|&index| self.reached(&steps[index])) {
                return SectionStatus {
                    status: Status::Unknown,
                    step: None,
                    bytes: None,
                };
            }
            // Only code after the first certain problem may set it up.
            return unset(Status::NotReached);
        }
        if ram.is_load(section) {
            unset(Status::NotCopied)
        } else {
            unset(Status::NotZeroed)
        }
    }
}

/// Works out what `steps` come to. With `all`, as with `--all`, the notes
/// list every name in full.
pub(super) fn judge(facts: &Facts, steps: &[Step], all: bool) -> Judged {
    let Facts {
        firmware,
        ram,
        main,
        end,
        ..
    } = *facts;
    let mut init: HashMap<usize, Setup> = HashMap::new();
    for (index, step) in steps.iter().enumerate() {
        for cover in step.covers() {
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
        statuses: HashMap::new(),
        problems: Vec::new(),
        notes: Vec::new(),
        keep_out: Vec::new(),
    };
    let mut early = EarlyRuns {
        facts,
        main: main.and_then(Main::function),
        call_trees: HashMap::new(),
        callees: HashMap::new(),
        problems: Vec::new(),
    };
    early.find(steps, &judged);
    judged.keep_out = early.keep_out(steps, &judged);
    let mut problems = early.problems;
    for p in &mut problems {
        let section = p.sections[0];
        let plain: Vec<u64> = (p.items.iter().copied())
            .filter(|&a| !firmware.is_veneer(a))
            .collect();
        if !plain.is_empty() {
            p.items = plain;
        }
        let first = p.first();
        p.unless = (steps.iter().enumerate())
            .filter(|(_, step)| {
                step.does == Does::Unread
                    && RunsAt::Event(step.event) <= first
                    && step.may.contains(&section)
            })
            .map(|(index, _)| index)
            .collect();
    }
    judged.fatal = (problems.iter())
        .filter(|p| p.certain())
        .map(Problem::first)
        .min();
    judged.statuses = (ram.sections.iter())
        .map(|&section| (section, judged.work_out_status(steps, ram, section)))
        .collect();
    // Load images not copied, copied short or from elsewhere; .bss and
    // .sbss not zeroed in full.
    let ran_from: Vec<usize> = problems.iter().map(|p| p.sections[0]).collect();
    for &section in &ram.sections {
        let status = judged.status(section).status;
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
                p.steps = vec![setup.step];
                problems.push(p);
            }
            _ => {}
        }
    }
    // Code elfq can't follow that may set up a load image, .bss or .sbss.
    let unknown: Vec<usize> = (ram.sections.iter().copied())
        .filter(|&section| judged.status(section).status == Status::Unknown)
        .collect();
    if unknown.iter().any(|&section| ram.required(section)) {
        let mut indexes: Vec<usize> = (unknown.iter())
            .filter(|&&section| ram.required(section))
            .flat_map(|section| judged.unread[section].iter().copied())
            .filter(|&index| judged.reached(&steps[index]))
            .collect();
        indexes.sort_unstable();
        indexes.dedup();
        // The problem names only what it is about; the rest gets a note.
        let sections = (unknown.iter().copied())
            .filter(|&section| ram.required(section))
            .collect();
        let mut p = Problem::new(Kind::CantFollow, sections);
        p.steps = indexes;
        problems.push(p);
    }
    judged.problems = problems;
    judged.notes = notes(facts, steps, &judged, all);
    judged
}

/// Finds the code that runs from load images before startup code copies
/// them.
struct EarlyRuns<'a> {
    facts: &'a Facts<'a>,
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
    /// Each function, in the order a breadth-first search finds them.
    found: Vec<Called>,
    /// The indexes into `found`, by address.
    by_address: Vec<usize>,
}

/// A function in a call tree.
#[derive(Debug, Clone, Copy)]
struct Called {
    function: u64,
    /// The index in [`CallTree::found`] of the function that calls it on
    /// the shortest chain from the first, or for the first, its own.
    caller: usize,
}

impl CallTree {
    /// The functions from `start` to `end`, in the order they were found,
    /// each with the chain of functions from the first to it.
    fn within(&self, start: u64, end: u64) -> Vec<(u64, Vec<u64>)> {
        let first = (self.by_address).partition_point(|&i| self.found[i].function < start);
        let mut indexes: Vec<usize> = (self.by_address[first..].iter().copied())
            .take_while(|&i| self.found[i].function < end)
            .collect();
        indexes.sort_unstable();
        (indexes.into_iter())
            .map(|i| (self.found[i].function, self.chain(i)))
            .collect()
    }

    /// The chain of functions from the first to the one at index `i`.
    fn chain(&self, mut i: usize) -> Vec<u64> {
        let mut chain = vec![self.found[i].function];
        while self.found[i].caller != i {
            i = self.found[i].caller;
            chain.push(self.found[i].function);
        }
        chain.reverse();
        chain
    }
}

/// A call that runs before the step that copies a load image (see
/// [`EarlyRuns::calls_before`]).
#[derive(Debug, Clone, Copy)]
struct CallBefore {
    event: usize,
    /// The function that the call goes to.
    target: u64,
    /// Whether the call holds the copy, so that only the function's own
    /// code runs before it.
    holds_copy: bool,
}

impl EarlyRuns<'_> {
    /// Finds the code that runs from each load image before `steps` copy
    /// it: the reset handler's own code, and what each call runs.
    fn find(&mut self, steps: &[Step], judged: &Judged) {
        let Facts {
            ram, events, reset, ..
        } = *self.facts;
        let boot = reset.address;
        for &section in &ram.loads {
            if ram.section(section).address <= boot && boot < ram.end(section) {
                let hits = [(boot, vec![boot])];
                self.add(
                    judged,
                    section,
                    RunsAt::ResetHandler,
                    None,
                    &hits,
                    None,
                    &[],
                );
            }
        }
        for &section in &ram.loads {
            let (start, stop) = (ram.section(section).address, ram.end(section));
            for CallBefore {
                event,
                target,
                holds_copy,
            } in self.calls_before(steps, judged, section)
            {
                let Some(call) = events.call(event) else {
                    continue;
                };
                let mut hits = if holds_copy {
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
                        RunsAt::Event(event),
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
    /// among them. Calls to `main` don't count.
    fn calls_before(&self, steps: &[Step], judged: &Judged, section: usize) -> Vec<CallBefore> {
        let events = self.facts.events;
        let end =
            (judged.init.get(&section)).map_or(events.list.len(), |setup| steps[setup.step].event);
        (0..events.list.len().min(end + 1))
            .filter_map(|event| {
                events.call(event)?;
                let target = events
                    .target(event)
                    .filter(|&target| Some(target) != self.main)?;
                Some(CallBefore {
                    event,
                    target,
                    holds_copy: event < end && end < events.span(event),
                })
            })
            .collect()
    }

    /// For each load image that holds code, by address, the functions
    /// that startup runs before it copies it, which must stay out of it:
    /// those that would run from it too early if they were in it. The
    /// reset handler comes first, then each call's function and what it
    /// calls, breadth first, each once, but no veneer.
    fn keep_out(&mut self, steps: &[Step], judged: &Judged) -> Vec<KeepOut> {
        let Facts {
            firmware,
            ram,
            reset,
            ..
        } = *self.facts;
        let mut rows = Vec::new();
        for &section in &ram.sections {
            if !ram.is_load(section) || ram.section(section).kind != SectionKind::Text {
                continue;
            }
            let mut items: Vec<usize> = Vec::new();
            let (mut addresses, mut indexes) = (HashSet::new(), HashSet::new());
            let mut add = |address: u64| {
                if !addresses.insert(address) || firmware.is_veneer(address) {
                    return;
                }
                let Some(index) = firmware.item(address) else {
                    return;
                };
                if firmware.binary.items[index].kind == ItemKind::Function && indexes.insert(index)
                {
                    items.push(index);
                }
            };
            add(reset.address);
            // A function called again adds nothing new, and going through
            // what it calls each time would take as long as the calls
            // times all they call.
            let mut expanded = HashSet::new();
            for call in self.calls_before(steps, judged, section) {
                if call.holds_copy {
                    add(call.target);
                } else if expanded.insert(call.target) {
                    for called in &self.call_tree(call.target).found {
                        add(called.function);
                    }
                }
            }
            rows.push(KeepOut { section, items });
        }
        rows
    }

    /// Counts the functions of `hits`, each with its chain, as code that
    /// runs from `section` at `at`, in the step at index `step`, from the
    /// call at `site`, inside the calls to `callers`, which come before
    /// each chain.
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        judged: &Judged,
        section: usize,
        at: RunsAt,
        step: Option<usize>,
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
        p.uses.push(at);
        if let Some(step) = step
            && !p.steps.contains(&step)
        {
            p.steps.push(step);
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
            let mut found = vec![Called {
                function: address,
                caller: 0,
            }];
            let mut seen: HashSet<u64> = HashSet::from([address]);
            let mut next = 0;
            while next < found.len() {
                let from = found[next].function;
                let firmware = self.facts.firmware;
                let callees = (self.callees.entry(from)).or_insert_with(|| firmware.callees(from));
                for &to in callees.iter() {
                    if Some(to) != self.main && seen.insert(to) {
                        found.push(Called {
                            function: to,
                            caller: next,
                        });
                    }
                }
                next += 1;
            }
            let mut by_address: Vec<usize> = (0..found.len()).collect();
            by_address.sort_unstable_by_key(|&i| found[i].function);
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
        let firmware = self.facts.firmware;
        while last > 0 && firmware.is_veneer(chain[last]) {
            last -= 1;
        }
        firmware.call_sites(chain[last], chain[last + 1])
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

/// The step of the reset handler's own code whose run holds `event`, as
/// an index into the steps.
fn step_at(steps: &[Step], event: usize) -> Option<usize> {
    (steps.iter()).rposition(|step| step.begins <= event && step.level == 0)
}

/// The notes under the steps: where the walk starts, branches it took,
/// steps that may never run, and sections without contents that may be
/// meant to stay as they are.
fn notes(facts: &Facts, steps: &[Step], judged: &Judged, all: bool) -> Vec<Note> {
    let Facts {
        firmware,
        ram,
        reset,
        events,
        ..
    } = *facts;
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
    let status = |section| judged.status(section).status;
    let named: Vec<usize> = (judged.problems.iter())
        .filter(|p| p.kind != Kind::CantFollow)
        .flat_map(|p| p.sections.iter().copied())
        .collect();
    let later: Vec<usize> = (steps.iter().enumerate())
        .filter(|(_, step)| judged.may_never_run(step))
        .map(|(index, _)| index)
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
        let zeroed = judged.status(section);
        if zeroed.status == Status::PartlyZeroed && !named.contains(&section) {
            notes.push(note(
                "partly zeroed",
                vec![section],
                format!(
                    "Step {} zeroes {} of {}'s {} bytes, which may be intended.",
                    zeroed.step.map_or(0, number),
                    zeroed.bytes.unwrap_or(0),
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
        let mut indexes: Vec<usize> = (unknown.iter())
            .flat_map(|section| judged.unread[section].iter().copied())
            .collect();
        indexes.sort_unstable();
        indexes.dedup();
        let text = format!(
            "elfq can't follow {}, which may {}.",
            text::unread_text(steps, &indexes),
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
