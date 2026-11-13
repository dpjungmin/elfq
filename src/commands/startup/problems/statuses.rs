//! The state of each section when `main` starts ([`Statuses`]): the first
//! step that sets it up, or the unread steps that may ([`Setups::new`]);
//! whether that step comes before the first certain problem, so that it
//! can be counted on to run; and the state it leaves the section in
//! ([`setup_status`]). `mod.rs` holds the states themselves ([`Status`]).

use std::collections::HashMap;

use super::super::ram_sections::RamSections;
use super::super::steps::{Does, Step};
use super::{RunsAt, SectionStatus, Setup, Setups, Status};

impl Setups {
    /// What `steps` set up: the first step that sets up each section, and
    /// the unread steps that may set up each of the others.
    pub(super) fn new(steps: &[Step]) -> Setups {
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
pub(super) struct Statuses {
    /// Where the first certain problem is (see
    /// [`first_certain`](super::first_certain)).
    fatal: Option<RunsAt>,
    /// The state of each RAM section.
    of: HashMap<usize, SectionStatus>,
}

impl Statuses {
    /// The state of each RAM section when `main` starts, when `steps` set
    /// them up as `setups` says and the first certain problem is at
    /// `fatal`.
    pub(super) fn new(
        ram: &RamSections,
        steps: &[Step],
        setups: &Setups,
        fatal: Option<RunsAt>,
    ) -> Statuses {
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
    pub(super) fn status(&self, section: usize) -> SectionStatus {
        self.of[&section]
    }

    /// Whether `step` comes before the first certain problem, so that it
    /// can be counted on to run.
    pub(super) fn reached(&self, step: &Step) -> bool {
        self.fatal
            .is_none_or(|fatal| RunsAt::Event(step.event) < fatal)
    }

    /// Whether `step` may never run: it comes after the first certain
    /// problem, or copies or zeroes at its event, after the code that runs
    /// from a load image there.
    pub(super) fn may_never_run(&self, step: &Step) -> bool {
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
            return SectionStatus {
                status: setup_status(ram, section, setup),
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

/// The state that `setup`, the first step that sets up `section`, leaves
/// it in when that step runs: a load image copied, partly copied, or
/// copied from somewhere other than where the file stores it, and a
/// section without contents zeroed or partly zeroed. It doesn't ask
/// whether the step runs: [`Statuses::work_out`] asks that first, and
/// [`set_up_wrongly`](super::set_up_wrongly) asks about steps that may
/// never run too.
pub(super) fn setup_status(ram: &RamSections, section: usize, setup: &Setup) -> Status {
    let Setup { bytes, from, .. } = *setup;
    let size = ram.section(section).size;
    if ram.is_load(section) {
        if from.is_some_and(|from| from != ram.load_address(section)) {
            Status::CopiedFromElsewhere
        } else if bytes >= size {
            Status::Copied
        } else {
            Status::PartlyCopied
        }
    } else if bytes >= size {
        Status::Zeroed
    } else {
        Status::PartlyZeroed
    }
}
