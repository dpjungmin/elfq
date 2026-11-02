//! What can go wrong in startup: the state of each section when `main`
//! starts, the problems, and the notes, as facts that `text.rs` words.
//!
//! A load image runs before its copy when code in it runs before the step
//! that copies it: the reset handler itself, or any function that a call
//! before the copy runs, at any depth, through calls and tail calls (not
//! through taken addresses, since a weak undefined function is at address
//! 0, which is in ITCM on many parts). The first such problem that no code
//! elfq can't follow comes before is certain, and nothing after it can be
//! counted on: the steps after it "may never run".

use std::collections::{HashMap, HashSet};

use super::Facts;
use super::events::Events;
use super::ram_sections::RamSections;
use super::start::Main;
use super::steps::{Does, Step};
use crate::arch::startup::{End, Event};
use crate::binary::ItemKind;

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

/// A problem with what startup code does, as facts that
/// `text::sentence` words.
pub(super) enum Problem {
    /// Code runs from a load image before startup code copies it.
    RunsBeforeCopy(EarlyRun),
    /// Nothing copies the load image `section`.
    NotCopied { section: usize },
    /// Nothing zeroes `section`, which is `.bss` or `.sbss`.
    NotZeroed { section: usize },
    /// The step at index `step` copies `bytes` of the load image
    /// `section`, short of its size.
    PartlyCopied {
        section: usize,
        step: usize,
        bytes: u64,
    },
    /// The step at index `step` zeroes `bytes` of `section`, which is
    /// `.bss` or `.sbss`, short of its size.
    PartlyZeroed {
        section: usize,
        step: usize,
        bytes: u64,
    },
    /// The step at index `step` copies the load image `section` from
    /// `from`, rather than from where the file stores it.
    CopiedFromElsewhere {
        section: usize,
        step: usize,
        from: u64,
    },
    /// Code elfq can't follow, at the unread steps at `steps`, as indexes
    /// into the steps, may set up `sections`, which no step does: load
    /// images, `.bss` or `.sbss`.
    CantFollow {
        sections: Vec<usize>,
        steps: Vec<usize>,
    },
}

impl Problem {
    /// The JSON's `kind`.
    pub fn field(&self) -> &'static str {
        match self {
            Problem::RunsBeforeCopy(_) => "runs before copy",
            Problem::NotCopied { .. } => "not copied",
            Problem::NotZeroed { .. } => "not zeroed",
            Problem::PartlyCopied { .. } => "partly copied",
            Problem::PartlyZeroed { .. } => "partly zeroed",
            Problem::CopiedFromElsewhere { .. } => "copied from elsewhere",
            Problem::CantFollow { .. } => "can't follow",
        }
    }

    /// Whether the problem is certain: not for code elfq can't follow,
    /// which may be a problem or not, nor for code that runs from a load
    /// image after unread steps that may copy it first.
    pub fn certain(&self) -> bool {
        match self {
            Problem::RunsBeforeCopy(run) => run.certain(),
            Problem::CantFollow { .. } => false,
            Problem::NotCopied { .. }
            | Problem::NotZeroed { .. }
            | Problem::PartlyCopied { .. }
            | Problem::PartlyZeroed { .. }
            | Problem::CopiedFromElsewhere { .. } => true,
        }
    }

    /// The steps it is about, as indexes into the steps.
    pub fn steps(&self) -> &[usize] {
        match self {
            Problem::RunsBeforeCopy(run) => &run.steps,
            Problem::NotCopied { .. } | Problem::NotZeroed { .. } => &[],
            Problem::PartlyCopied { step, .. }
            | Problem::PartlyZeroed { step, .. }
            | Problem::CopiedFromElsewhere { step, .. } => std::slice::from_ref(step),
            Problem::CantFollow { steps, .. } => steps,
        }
    }

    /// The sections it is about.
    pub fn sections(&self) -> &[usize] {
        match self {
            Problem::RunsBeforeCopy(EarlyRun { section, .. })
            | Problem::NotCopied { section }
            | Problem::NotZeroed { section }
            | Problem::PartlyCopied { section, .. }
            | Problem::PartlyZeroed { section, .. }
            | Problem::CopiedFromElsewhere { section, .. } => std::slice::from_ref(section),
            Problem::CantFollow { sections, .. } => sections,
        }
    }
}

/// Code that runs from a load image before startup code copies it there:
/// an early run, which [`EarlyRunFinder`] finds.
pub(super) struct EarlyRun {
    /// The load image.
    pub section: usize,
    /// The step that copies it, as an index into the steps: none when no
    /// step does.
    pub before: Option<usize>,
    /// The steps of the reset handler whose code holds the calls that run
    /// code from it, as indexes into the steps.
    pub steps: Vec<usize>,
    /// The functions in it that run, in the order they were found.
    pub functions: Vec<EarlyFunction>,
    /// Where the code calls on into it.
    pub sites: Vec<u64>,
    /// Where in the walk the code runs from it.
    pub runs_at: Vec<RunsAt>,
    /// The first of those in the walk (see [`EarlyRun::first`]).
    first: RunsAt,
    /// The unread steps up to where the code first runs from it that may
    /// copy it first, as indexes into the steps.
    pub unless: Vec<usize>,
}

impl EarlyRun {
    /// Code that runs from the load image `section`, which the step at
    /// index `before` copies, first at `at`, before any of it is counted.
    fn new(section: usize, before: Option<usize>, at: RunsAt) -> EarlyRun {
        EarlyRun {
            section,
            before,
            steps: Vec::new(),
            functions: Vec::new(),
            sites: Vec::new(),
            runs_at: Vec::new(),
            first: at,
            unless: Vec::new(),
        }
    }

    /// Whether the code certainly runs before the copy: no unread step
    /// before it may copy the load image first.
    pub fn certain(&self) -> bool {
        self.unless.is_empty()
    }

    /// Whether the function at `address` is one of those that run from the
    /// load image.
    pub fn runs(&self, address: u64) -> bool {
        (self.functions.iter()).any(|early| early.function == address)
    }

    /// Whether the reset handler's own code runs from the load image.
    pub fn reset_runs_from_image(&self) -> bool {
        self.runs_at.contains(&RunsAt::ResetHandler)
    }

    /// The first place in the walk where the code runs from the load
    /// image.
    pub fn first(&self) -> RunsAt {
        self.first
    }
}

/// A function in a load image that runs before startup code copies it.
pub(super) struct EarlyFunction {
    pub function: u64,
    /// The functions that the code goes through to get to `function`,
    /// from the one that the reset handler calls down to `function`
    /// itself; for the reset handler's own code, the reset handler.
    pub chain: Vec<u64>,
}

/// A note under the steps, as facts, which `text::note_text` words.
pub(super) enum Note {
    /// elfq finds no vector table, so it reads from the entry point.
    NoVectorTable,
    /// The vector table at `table` gives `outside` for reset, which is in
    /// no function, so elfq reads from the entry point.
    ResetInNoFunction { table: u64, outside: u64 },
    /// The vector table at `table` gives a reset handler other than the
    /// entry point.
    EntryDiffers { table: u64 },
    /// At `site` the walk takes a branch whose condition it can't tell,
    /// since the other way ends as `because` says.
    Branch { site: u64, because: End },
    /// The steps at `later`, as indexes into the steps, may never run (see
    /// [`Statuses::may_never_run`]), so the sections in `lost` may not be
    /// set up. No problem names those in `unnamed`.
    NotReached {
        later: Vec<usize>,
        lost: Vec<usize>,
        unnamed: Vec<usize>,
    },
    /// Sections without contents that nothing zeroes and no problem names,
    /// which may be meant to stay as they are.
    NotZeroed { sections: Vec<usize> },
    /// A section without contents that the step at index `step` zeroes
    /// `bytes` of, short of its size, which no problem names.
    PartlyZeroed {
        section: usize,
        step: usize,
        bytes: u64,
    },
    /// Sections that code elfq can't follow may set up, which the
    /// can't-follow problem doesn't name, with the unread steps that may,
    /// as indexes into the steps.
    CantTell {
        sections: Vec<usize>,
        steps: Vec<usize>,
    },
}

impl Note {
    /// The JSON's `kind`.
    pub fn field(&self) -> &'static str {
        match self {
            Note::NoVectorTable => "no vector table",
            Note::ResetInNoFunction { .. } => "reset in no function",
            Note::EntryDiffers { .. } => "entry differs",
            Note::Branch { .. } => "branch",
            Note::NotReached { .. } => "not reached",
            Note::NotZeroed { .. } => "not zeroed",
            Note::PartlyZeroed { .. } => "partly zeroed",
            Note::CantTell { .. } => "can't tell",
        }
    }

    /// The sections it is about.
    pub fn sections(&self) -> &[usize] {
        match self {
            Note::NoVectorTable
            | Note::ResetInNoFunction { .. }
            | Note::EntryDiffers { .. }
            | Note::Branch { .. } => &[],
            Note::NotReached { lost, .. } => lost,
            Note::NotZeroed { sections } | Note::CantTell { sections, .. } => sections,
            Note::PartlyZeroed { section, .. } => std::slice::from_ref(section),
        }
    }
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

/// What sets up each section (see [`Setups::new`]).
pub(super) struct Setups {
    /// The first step that sets up each section.
    pub init: HashMap<usize, Setup>,
    /// The steps of code elfq can't follow that may set up each section
    /// that no step does, as indexes into the steps: the unread steps.
    pub unread: HashMap<usize, Vec<usize>>,
}

impl Setups {
    /// What `steps` set up: the first step that sets up each section, and
    /// the unread steps that may set up each of the others.
    fn new(steps: &[Step]) -> Setups {
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
        Setups { init, unread }
    }
}

/// The state of each section when `main` starts, with where the first
/// certain problem is, which it depends on: nothing after that can be
/// counted on (see [`Statuses::new`]).
struct Statuses {
    /// Where the first certain problem is (see [`first_certain`]).
    fatal: Option<RunsAt>,
    /// The state of each RAM section.
    of: HashMap<usize, SectionStatus>,
}

impl Statuses {
    /// The state of each RAM section when `main` starts, when `steps` set
    /// them up as `setups` says and the first certain problem is at
    /// `fatal`.
    fn new(ram: &RamSections, steps: &[Step], setups: &Setups, fatal: Option<RunsAt>) -> Statuses {
        let mut statuses = Statuses {
            fatal,
            of: HashMap::new(),
        };
        for &section in &ram.sections {
            let status = statuses.work_out(ram, steps, setups, section);
            statuses.of.insert(section, status);
        }
        statuses
    }

    /// The state of `section`, one of the RAM sections, when `main` starts,
    /// with the step that sets it up and how many of its bytes.
    fn status(&self, section: usize) -> SectionStatus {
        self.of[&section]
    }

    /// Whether `step` comes before the first certain problem, so that it
    /// can be counted on to run.
    fn reached(&self, step: &Step) -> bool {
        self.fatal
            .is_none_or(|fatal| RunsAt::Event(step.event) < fatal)
    }

    /// Whether `step` may never run: it comes after the first certain
    /// problem, or copies or zeroes at its event, after the code that runs
    /// from a load image there.
    fn may_never_run(&self, step: &Step) -> bool {
        self.fatal.is_some_and(|fatal| {
            let at = RunsAt::Event(step.event);
            at > fatal || (at == fatal && matches!(step.does, Does::Copy | Does::Zero))
        })
    }

    /// Works out the state of `section` when `main` starts, which
    /// [`Statuses::new`] keeps for [`Statuses::status`] to give.
    fn work_out(
        &self,
        ram: &RamSections,
        steps: &[Step],
        setups: &Setups,
        section: usize,
    ) -> SectionStatus {
        let unset = |status| SectionStatus {
            status,
            step: None,
            bytes: Some(0),
        };
        if let Some(setup) = setups.init.get(&section) {
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
        if let Some(unread) = setups.unread.get(&section) {
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

/// What startup's steps come to.
pub(super) struct Judged {
    /// What sets up each section.
    pub setups: Setups,
    /// The state of each section that startup code sets up when `main`
    /// starts (see [`Judged::status`]).
    statuses: Statuses,
    /// Whether elfq followed all the code before `main`.
    pub complete: bool,
    pub problems: Vec<Problem>,
    pub notes: Vec<Note>,
    /// For each load image that holds code, the functions that must stay
    /// out of it (see [`EarlyRunFinder::keep_out`]).
    pub keep_out: Vec<KeepOut>,
}

/// The functions that must stay out of a load image that holds code.
pub(super) struct KeepOut {
    pub section: usize,
    /// The functions that must stay out of it, as item indexes.
    pub items: Vec<usize>,
}

impl Judged {
    /// The state of `section`, one of the RAM sections, when `main` starts,
    /// with the step that sets it up and how many of its bytes.
    pub fn status(&self, section: usize) -> SectionStatus {
        self.statuses.status(section)
    }
}

/// Works out what `steps` come to, in phases: what sets up each section;
/// the code that runs from load images before startup code copies them,
/// and what must stay out of each that holds code; the state of each
/// section when `main` starts, which depends on the first certain problem
/// among that code's; the problems that the states make; and the notes.
pub(super) fn judge(facts: &Facts, steps: &[Step]) -> Judged {
    let setups = Setups::new(steps);
    let mut finder = EarlyRunFinder::new(facts, steps, &setups);
    let runs = finder.find();
    let keep_out = finder.keep_out();
    let statuses = Statuses::new(facts.ram, steps, &setups, first_certain(&runs));
    let ran_from: Vec<usize> = runs.iter().map(|run| run.section).collect();
    let mut problems: Vec<Problem> = runs.into_iter().map(Problem::RunsBeforeCopy).collect();
    problems.extend(section_problems(facts.ram, &setups, &statuses, &ran_from));
    problems.extend(cant_follow_problem(facts.ram, steps, &setups, &statuses));
    let notes = notes(facts, steps, &setups, &statuses, &problems);
    // A walk that ends where it can't read on, or stops short, didn't
    // follow what comes after, though that may set up nothing.
    let complete = !facts.end.stops_short() && !steps.iter().any(|step| step.does == Does::Unread);
    Judged {
        setups,
        statuses,
        complete,
        problems,
        notes,
        keep_out,
    }
}

/// Where the first certain problem is, after which nothing can be counted
/// on: the first place in the walk where code certainly runs from a load
/// image before startup code copies it. Problems of other kinds have no
/// place in the walk.
fn first_certain(runs: &[EarlyRun]) -> Option<RunsAt> {
    (runs.iter())
        .filter(|run| run.certain())
        .map(EarlyRun::first)
        .min()
}

/// The problems that the states of the RAM sections make: a load image
/// that nothing copies, but for one that code runs from, in `ran_from`,
/// which that problem names; `.bss` or `.sbss` not zeroed; and the first
/// step that sets up a section wrongly (see [`set_up_wrongly`]), even
/// when it may never run.
fn section_problems(
    ram: &RamSections,
    setups: &Setups,
    statuses: &Statuses,
    ran_from: &[usize],
) -> Vec<Problem> {
    let problem = |section: usize| match statuses.status(section).status {
        Status::NotCopied => {
            (!ran_from.contains(&section)).then_some(Problem::NotCopied { section })
        }
        Status::NotZeroed => ram
            .zeroed_by_rule(section)
            .then_some(Problem::NotZeroed { section }),
        Status::PartlyCopied
        | Status::PartlyZeroed
        | Status::CopiedFromElsewhere
        | Status::NotReached => set_up_wrongly(ram, section, setups.init.get(&section)?),
        Status::Copied | Status::Zeroed | Status::Unknown => None,
    };
    (ram.sections.iter().copied()).filter_map(problem).collect()
}

/// The problem with `setup`, the first step that sets up `section`, if it
/// sets it up wrongly: it copies the load image from somewhere other than
/// where the file stores it, or it copies a load image, or zeroes `.bss`
/// or `.sbss`, short of the section's size.
fn set_up_wrongly(ram: &RamSections, section: usize, setup: &Setup) -> Option<Problem> {
    let Setup { step, bytes, from } = *setup;
    if let Some(from) = from
        && ram.is_load(section)
        && from != ram.load_address(section)
    {
        return Some(Problem::CopiedFromElsewhere {
            section,
            step,
            from,
        });
    }
    if bytes >= ram.section(section).size || !ram.required(section) {
        return None;
    }
    Some(if ram.is_load(section) {
        Problem::PartlyCopied {
            section,
            step,
            bytes,
        }
    } else {
        Problem::PartlyZeroed {
            section,
            step,
            bytes,
        }
    })
}

/// The problem of code elfq can't follow that may set up a load image,
/// `.bss` or `.sbss` that no step does, with the unread steps before the
/// first certain problem that may; none when it may set up no such
/// section. The other sections that it may set up get a note.
fn cant_follow_problem(
    ram: &RamSections,
    steps: &[Step],
    setups: &Setups,
    statuses: &Statuses,
) -> Option<Problem> {
    let sections: Vec<usize> = (ram.sections.iter().copied())
        .filter(|&section| {
            statuses.status(section).status == Status::Unknown && ram.required(section)
        })
        .collect();
    if sections.is_empty() {
        return None;
    }
    let mut indexes: Vec<usize> = (sections.iter())
        .flat_map(|section| setups.unread[section].iter().copied())
        .filter(|&index| statuses.reached(&steps[index]))
        .collect();
    indexes.sort_unstable();
    indexes.dedup();
    Some(Problem::CantFollow {
        sections,
        steps: indexes,
    })
}

/// Finds the code that runs from load images before startup code copies
/// them (see [`EarlyRunFinder::find`]), and what must stay out of each that
/// holds code (see [`EarlyRunFinder::keep_out`]).
struct EarlyRunFinder<'a> {
    facts: &'a Facts<'a>,
    /// The steps, which copy the load images.
    steps: &'a [Step],
    /// What sets up each section.
    setups: &'a Setups,
    /// The function that is `main`, which the call trees leave out.
    main: Option<u64>,
    /// The call tree of each function (see [`EarlyRunFinder::call_tree`]).
    call_trees: HashMap<u64, CallTree>,
    /// The functions that each function calls or jumps to.
    callees: HashMap<u64, Vec<u64>>,
}

/// The functions that run when a function runs, each with the function
/// that calls it on a shortest chain from the first (see
/// [`EarlyRunFinder::call_tree`]).
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
    fn within(&self, start: u64, end: u64) -> Vec<Hit> {
        let first = (self.by_address).partition_point(|&i| self.found[i].function < start);
        let mut indexes: Vec<usize> = (self.by_address[first..].iter().copied())
            .take_while(|&i| self.found[i].function < end)
            .collect();
        indexes.sort_unstable();
        (indexes.into_iter())
            .map(|i| Hit {
                function: self.found[i].function,
                chain: self.chain(i),
            })
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

/// A function in a load image that the reset handler or a call runs,
/// with the chain of functions that gets to it: for one that a call's
/// call tree finds, from the function that the call goes to down to it
/// (see [`CallTree::within`]); otherwise the function alone.
#[derive(Debug, Clone)]
struct Hit {
    function: u64,
    chain: Vec<u64>,
}

impl Hit {
    /// The function at `function`, which the code runs itself: its chain
    /// is the function alone.
    fn alone(function: u64) -> Hit {
        Hit {
            function,
            chain: vec![function],
        }
    }
}

/// What runs code from a load image (see [`EarlyRunFinder::add`]).
enum RunBy {
    /// The reset handler, whose own code is in it.
    ResetHandler,
    /// The call at `event`, from `site`, in the step at index `step`,
    /// inside the calls to `callers`, from the reset handler's own call
    /// down.
    Call {
        event: usize,
        site: u64,
        step: Option<usize>,
        callers: Vec<u64>,
    },
}

impl RunBy {
    /// Where in the walk it runs the code.
    fn at(&self) -> RunsAt {
        match self {
            RunBy::ResetHandler => RunsAt::ResetHandler,
            RunBy::Call { event, .. } => RunsAt::Event(*event),
        }
    }
}

/// A call that runs before the step that copies a load image (see
/// [`EarlyRunFinder::calls_before`]).
#[derive(Debug, Clone, Copy)]
struct CallBefore {
    event: usize,
    /// The function that the call goes to.
    target: u64,
    /// Whether the call holds the copy, so that only the function's own
    /// code runs before it.
    holds_copy: bool,
}

impl<'a> EarlyRunFinder<'a> {
    /// What finds the code that runs from load images before `steps`,
    /// which set up the sections as `setups` says, copy them.
    fn new(facts: &'a Facts<'a>, steps: &'a [Step], setups: &'a Setups) -> EarlyRunFinder<'a> {
        EarlyRunFinder {
            facts,
            steps,
            setups,
            main: facts.main.and_then(Main::function),
            call_trees: HashMap::new(),
            callees: HashMap::new(),
        }
    }

    /// The code that runs from each load image before the steps copy it,
    /// one early run for each: the reset handler's own code, and what each
    /// call runs (see [`EarlyRunFinder::add_calls`]). An early run leaves
    /// out the veneers among its functions, unless all are veneers, and
    /// holds the unread steps that may copy its load image first.
    fn find(&mut self) -> Vec<EarlyRun> {
        let Facts {
            firmware,
            ram,
            reset,
            ..
        } = *self.facts;
        let mut runs = Vec::new();
        let boot = reset.address;
        for &section in &ram.loads {
            if ram.section(section).address <= boot && boot < ram.end(section) {
                let hits = [Hit::alone(boot)];
                self.add(&mut runs, section, RunBy::ResetHandler, &hits);
            }
        }
        for &section in &ram.loads {
            self.add_calls(&mut runs, section);
        }
        for run in &mut runs {
            if (run.functions.iter()).any(|early| !firmware.is_veneer(early.function)) {
                (run.functions).retain(|early| !firmware.is_veneer(early.function));
            }
            let first = run.first();
            run.unless = (self.steps.iter().enumerate())
                .filter(|(_, step)| {
                    step.does == Does::Unread
                        && RunsAt::Event(step.event) <= first
                        && step.may.contains(&run.section)
                })
                .map(|(index, _)| index)
                .collect();
        }
        runs
    }

    /// Adds to `runs` the code that each call before the step that copies
    /// the load image `section` runs from it: the function that a call
    /// holding the copy goes to, when it is in the load image, and for
    /// other calls, the functions of its call tree that are; with a
    /// veneer in the load image that the call goes through.
    fn add_calls(&mut self, runs: &mut Vec<EarlyRun>, section: usize) {
        let Facts { ram, events, .. } = *self.facts;
        let (start, stop) = (ram.section(section).address, ram.end(section));
        for CallBefore {
            event,
            target,
            holds_copy,
        } in self.calls_before(section)
        {
            let Some(call) = events.call(event) else {
                continue;
            };
            let mut hits = if holds_copy {
                (start..stop)
                    .contains(&target)
                    .then(|| Hit::alone(target))
                    .into_iter()
                    .collect()
            } else {
                self.call_tree(target).within(start, stop)
            };
            if let Some(veneer) = events
                .veneer(event)
                .filter(|veneer| (start..stop).contains(veneer))
            {
                match hits.iter_mut().find(|hit| hit.function == veneer) {
                    Some(hit) => hit.chain = vec![veneer],
                    None => hits.push(Hit::alone(veneer)),
                }
            }
            if !hits.is_empty() {
                let by = RunBy::Call {
                    event,
                    site: call.site,
                    step: step_at(self.steps, event),
                    callers: enclosing(events, event),
                };
                self.add(runs, section, by, &hits);
            }
        }
    }

    /// The calls that run before the step that copies the load image
    /// `section`, or before `main` when no step does, the copy's own call
    /// among them. Calls to `main` don't count.
    fn calls_before(&self, section: usize) -> Vec<CallBefore> {
        let events = self.facts.events;
        let end = (self.setups.init.get(&section))
            .map_or(events.list.len(), |setup| self.steps[setup.step].event);
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
    fn keep_out(&mut self) -> Vec<KeepOut> {
        let Facts {
            firmware,
            ram,
            reset,
            ..
        } = *self.facts;
        let mut rows = Vec::new();
        for &section in &ram.sections {
            if !ram.is_load(section) || !ram.section(section).is_code() {
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
            for call in self.calls_before(section) {
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
    /// runs from the load image `section` when `by` runs it, in its early
    /// run in `runs`.
    fn add(&self, runs: &mut Vec<EarlyRun>, section: usize, by: RunBy, hits: &[Hit]) {
        // The reset handler's own code is in the load image; a call's
        // code calls on into it.
        let (step, callers, sites): (Option<usize>, &[u64], Vec<u64>) = match &by {
            RunBy::ResetHandler => (None, &[], hits.iter().map(|hit| hit.function).collect()),
            RunBy::Call {
                site,
                step,
                callers,
                ..
            } => (
                *step,
                callers,
                (hits.iter())
                    .flat_map(|hit| self.use_sites(&hit.chain, *site))
                    .collect(),
            ),
        };
        let index = match runs.iter().position(|run| run.section == section) {
            Some(index) => index,
            None => {
                let before = self.setups.init.get(&section).map(|setup| setup.step);
                runs.push(EarlyRun::new(section, before, by.at()));
                runs.len() - 1
            }
        };
        let run = &mut runs[index];
        for hit in hits {
            if !run.runs(hit.function) {
                run.functions.push(EarlyFunction {
                    function: hit.function,
                    chain: callers.iter().chain(&hit.chain).copied().collect(),
                });
            }
        }
        run.runs_at.push(by.at());
        run.first = run.first.min(by.at());
        if let Some(step) = step
            && !run.steps.contains(&step)
        {
            run.steps.push(step);
        }
        for site in sites {
            if !run.sites.contains(&site) {
                run.sites.push(site);
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

/// The notes under the steps, as `problems` leave them: where the walk
/// starts and the branches it took, the steps that may never run, the
/// sections without contents that may be meant to stay as they are, and
/// those that code elfq can't follow may set up, which no problem names.
fn notes(
    facts: &Facts,
    steps: &[Step],
    setups: &Setups,
    statuses: &Statuses,
    problems: &[Problem],
) -> Vec<Note> {
    let ram = facts.ram;
    // The sections that problems say are not set up as they should be,
    // which need no note.
    let named: Vec<usize> = (problems.iter())
        .filter(|p| !matches!(p, Problem::CantFollow { .. }))
        .flat_map(|p| p.sections().iter().copied())
        .collect();
    let mut notes = walk_notes(facts);
    notes.extend(not_reached_note(ram, steps, statuses, &named));
    notes.extend(zeroing_notes(ram, statuses, &named));
    notes.extend(cant_tell_note(ram, setups, statuses, problems));
    notes
}

/// The notes on where the walk starts, when not at the reset handler that
/// the vector table gives as the entry point, and on each branch whose
/// condition it can't tell.
fn walk_notes(facts: &Facts) -> Vec<Note> {
    let Facts {
        firmware,
        reset,
        events,
        ..
    } = *facts;
    let mut notes = Vec::new();
    match (reset.table, reset.outside) {
        (None, _) => notes.push(Note::NoVectorTable),
        (Some(table), Some(outside)) => notes.push(Note::ResetInNoFunction { table, outside }),
        (Some(table), None) if firmware.binary.entry != reset.address => {
            notes.push(Note::EntryDiffers { table });
        }
        (Some(_), None) => {}
    }
    for event in events.list {
        if let Event::Backtrack { site, because, .. } = *event {
            notes.push(Note::Branch { site, because });
        }
    }
    notes
}

/// The note on the steps that may never run, with the sections that may
/// not be set up, saying which of them no problem names, as `named`
/// lists those it does: none when every step can be counted on.
fn not_reached_note(
    ram: &RamSections,
    steps: &[Step],
    statuses: &Statuses,
    named: &[usize],
) -> Option<Note> {
    let later: Vec<usize> = (steps.iter().enumerate())
        .filter(|(_, step)| statuses.may_never_run(step))
        .map(|(index, _)| index)
        .collect();
    if later.is_empty() {
        return None;
    }
    let lost: Vec<usize> = (ram.sections.iter().copied())
        .filter(|&section| statuses.status(section).status == Status::NotReached)
        .collect();
    let unnamed = (lost.iter().copied())
        .filter(|section| !named.contains(section))
        .collect();
    Some(Note::NotReached {
        later,
        lost,
        unnamed,
    })
}

/// The notes on the sections without contents that no problem names, as
/// `named` lists those it does, which may be meant to stay as they are:
/// one for those that nothing zeroes, then one for each that a step zeroes
/// short.
fn zeroing_notes(ram: &RamSections, statuses: &Statuses, named: &[usize]) -> Vec<Note> {
    let mut notes = Vec::new();
    let unnamed = || (ram.zeros.iter().copied()).filter(|section| !named.contains(section));
    let not_zeroed: Vec<usize> = unnamed()
        .filter(|&section| statuses.status(section).status == Status::NotZeroed)
        .collect();
    if !not_zeroed.is_empty() {
        notes.push(Note::NotZeroed {
            sections: not_zeroed,
        });
    }
    for section in unnamed() {
        if let SectionStatus {
            status: Status::PartlyZeroed,
            step: Some(step),
            bytes: Some(bytes),
        } = statuses.status(section)
        {
            notes.push(Note::PartlyZeroed {
                section,
                step,
                bytes,
            });
        }
    }
    notes
}

/// The note on the sections that code elfq can't follow may set up, but
/// for those that the can't-follow problem among `problems` names, with
/// the unread steps that may: none when there are none.
fn cant_tell_note(
    ram: &RamSections,
    setups: &Setups,
    statuses: &Statuses,
    problems: &[Problem],
) -> Option<Note> {
    let in_problem: Vec<usize> = (problems.iter())
        .filter(|p| matches!(p, Problem::CantFollow { .. }))
        .flat_map(|p| p.sections().iter().copied())
        .collect();
    let sections: Vec<usize> = (ram.sections.iter().copied())
        .filter(|&section| {
            statuses.status(section).status == Status::Unknown && !in_problem.contains(&section)
        })
        .collect();
    if sections.is_empty() {
        return None;
    }
    let mut indexes: Vec<usize> = (sections.iter())
        .flat_map(|section| setups.unread[section].iter().copied())
        .collect();
    indexes.sort_unstable();
    indexes.dedup();
    Some(Note::CantTell {
        sections,
        steps: indexes,
    })
}

#[cfg(test)]
mod tests;
