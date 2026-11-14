//! The notes under the steps, as the problems leave them: where the walk
//! starts and the branches it took, the steps that may never run, the
//! sections without contents that may be meant to stay as they are, and
//! those that code elfq can't follow may set up.

use super::super::Facts;
use super::super::ram_sections::RamSections;
use super::super::steps::Step;
use super::statuses::Statuses;
use super::{Note, Problem, SectionStatus, Setups, Status};
use crate::arch::startup::Event;

/// The notes under the steps, as `problems` leave them: where the walk
/// starts and the branches it took, the steps that may never run, the
/// sections without contents that may be meant to stay as they are, and
/// those that code elfq can't follow may set up, which no problem names.
pub(super) fn notes(
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
