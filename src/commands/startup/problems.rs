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

use super::Firmware;
use super::start::{Main, Reset};
use super::steps::{Does, Events, Memory, Step};
use super::text;
use crate::arch::startup::{End, Event};
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
    /// that no step does, as indexes into the steps.
    pub unknown: HashMap<usize, Vec<usize>>,
    /// Whether elfq followed all the code before `main`.
    pub complete: bool,
    /// The event of the first certain problem, after which nothing can be
    /// counted on.
    pub fatal: Option<i64>,
    pub problems: Vec<Problem>,
    pub notes: Vec<Note>,
}

impl Judged {
    /// The state of section `x` when `main` starts, with the number of the
    /// step that sets it up and how many of its bytes.
    pub fn status(
        &self,
        steps: &[Step],
        memory: &Memory,
        x: usize,
    ) -> (Status, Option<usize>, Option<u64>) {
        if let Some(setup) = self.init.get(&x) {
            let step = &steps[setup.step];
            if self.fatal.is_some_and(|fatal| step.event as i64 >= fatal) {
                return (Status::NotReached, Some(step.number), Some(0));
            }
            let size = memory.section(x).size;
            let status = if memory.is_load(x) {
                if setup
                    .from
                    .is_some_and(|from| from != memory.load_address(x))
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
        if let Some(unread) = self.unknown.get(&x) {
            if (unread.iter()).any(|&s| {
                self.fatal
                    .is_none_or(|fatal| (steps[s].event as i64) < fatal)
            }) {
                return (Status::Unknown, None, None);
            }
            // Only code after the first certain problem may set it up.
            return (Status::NotReached, None, Some(0));
        }
        if memory.is_load(x) {
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
    memory: &Memory,
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
    let mut unknown: HashMap<usize, Vec<usize>> = HashMap::new();
    for (index, step) in steps.iter().enumerate() {
        if step.does == Does::Unread {
            for &x in &step.may {
                if !init.contains_key(&x) {
                    unknown.entry(x).or_default().push(index);
                }
            }
        }
    }
    // A walk that ends where it can't read on, or stops short, didn't
    // follow what comes after, though that may set up nothing.
    let stopped = matches!(
        end,
        End::Undecoded(_) | End::Branches { .. } | End::LeavesFile { .. } | End::Budget(_)
    );
    let mut judged = Judged {
        init,
        unknown,
        complete: !stopped && !steps.iter().any(|step| step.does == Does::Unread),
        fatal: None,
        problems: Vec::new(),
        notes: Vec::new(),
    };
    let mut runs = Runs {
        firmware,
        main: main.and_then(Main::function),
        closures: HashMap::new(),
        callees: HashMap::new(),
        problems: Vec::new(),
    };
    runs.find(memory, events, reset, steps, &judged);
    let mut problems = runs.problems;
    for p in &mut problems {
        let x = p.sections[0];
        let plain: Vec<u64> = (p.items.iter().copied())
            .filter(|&a| firmware.veneer_target(a).is_none())
            .collect();
        if !plain.is_empty() {
            p.items = plain;
        }
        let first = p.first();
        p.certain = !steps.iter().any(|step| {
            step.does == Does::Unread && (step.event as i64) <= first && step.may.contains(&x)
        });
    }
    judged.fatal = (problems.iter())
        .filter(|p| p.certain)
        .map(Problem::first)
        .min();
    // Load images not copied, copied short or from elsewhere; .bss and
    // .sbss not zeroed in full.
    let ran_from: Vec<usize> = problems.iter().map(|p| p.sections[0]).collect();
    for &x in &memory.ram {
        let (status, _, _) = judged.status(steps, memory, x);
        let setup = judged.init.get(&x).copied();
        match (status, setup) {
            (Status::NotCopied, _) if !ran_from.contains(&x) => {
                problems.push(Problem::new(Kind::NotCopied, vec![x]));
            }
            (Status::NotZeroed, _) if memory.zeroed_by_rule(x) => {
                problems.push(Problem::new(Kind::NotZeroed, vec![x]));
            }
            (
                Status::PartlyCopied
                | Status::PartlyZeroed
                | Status::CopiedFromElsewhere
                | Status::NotReached,
                Some(setup),
            ) => {
                let size = memory.section(x).size;
                let mut p = if memory.is_load(x)
                    && setup
                        .from
                        .is_some_and(|from| from != memory.load_address(x))
                {
                    let mut p = Problem::new(Kind::CopiedFromElsewhere, vec![x]);
                    p.from = setup.from;
                    p
                } else if setup.bytes < size && memory.required(x) {
                    let kind = if memory.is_load(x) {
                        Kind::PartlyCopied
                    } else {
                        Kind::PartlyZeroed
                    };
                    let mut p = Problem::new(kind, vec![x]);
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
    let unknown: Vec<usize> = (memory.ram.iter().copied())
        .filter(|&x| judged.status(steps, memory, x).0 == Status::Unknown)
        .collect();
    if unknown.iter().any(|&x| memory.required(x)) {
        let mut numbers: Vec<usize> = (unknown.iter())
            .filter(|&&x| memory.required(x))
            .flat_map(|x| &judged.unknown[x])
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
            .filter(|&x| memory.required(x))
            .collect();
        let mut p = Problem::new(Kind::CantFollow, sections);
        p.certain = false;
        p.steps = numbers;
        problems.push(p);
    }
    judged.problems = problems;
    judged.notes = notes(firmware, memory, events, reset, steps, &judged, all);
    judged
}

/// Finds the code that runs from load images before startup code copies
/// them.
struct Runs<'a> {
    firmware: &'a Firmware<'a>,
    /// The function that is `main`, which the closures leave out.
    main: Option<u64>,
    /// The closure of each function (see [`Runs::closure`]).
    closures: HashMap<u64, Closure>,
    /// The functions that each function calls or jumps to.
    callees: HashMap<u64, Vec<u64>>,
    problems: Vec<Problem>,
}

/// The functions that run when a function runs (see [`Runs::closure`]).
struct Closure {
    /// Each function, in the order a breadth-first search finds them, with
    /// the index of the function that calls it on the shortest chain from
    /// the first, whose own index it has.
    found: Vec<(u64, usize)>,
    /// The indexes into `found`, by address.
    by_address: Vec<usize>,
}

impl Closure {
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

impl Runs<'_> {
    fn find(
        &mut self,
        memory: &Memory,
        events: &Events,
        reset: &Reset,
        steps: &[Step],
        judged: &Judged,
    ) {
        let boot = reset.address;
        for &x in &memory.loads {
            if memory.section(x).address <= boot && boot < memory.end(x) {
                let hits = [(boot, vec![boot])];
                self.add(judged, x, RESET, None, &hits, None, &[]);
            }
        }
        let main_event = events.list.len();
        for &x in &memory.loads {
            let (start, stop) = (memory.section(x).address, memory.end(x));
            let end = judged
                .init
                .get(&x)
                .map_or(main_event, |setup| steps[setup.step].event);
            for k in 0..events.list.len().min(end + 1) {
                let (Some(call), Some(target)) = (events.call(k), events.target(k)) else {
                    continue;
                };
                if Some(target) == self.main {
                    continue;
                }
                // A call that holds the copy runs only its own code before
                // it.
                let mut hits = if k < end && end < events.span(k) {
                    (start..stop)
                        .contains(&target)
                        .then(|| (target, vec![target]))
                        .into_iter()
                        .collect()
                } else {
                    self.closure(target).within(start, stop)
                };
                if let Some(veneer) = events
                    .veneer(k)
                    .filter(|veneer| (start..stop).contains(veneer))
                {
                    match hits.iter_mut().find(|(a, _)| *a == veneer) {
                        Some(hit) => hit.1 = vec![veneer],
                        None => hits.push((veneer, vec![veneer])),
                    }
                }
                if !hits.is_empty() {
                    let step = step_at(steps, k);
                    let callers = enclosing(events, k);
                    self.add(judged, x, k as i64, step, &hits, Some(call.site), &callers);
                }
            }
        }
    }

    /// Counts the functions of `hits`, each with its chain, as code that
    /// runs from `x` at event `k`, in `step`, from the call at `site`,
    /// inside the calls to `callers`, which come before each chain.
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        judged: &Judged,
        x: usize,
        k: i64,
        step: Option<&Step>,
        hits: &[(u64, Vec<u64>)],
        site: Option<u64>,
        callers: &[u64],
    ) {
        let index = match self.problems.iter().position(|p| p.sections[0] == x) {
            Some(index) => index,
            None => {
                let mut p = Problem::new(Kind::RunsBeforeCopy, vec![x]);
                p.before = judged.init.get(&x).map(|setup| setup.step);
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
        p.uses.push(k);
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
    fn closure(&mut self, address: u64) -> &Closure {
        if !self.closures.contains_key(&address) {
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
            self.closures.insert(address, Closure { found, by_address });
        }
        &self.closures[&address]
    }

    /// Where the last function of `chain` that isn't a veneer calls on
    /// toward the function at its end: `site` when the chain is that
    /// function alone.
    fn use_sites(&self, chain: &[u64], site: u64) -> Vec<u64> {
        if chain.len() < 2 {
            return vec![site];
        }
        let mut k = chain.len() - 2;
        while k > 0 && self.firmware.veneer_target(chain[k]).is_some() {
            k -= 1;
        }
        (self.firmware.callees(chain[k]).into_iter())
            .find(|(to, _)| *to == chain[k + 1])
            .map_or(Vec::new(), |(_, sites)| sites)
    }
}

/// The functions that the calls holding event `k` go to, from the reset
/// handler's own call down.
fn enclosing(events: &Events, k: usize) -> Vec<u64> {
    let mut callers = Vec::new();
    let mut level = events.level(k);
    for j in (0..k).rev() {
        if level == 0 {
            break;
        }
        if events.level(j) == level - 1
            && events.call(j).is_some()
            && k < events.span(j)
            && let Some(target) = events.target(j)
        {
            callers.push(target);
            level -= 1;
        }
    }
    callers.reverse();
    callers
}

/// The step of the reset handler's own code whose run holds event `k`.
fn step_at(steps: &[Step], k: usize) -> Option<&Step> {
    (steps.iter())
        .rev()
        .find(|step| step.event <= k && step.level == 0)
}

/// The notes under the steps: where the walk starts, branches it took,
/// steps that may never run, and sections without contents that may be
/// meant to stay as they are.
fn notes(
    firmware: &Firmware,
    memory: &Memory,
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
    let status = |x| judged.status(steps, memory, x).0;
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
            let lost: Vec<usize> = (memory.ram.iter().copied())
                .filter(|&x| status(x) == Status::NotReached)
                .collect();
            let rest: Vec<&str> = (lost.iter())
                .filter(|x| !named.contains(x))
                .map(|&x| memory.name(x))
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
    let not_zeroed: Vec<usize> = (memory.zeros.iter().copied())
        .filter(|&x| status(x) == Status::NotZeroed && !named.contains(&x))
        .collect();
    if !not_zeroed.is_empty() {
        let parts: Vec<String> = (not_zeroed.iter())
            .map(|&x| {
                let held = data_in(firmware, memory, x);
                if held.is_empty() {
                    memory.name(x).to_string()
                } else {
                    format!("{} ({})", memory.name(x), names_text(&held, all))
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
    for &x in &memory.zeros {
        let (state, step, bytes) = judged.status(steps, memory, x);
        if state == Status::PartlyZeroed && !named.contains(&x) {
            notes.push(note(
                "partly zeroed",
                vec![x],
                format!(
                    "Step {} zeroes {} of {}'s {} bytes, which may be intended.",
                    step.unwrap_or(0),
                    bytes.unwrap_or(0),
                    memory.name(x),
                    memory.section(x).size
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
    let unknown: Vec<usize> = (memory.ram.iter().copied())
        .filter(|&x| status(x) == Status::Unknown && !in_problem.contains(&x))
        .collect();
    if !unknown.is_empty() {
        let mut numbers: Vec<usize> = (unknown.iter())
            .flat_map(|x| &judged.unknown[x])
            .map(|&s| steps[s].number)
            .collect();
        numbers.sort_unstable();
        numbers.dedup();
        let text = format!(
            "elfq can't follow {}, which may {}.",
            text::unread_text(steps, &numbers),
            text::may_text(memory, &unknown)
        );
        notes.push(note("can't tell", unknown, text));
    }
    notes
}

/// The names of the data objects in section `x`, by address, but for
/// those named after where they are.
fn data_in<'a>(firmware: &'a Firmware, memory: &Memory, x: usize) -> Vec<&'a str> {
    let section = memory.section(x);
    (firmware.binary.items.iter())
        .filter(|item| {
            item.kind == crate::binary::ItemKind::Data
                && section.address <= item.address
                && item.address < memory.end(x)
                && !item.name.contains('@')
        })
        .map(|item| item.name.as_str())
        .collect()
}
