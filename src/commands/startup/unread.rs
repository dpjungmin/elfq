//! What code that elfq can't follow may set up. Such code is a step that
//! does [`Does::Unread`], with the sections it may set up in `may`. A loop
//! that elfq can't match may set up the sections where its own stores go
//! ([`own_stores`], [`loop_targets`]); else those of the kind that a loop
//! in its code copies or zeroes, as a loop over a table of sections does
//! for its first row ([`loop_repeats`]); else, when it calls code that
//! sets up a section, any not set up yet ([`loop_calls`]). A loop that the
//! walk went through one iteration at a time is such a loop too, when its
//! iterations' steps don't stand for all they store
//! ([`unfollowed_iterations`]).

use std::collections::HashSet;
use std::ops::Range;

use super::events::Events;
use super::ram_sections::Memory;
use super::steps::{Does, Step, Way};
use crate::arch::startup::{Event, Loop, Store};

/// Makes each loop of `walked`, which the walk went through each time,
/// with the function that runs it, a loop that elfq can't follow, as it is
/// when the walk goes through it once, when steps don't stand for all its
/// times' stores into the sections: as when each time stores words by
/// several stores, which don't join across times, or runs a loop inside
/// that the walk went through each time too, or stores through a pointer
/// that it can't tell, in the loop's code or in a function it calls. Then
/// the loop's step stands for its times' steps that copy or zero part of
/// what it may set up. Leaves out the stores of events that are
/// `skipped`, whose own steps stand for them, and the loops inside one
/// that it makes such a loop.
pub(super) fn unfollowed_iterations(
    memory: &Memory,
    events: &Events,
    walked: &[(usize, u64)],
    skipped: impl Fn(usize) -> bool,
    steps: &mut Vec<Step>,
) {
    let mut unfollowed: Vec<Range<usize>> = Vec::new();
    // Outer loops first.
    let mut walked = walked.to_vec();
    walked.sort_by_key(|&(k, _)| match &events.list[k] {
        Event::Loop(l) => (l.events.start, usize::MAX - k),
        _ => (k, 0),
    });
    for (k, function) in walked {
        let Event::Loop(l) = &events.list[k] else {
            continue;
        };
        if unfollowed.iter().any(|run| run.contains(&l.events.start)) {
            continue;
        }
        let covered = |at: u32, size: u32| {
            let (at, size) = (u64::from(at), u64::from(size.max(1)));
            steps.iter().any(|step| {
                matches!(step.does, Does::Copy | Does::Zero)
                    && (step.to.zip(step.size))
                        .is_some_and(|(to, bytes)| to <= at && at + size <= to + bytes)
            })
        };
        let stores: Vec<&Store> = (l.events.clone())
            .filter(|&j| !skipped(j))
            .filter_map(|j| match &events.list[j] {
                Event::Store(store) if !store.conditional => Some(store),
                _ => None,
            })
            .filter(|store| match store.address {
                Some(at) => memory.holding(u64::from(at)).is_some() && !covered(at, store.size),
                None => true,
            })
            .collect();
        let mut may = loop_targets(memory, &stores, &HashSet::new());
        if may.is_empty() {
            continue;
        }
        let part = |step: &Step| {
            l.events.contains(&step.event)
                && matches!(step.does, Does::Copy | Does::Zero)
                && (step.covers.iter()).any(|cover| may.contains(&cover.section))
        };
        let parts: Vec<usize> = (steps.iter())
            .filter(|step| part(step))
            .flat_map(|step| step.covers.iter().map(|cover| cover.section))
            .collect();
        steps.retain(|step| !part(step));
        for x in parts {
            if !may.contains(&x) {
                may.push(x);
            }
        }
        may.sort_by_key(|&x| memory.section(x).address);
        let level = events.level(k);
        let mut step = Step::new(k, l.head, Does::Unread, Way::Loop, function, level);
        step.function = Some(function);
        step.may = may;
        let at = steps.partition_point(|step| step.event <= k);
        steps.insert(at, step);
        unfollowed.push(l.events.clone());
    }
}

/// The sections a loop that elfq can't follow may set up: the run of
/// sections that its stores start in, or when it stores through a pointer
/// the walk can't tell, any not set up yet, or only those without
/// contents when it stores only zeros. None when it stores only elsewhere,
/// as to peripherals.
pub(super) fn loop_targets(
    memory: &Memory,
    stores: &[&Store],
    ready: &HashSet<usize>,
) -> Vec<usize> {
    let mut may: Vec<usize> = Vec::new();
    for store in stores {
        if store.base.is_none() && store.address.is_none() {
            let zeros_only = stores.iter().all(|store| store.value == Some(0));
            return (memory.ram.iter().copied())
                .filter(|x| !ready.contains(x) && (!zeros_only || !memory.is_load(*x)))
                .collect();
        }
        // The pointer, and where the store goes, which may be past it.
        for at in [store.base, store.address].into_iter().flatten() {
            if let Some(x) = memory.holding(u64::from(at)) {
                for y in memory.run_from(x) {
                    if !ready.contains(&y) && !may.contains(&y) {
                        may.push(y);
                    }
                }
            }
        }
    }
    may
}

/// The stores of the loop `l`, whose event is at `k`: its own, not those
/// of a loop inside it, nor of what it calls.
pub(super) fn own_stores<'w>(events: &Events<'w>, k: usize, l: &Loop) -> Vec<&'w Store> {
    let level = events.level(k);
    let inner: Vec<Range<usize>> = (l.events.clone())
        .filter_map(|j| match &events.list[j] {
            Event::Loop(inner) if events.level(j) == level => Some(inner.events.clone()),
            _ => None,
        })
        .collect();
    (l.events.clone())
        .filter(|&j| events.level(j) == level && !inner.iter().any(|range| range.contains(&j)))
        .filter_map(|j| match &events.list[j] {
            Event::Store(store) => Some(store),
            _ => None,
        })
        .collect()
}

/// When a loop's code calls a function that copies, zeroes, or stores
/// into a section, as a loop over a table of sections does, every
/// section not set up yet.
pub(super) fn loop_calls(
    memory: &Memory,
    events: &Events,
    k: usize,
    l: &Loop,
    ready: &HashSet<usize>,
) -> Vec<usize> {
    let level = events.level(k);
    let stores_into_ram = |j: usize| match &events.list[j] {
        Event::Store(store) => match store.address.or(store.base) {
            Some(at) => memory.holding(u64::from(at)).is_some(),
            None => true,
        },
        _ => false,
    };
    let sets_up = (l.events.clone()).any(|j| {
        events.level(j) == level
            && events.call(j).is_some_and(|call| {
                memory.match_call(&call.passes()).is_some()
                    || (j + 1..events.span(j)).any(stores_into_ram)
            })
    });
    if !sets_up {
        return Vec::new();
    }
    (memory.ram.iter().copied())
        .filter(|x| !ready.contains(x))
        .collect()
}

/// When the code of a loop that elfq can't follow has a loop that copies
/// or zeroes, as a loop over a table of sections does for its first row,
/// the sections of that kind not set up yet, which the loop's next times
/// may set up.
pub(super) fn loop_repeats(
    memory: &Memory,
    steps: &[Step],
    l: &Loop,
    ready: &HashSet<usize>,
) -> Vec<usize> {
    let mut may: Vec<usize> = Vec::new();
    let inner = (steps.iter()).filter(|step| step.way == Way::Loop && step.event >= l.events.start);
    for step in inner {
        let sections = match step.does {
            Does::Copy => &memory.loads,
            Does::Zero => &memory.zeros,
            _ => continue,
        };
        for &x in &memory.ram {
            if sections.contains(&x) && !ready.contains(&x) && !may.contains(&x) {
                may.push(x);
            }
        }
    }
    may
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{
        alone, binary, closed, each, steps, stored, timed_inside, walked_each,
    };
    use super::*;

    #[test]
    fn a_store_through_a_pointer_in_a_loop_walked_each_time_is_unread() {
        let binary = binary();
        // Each time stores 0 past a pointer that the walk can't tell, which
        // may zero any section without contents.
        let mut list: Vec<Event> = (0..3)
            .map(|_| {
                Event::Store(Store {
                    address: None,
                    base: None,
                    ..stored(0, Some(0), None)
                })
            })
            .collect();
        list.push(walked_each(&[0..1, 1..2, 2..3]));
        assert_eq!(
            steps(&binary, &list),
            ["unread 0x0 0 by a loop: .bss, .noinit"]
        );
    }

    #[test]
    fn a_loop_walked_each_time_whose_steps_dont_stand_for_its_stores_is_unread() {
        let binary = binary();
        // Each time zeroes the first 16 bytes of .bss by a loop, and stores
        // a word past them: the loop may set up .bss, and what follows it.
        let row = |at: usize| {
            [
                closed(vec![each(0x2000_0030, 4, 4, 4, Some(0))], at..at),
                alone(0x800_0030, Some(0x2000_0050), 0x2000_0050, None, Some(1)),
            ]
        };
        let mut list: Vec<Event> = row(0).into_iter().chain(row(2)).collect();
        list.push(walked_each(&[0..2, 2..4]));
        assert_eq!(
            steps(&binary, &list),
            ["unread 0x0 0 by a loop: .bss, .noinit"]
        );
        // A loop walked each time inside one: only the outer loop is one
        // that elfq can't follow.
        let store = || alone(0x800_0030, Some(0x2000_0050), 0x2000_0050, None, Some(1));
        let list = [
            store(),
            store(),
            timed_inside(&[0..1, 1..2]),
            store(),
            store(),
            timed_inside(&[3..4, 4..5]),
            walked_each(&[0..3, 3..6]),
        ];
        assert_eq!(
            steps(&binary, &list),
            ["unread 0x0 0 by a loop: .bss, .noinit"]
        );
    }

    #[test]
    fn a_loop_elfq_cant_match_may_set_up_where_its_stores_go() {
        let binary = binary();
        let memory = Memory::new(&binary);
        // A store 4 bytes past a pointer into .ramfunc goes to .bss.
        let mut store = stored(0x2000_0030, None, None);
        store.base = Some(0x2000_002c);
        let names = |stores: &[&Store]| -> Vec<&str> {
            (loop_targets(&memory, stores, &HashSet::new()).into_iter())
                .map(|x| memory.name(x))
                .collect()
        };
        assert_eq!(names(&[&store]), [".ramfunc", ".bss", ".noinit"]);
        // Through a pointer the walk can't tell, any section not set up
        // yet, or only those without contents for a store of 0.
        store.base = None;
        store.address = None;
        assert_eq!(names(&[&store]), [".data", ".ramfunc", ".bss", ".noinit"]);
        store.value = Some(0);
        assert_eq!(names(&[&store]), [".bss", ".noinit"]);
    }
}
