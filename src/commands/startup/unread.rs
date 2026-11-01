//! What code that elfq can't follow may set up. Such code is a step that
//! does [`Does::Unread`], with the sections it may set up in `may`. A loop
//! that elfq can't match may set up the sections where its own stores go
//! ([`own_stores`], [`loop_targets`]); else those of the kind that a loop
//! in its code copies or zeroes, as a loop over a table of sections does
//! for its first row ([`loop_repeats`]); else, when it calls code that
//! sets up a section, any not set up yet ([`loop_calls`]). A loop that the
//! walk went through one iteration at a time is such a loop too, when its
//! iterations' steps don't stand for all they store
//! ([`unread_walked_loops`]).

use std::collections::HashSet;
use std::ops::Range;

use super::events::Events;
use super::ram_sections::RamSections;
use super::steps::{Does, Step, Way};
use crate::arch::startup::{Event, Loop, Store};

/// A loop that the walk went through one iteration at a time, by its
/// event, with the function that runs it.
#[derive(Debug, Clone, Copy)]
pub(super) struct WalkedLoop {
    pub event: usize,
    pub function: u64,
}

/// Makes each of the `walked` loops a loop that elfq can't follow, as it
/// is when the walk goes through it once, when steps don't stand for all
/// its iterations' stores into the sections. Outer loops go first, and
/// for each:
///
/// 1. A loop inside one that this has made such a loop is left out: the
///    outer loop's step stands for it.
/// 2. The stores that steps don't stand for are its iterations' stores,
///    in its code or in a function it calls, that go where the walk can't
///    tell, or into a section that startup code sets up where no step
///    that copies or zeroes covers them; as when each iteration stores
///    words by several stores, which don't join across iterations, or
///    runs a loop inside that the walk went through one iteration at a
///    time too. Conditional stores, and those of events that are
///    `skipped`, whose own steps stand for them, don't count.
/// 3. When those stores may set up no section (see [`loop_targets`]), the
///    loop is left as it is.
/// 4. Else its step stands for its iterations' steps that copy or zero
///    part of what it may set up: they go, and it may set up what they
///    did too.
pub(super) fn unread_walked_loops(
    ram: &RamSections,
    events: &Events,
    walked: &[WalkedLoop],
    skipped: impl Fn(usize) -> bool,
    steps: &mut Vec<Step>,
) {
    // The events of the loops that this has made loops that elfq can't
    // follow.
    let mut made_unread: Vec<Range<usize>> = Vec::new();
    // Outer loops first.
    let mut walked = walked.to_vec();
    walked.sort_by_key(|walked| match &events.list[walked.event] {
        Event::Loop(l) => (l.events.start, usize::MAX - walked.event),
        _ => (walked.event, 0),
    });
    for WalkedLoop {
        event: loop_event,
        function,
    } in walked
    {
        let Event::Loop(l) = &events.list[loop_event] else {
            continue;
        };
        // 1.
        if (made_unread.iter()).any(|made| made.contains(&l.events.start)) {
            continue;
        }
        // 2.
        let covered = |at: u32, size: u32| {
            let (at, size) = (u64::from(at), u64::from(size.max(1)));
            steps.iter().any(|step| {
                matches!(step.does, Does::Copy | Does::Zero)
                    && (step.to.zip(step.size))
                        .is_some_and(|(to, bytes)| to <= at && at + size <= to + bytes)
            })
        };
        let stores: Vec<&Store> = (l.events.clone())
            .filter(|&event| !skipped(event))
            .filter_map(|event| match &events.list[event] {
                Event::Store(store) if !store.conditional => Some(store),
                _ => None,
            })
            .filter(|store| match store.address {
                Some(at) => ram.holding(u64::from(at)).is_some() && !covered(at, store.size),
                None => true,
            })
            .collect();
        // 3.
        let mut may = loop_targets(ram, &stores, &HashSet::new());
        if may.is_empty() {
            continue;
        }
        // 4.
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
        for section in parts {
            if !may.contains(&section) {
                may.push(section);
            }
        }
        may.sort_by_key(|&section| ram.section(section).address);
        let level = events.level(loop_event);
        let mut step = Step::new(loop_event, l.head, Does::Unread, Way::Loop, function, level);
        step.function = Some(function);
        step.may = may;
        let at = steps.partition_point(|step| step.event <= loop_event);
        steps.insert(at, step);
        made_unread.push(l.events.clone());
    }
}

/// The sections a loop that elfq can't follow may set up: the run of
/// sections that its stores start in, or when it stores through a pointer
/// the walk can't tell, any not set up yet, or only those without
/// contents when it stores only zeros. None when it stores only elsewhere,
/// as to peripherals.
pub(super) fn loop_targets(
    ram: &RamSections,
    stores: &[&Store],
    ready: &HashSet<usize>,
) -> Vec<usize> {
    let mut may: Vec<usize> = Vec::new();
    for store in stores {
        if store.base.is_none() && store.address.is_none() {
            let zeros_only = stores.iter().all(|store| store.value == Some(0));
            return (ram.sections.iter().copied())
                .filter(|section| {
                    !ready.contains(section) && (!zeros_only || !ram.is_load(*section))
                })
                .collect();
        }
        // The pointer, and where the store goes, which may be past it.
        for at in [store.base, store.address].into_iter().flatten() {
            if let Some(first) = ram.holding(u64::from(at)) {
                for section in ram.run_from(first) {
                    if !ready.contains(&section) && !may.contains(&section) {
                        may.push(section);
                    }
                }
            }
        }
    }
    may
}

/// The stores of the loop `l`, at `loop_event`: its own, not those of a
/// loop inside it, nor of what it calls.
pub(super) fn own_stores<'w>(events: &Events<'w>, loop_event: usize, l: &Loop) -> Vec<&'w Store> {
    let level = events.level(loop_event);
    let inner: Vec<Range<usize>> = (l.events.clone())
        .filter_map(|event| match &events.list[event] {
            Event::Loop(inner) if events.level(event) == level => Some(inner.events.clone()),
            _ => None,
        })
        .collect();
    (l.events.clone())
        .filter(|&event| {
            events.level(event) == level && !inner.iter().any(|range| range.contains(&event))
        })
        .filter_map(|event| match &events.list[event] {
            Event::Store(store) => Some(store),
            _ => None,
        })
        .collect()
}

/// When the code of the loop `l`, at `loop_event`, calls a function that
/// copies, zeroes, or stores into a section, as a loop over a table of
/// sections does, every section not set up yet.
pub(super) fn loop_calls(
    ram: &RamSections,
    events: &Events,
    loop_event: usize,
    l: &Loop,
    ready: &HashSet<usize>,
) -> Vec<usize> {
    let level = events.level(loop_event);
    let stores_into_ram = |event: usize| match &events.list[event] {
        Event::Store(store) => match store.address.or(store.base) {
            Some(at) => ram.holding(u64::from(at)).is_some(),
            None => true,
        },
        _ => false,
    };
    let sets_up = (l.events.clone()).any(|event| {
        events.level(event) == level
            && events.call(event).is_some_and(|call| {
                ram.match_call(&call.passes()).is_some()
                    || (event + 1..events.span(event)).any(stores_into_ram)
            })
    });
    if !sets_up {
        return Vec::new();
    }
    (ram.sections.iter().copied())
        .filter(|section| !ready.contains(section))
        .collect()
}

/// When the code of a loop that elfq can't follow has a loop that copies
/// or zeroes, as a loop over a table of sections does for its first row,
/// the sections of that kind not set up yet, which the loop's next times
/// may set up.
pub(super) fn loop_repeats(
    ram: &RamSections,
    steps: &[Step],
    l: &Loop,
    ready: &HashSet<usize>,
) -> Vec<usize> {
    let mut may: Vec<usize> = Vec::new();
    let inner = (steps.iter()).filter(|step| step.way == Way::Loop && step.event >= l.events.start);
    for step in inner {
        let sections = match step.does {
            Does::Copy => &ram.loads,
            Does::Zero => &ram.zeros,
            _ => continue,
        };
        for &section in &ram.sections {
            if sections.contains(&section) && !ready.contains(&section) && !may.contains(&section) {
                may.push(section);
            }
        }
    }
    may
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{
        alone, binary, closed, each, steps, stored, walked_each, walked_each_inside,
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
            walked_each_inside(&[0..1, 1..2]),
            store(),
            store(),
            walked_each_inside(&[3..4, 4..5]),
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
        let ram = RamSections::new(&binary);
        // A store 4 bytes past a pointer into .ramfunc goes to .bss.
        let mut store = stored(0x2000_0030, None, None);
        store.base = Some(0x2000_002c);
        let names = |stores: &[&Store]| -> Vec<&str> {
            (loop_targets(&ram, stores, &HashSet::new()).into_iter())
                .map(|section| ram.name(section))
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
