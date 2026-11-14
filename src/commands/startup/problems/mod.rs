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
//!
//! This file holds what a judgment gives, which `text.rs` and `json.rs`
//! write: the state of each section ([`Status`]), the problems
//! ([`Problem`]) and the notes ([`Note`]), gathered in [`Judged`]. It also
//! holds [`judge`], which works them out in phases, and the phase that
//! finds the problems that the states of the sections make
//! ([`section_problems`], [`set_up_wrongly`] and [`cant_follow_problem`]).
//! The other files each take a phase: `statuses.rs` works out what sets
//! up each section and the state of each when `main` starts
//! ([`Statuses`], and [`setup_status`] for the state that the first step
//! that sets up a section leaves it in), `early_run.rs` finds the code
//! that runs from load images before startup code copies them, which are
//! problems too, and what must stay out of each that holds code
//! ([`EarlyRunFinder`]), and `notes.rs` picks the notes under the steps.
//! The unit tests are in `tests.rs`.

mod early_run;
mod notes;
mod statuses;

use std::collections::HashMap;

use super::Facts;
use super::ram_sections::RamSections;
use super::steps::{Does, Step};
use crate::arch::startup::End;
use early_run::EarlyRunFinder;
use notes::notes;
use statuses::{Statuses, setup_status};

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
    pub(super) fn field(self) -> &'static str {
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
    pub(super) fn event(self) -> Option<usize> {
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
    pub(super) fn field(&self) -> &'static str {
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
    pub(super) fn certain(&self) -> bool {
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
    pub(super) fn steps(&self) -> &[usize] {
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
    pub(super) fn sections(&self) -> &[usize] {
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
    pub(super) section: usize,
    /// The step that copies it, as an index into the steps: none when no
    /// step does.
    pub(super) before: Option<usize>,
    /// The steps of the reset handler whose code holds the calls that run
    /// code from it, as indexes into the steps.
    pub(super) steps: Vec<usize>,
    /// The functions in it that run, in the order they were found.
    pub(super) functions: Vec<EarlyFunction>,
    /// Where the code calls on into it.
    pub(super) sites: Vec<u64>,
    /// Where in the walk the code runs from it.
    pub(super) runs_at: Vec<RunsAt>,
    /// The first of those in the walk (see [`EarlyRun::first`]).
    first: RunsAt,
    /// The unread steps up to where the code first runs from it that may
    /// copy it first, as indexes into the steps.
    pub(super) unless: Vec<usize>,
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
    fn certain(&self) -> bool {
        self.unless.is_empty()
    }

    /// Whether the function at `address` is one of those that run from the
    /// load image.
    pub(super) fn runs(&self, address: u64) -> bool {
        (self.functions.iter()).any(|early| early.function == address)
    }

    /// Whether the reset handler's own code runs from the load image.
    pub(super) fn reset_runs_from_image(&self) -> bool {
        self.runs_at.contains(&RunsAt::ResetHandler)
    }

    /// The first place in the walk where the code runs from the load
    /// image.
    fn first(&self) -> RunsAt {
        self.first
    }
}

/// A function in a load image that runs before startup code copies it.
pub(super) struct EarlyFunction {
    pub(super) function: u64,
    /// The functions that the code goes through to get to `function`,
    /// from the one that the reset handler calls down to `function`
    /// itself; for the reset handler's own code, the reset handler.
    pub(super) chain: Vec<u64>,
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
    pub(super) fn field(&self) -> &'static str {
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
    pub(super) fn sections(&self) -> &[usize] {
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
    pub(super) step: usize,
    bytes: u64,
    from: Option<u64>,
}

/// The state of a section when `main` starts (see [`Judged::status`]).
#[derive(Debug, Clone, Copy)]
pub(super) struct SectionStatus {
    pub(super) status: Status,
    /// The step that sets the section up, as an index into the steps.
    pub(super) step: Option<usize>,
    /// How many of its bytes are set up: none when elfq can't tell.
    pub(super) bytes: Option<u64>,
}

/// What sets up each section (see [`Setups::new`]).
pub(super) struct Setups {
    /// The first step that sets up each section.
    pub(super) init: HashMap<usize, Setup>,
    /// The steps of code elfq can't follow that may set up each section
    /// that no step does, as indexes into the steps: the unread steps.
    pub(super) unread: HashMap<usize, Vec<usize>>,
}

/// What startup's steps come to.
pub(super) struct Judged {
    /// What sets up each section.
    pub(super) setups: Setups,
    /// The state of each section that startup code sets up when `main`
    /// starts (see [`Judged::status`]).
    statuses: Statuses,
    /// Whether elfq followed all the code before `main`.
    pub(super) complete: bool,
    pub(super) problems: Vec<Problem>,
    pub(super) notes: Vec<Note>,
    /// For each load image that holds code, the functions that must stay
    /// out of it (see [`EarlyRunFinder::keep_out`]).
    pub(super) keep_out: Vec<KeepOut>,
}

/// The functions that must stay out of a load image that holds code.
pub(super) struct KeepOut {
    pub(super) section: usize,
    /// The functions that must stay out of it, as item indexes.
    pub(super) items: Vec<usize>,
}

impl Judged {
    /// The state of `section`, one of the RAM sections, when `main` starts,
    /// with the step that sets it up and how many of its bytes.
    pub(super) fn status(&self, section: usize) -> SectionStatus {
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
/// sets it up wrongly, by the state it leaves the section in (see
/// [`setup_status`]): it copies the load image from somewhere other than
/// where the file stores it, or it copies a load image, or zeroes `.bss`
/// or `.sbss`, short of the section's size.
fn set_up_wrongly(ram: &RamSections, section: usize, setup: &Setup) -> Option<Problem> {
    let Setup { step, bytes, from } = *setup;
    match setup_status(ram, section, setup) {
        // setup_status gives this only for a step that copies from an
        // address, so `from` is some.
        Status::CopiedFromElsewhere => from.map(|from| Problem::CopiedFromElsewhere {
            section,
            step,
            from,
        }),
        Status::PartlyCopied => ram.required(section).then_some(Problem::PartlyCopied {
            section,
            step,
            bytes,
        }),
        Status::PartlyZeroed => ram.required(section).then_some(Problem::PartlyZeroed {
            section,
            step,
            bytes,
        }),
        Status::Copied | Status::Zeroed => None,
        // setup_status never gives these: work_out gives them to a section
        // that no step sets up, or whose first step doesn't come before the
        // first certain problem (see `Statuses::reached`).
        Status::NotCopied | Status::NotZeroed | Status::Unknown | Status::NotReached => None,
    }
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

#[cfg(test)]
mod tests;
