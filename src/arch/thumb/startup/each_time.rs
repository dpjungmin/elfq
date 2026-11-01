use std::collections::HashMap;

use crate::arch::startup::{Event, How, Loop, LoopStore, Source, Store, Time};
use crate::arch::thumb::PC;
use crate::arch::thumb::decode::{Effect, Operation, Registers};

use super::count::{first_time, line_through};
use super::execute::each;
use super::walk_state::{Path, Visit};
use super::{Frame, Run, TIMES_BUDGET, TIMES_COUNT_BUDGET, Walker};

/// What a loop that the walk went through each time leaves (see
/// [`Walker::walk_times`]).
pub(super) struct Walked {
    pub(super) path: Path,
    pub(super) exit: u64,
    pub(super) times: Vec<Time>,
}

impl Walker<'_> {
    /// Goes through the code of the loop from `head` to `back` each of
    /// the `passes` times it runs, as it counted them, from what it knew
    /// at the head the first time, `before`, following the calls it makes
    /// and counting the loops inside, as outside loops: for a loop that
    /// runs a few times and makes calls or has loops inside, as one over a
    /// table of sections that copies each with a function or a loop. The
    /// events of each time take the place of those that the walk found
    /// the first time. `None` when a time goes past a branch that the walk
    /// can't tell, the loop goes back or leaves other than it counted, it
    /// calls through a register a function that the walk can't tell, or
    /// another than the first time, as a loop over a table of functions
    /// does, such as the constructors that `__libc_init_array` calls,
    /// which set up no sections, or the times go through more instructions
    /// than [`TIMES_BUDGET`] lets; then the walk keeps the events of the
    /// first time.
    pub(super) fn walk_times(
        &mut self,
        run: &Run,
        frame: Frame,
        head: u64,
        back: u64,
        before: &Visit,
        passes: u32,
    ) -> Option<Walked> {
        let counting = self.times_of(run, head, back, before, true)?;
        let first = self.events.split_off(before.events);
        let lowest = self.lowest;
        // The times spend budgets of their own, but in a loop inside one
        // that the walk goes through each time.
        let outermost = !self.counting.iter().any(|counting| counting.walks);
        let walk_budgets = (self.budget, self.count_budget);
        let given = (
            TIMES_BUDGET.min(self.times_budget),
            TIMES_COUNT_BUDGET.min(self.times_count_budget),
        );
        if outermost {
            (self.budget, self.count_budget) = given;
        }
        self.counting.push(counting);
        let mut path = Path {
            sources: before.sources,
            ..first_time(before)
        };
        let mut times: Vec<Time> = Vec::new();
        let mut loads: Vec<Vec<u32>> = Vec::new();
        // Whether the walk can tell each time's row.
        let mut tells = true;
        // What the times so far copy from.
        let mut copied: Vec<(u64, u64)> = Vec::new();
        let mut exit = None;
        let mut called = HashMap::new();
        for n in 1..=passes {
            let first_event = self.events.len();
            let Some(pass) = self.pass(frame, head, path.clone()) else {
                break;
            };
            if n == 1 {
                self.learn_code(&pass);
            }
            if pass.again != (n < passes)
                || !same_calls(&self.events[first_event..], frame.depth, &mut called)
            {
                break;
            }
            // The words of a row of a table that the time reads: those it
            // loads, other than what the times so far copy. Its row starts
            // at the lowest, where a register that it loads through must
            // point: when such a register points lower, as to a row whose
            // first word the time doesn't read, while it reads the next
            // row's start, as a loop that tests the next row at its end
            // does, or none points there, as when registers hold the row's
            // words from the time before, the walk can't tell where its row
            // starts.
            copied.extend(self.events[first_event..].iter().flat_map(copied_from));
            let row: Vec<(u32, u32)> = (pass.loads.into_iter())
                .filter(|&(at, _)| {
                    !(copied.iter()).any(|(start, end)| (start..end).contains(&&u64::from(at)))
                })
                .collect();
            let start = row.first().map(|&(at, _)| at);
            tells &= row.iter().map(|&(_, pointer)| pointer).min() == start;
            times.push(Time {
                first_event,
                row: start,
            });
            loads.push(row.into_iter().map(|(at, _)| at).collect());
            path = pass.path;
            if n == passes {
                exit = Some(pass.exit);
            }
        }
        self.counting.pop();
        if outermost {
            let spent = (
                given.0.saturating_sub(self.budget),
                given.1.saturating_sub(self.count_budget),
            );
            self.times_budget = self.times_budget.saturating_sub(spent.0);
            self.times_count_budget = self.times_count_budget.saturating_sub(spent.1);
            (self.budget, self.count_budget) = walk_budgets;
        }
        let Some(exit) = exit else {
            self.events.truncate(before.events);
            self.events.extend(first);
            self.lowest = lowest;
            return None;
        };
        // The rows of a table, one after another.
        let rows: Vec<(u32, u32)> = (times.iter().zip(1..))
            .filter_map(|(time, n)| Some((n, time.row?)))
            .collect();
        let steady = match line_through(rows.iter().map(|&(n, row)| (n, Some(row)))) {
            Some((_, step)) => step != 0 || rows.len() == 1,
            None => false,
        };
        if !tells || !steady || !same_words(&loads) {
            times.iter_mut().for_each(|time| time.row = None);
        }
        Some(Walked { path, exit, times })
    }

    /// Keeps where the load at `site` that does `effect`, in the time
    /// through the code of the loop at `index` in [`Walker::counting`]
    /// that the walk goes through, loads from (see [`Counting::loads`](super::count::Counting::loads)).
    pub(super) fn row_load(&mut self, run: &Run, index: usize, site: u64, effect: &Effect) {
        let Operation::Load { base, .. } = effect.operation else {
            return;
        };
        let Some(at) = self.access(&run.path, site, effect) else {
            return;
        };
        if self.is_stack(Some(at)) || self.program.word_at_reset(u64::from(at)).is_none() {
            return;
        }
        // Where the register points that the load adds an offset to, as
        // to the start of a row of a table, but for `[base, #imm]!`, which
        // moves it there first.
        let before = effect.writeback.is_some_and(|writeback| writeback.before);
        let pointer = match effect.index {
            None if base != PC && !before => self.register(&run.path, base, site),
            _ => None,
        };
        let pointer = pointer.map_or(at, |pointer| pointer.min(at));
        self.counting[index].loads.push((site, at, pointer));
    }

    /// Keeps where the words that registers held at the head of the time
    /// through the code of the loop at `index` in [`Walker::counting`]
    /// were loaded from, of those that an instruction that `reads` and
    /// `writes` these registers reads, as loads of the time, unless it is
    /// `conditional`, when the loop's function loaded them, rather than a
    /// function it calls; and forgets those of the registers it writes.
    pub(super) fn carried_reads(
        &mut self,
        index: usize,
        reads: Registers,
        writes: Registers,
        conditional: bool,
    ) {
        let counting = &mut self.counting[index];
        let read: Vec<Source> = (each(if conditional { 0 } else { reads }))
            .filter_map(|register| counting.carried[usize::from(register)].take())
            .collect();
        for register in each(writes) {
            counting.carried[usize::from(register)] = None;
        }
        let bounds = counting.bounds;
        for Source { site, address } in read {
            if bounds.is_some_and(|(start, end)| (start..end).contains(&site))
                && !self.is_stack(Some(address))
                && self.program.word_at_reset(u64::from(address)).is_some()
            {
                self.counting[index].loads.push((site, address, u32::MAX));
            }
        }
    }
}

/// Whether the calls through registers that the code of a loop `depth`
/// calls deep makes in one time through it, whose `events` these are,
/// call functions that the walk can tell, and what they called in the
/// times before, as `called` has them by call instruction, which it keeps
/// those of this time in.
fn same_calls(events: &[Event], depth: u32, called: &mut HashMap<u64, u64>) -> bool {
    (events.iter()).all(|event| match event {
        Event::Call(call) if call.depth == depth && matches!(call.how, How::Register(_)) => {
            (call.target).is_some_and(|target| *called.entry(call.site).or_insert(target) == target)
        }
        _ => true,
    })
}

/// Whether each time through a loop's code, whose `loads` these are, as
/// `Pass::loads` says, the first time's first, reads the same words of
/// the row of a table that the lowest of its loads starts as the first
/// time, but the last, which may read fewer, as one that reads only the
/// word that ends the table. A time that loads the start of the next row
/// at its end, and not the start of its own, as GCC's code for a loop over
/// a table that ends at a zero word may the first time, doesn't.
pub(super) fn same_words(loads: &[Vec<u32>]) -> bool {
    let words = |loads: &Vec<u32>| -> Vec<u32> {
        let first = loads.first().copied().unwrap_or(0);
        loads.iter().map(|&at| at - first).collect()
    };
    let loads: Vec<Vec<u32>> = (loads.iter())
        .filter(|loads| !loads.is_empty())
        .map(words)
        .collect();
    let Some((last, others)) = loads.split_last() else {
        return true;
    };
    let first = others.first().unwrap_or(last);
    others.iter().all(|time| time == first) && last.iter().all(|word| first.contains(word))
}

/// Whether the walk can tell what each of a loop's `stores` that stores
/// a word the loop loaded copies from.
pub(super) fn knows_sources(stores: Option<&[LoopStore]>) -> bool {
    stores
        .is_some_and(|stores| (stores.iter()).all(|store| store.source.is_some() || !store.loaded))
}

/// The bytes that `event` copies from, from the first to past the last,
/// as far as the walk can tell.
fn copied_from(event: &Event) -> Vec<(u64, u64)> {
    match event {
        Event::Store(Store {
            source: Some(source),
            size,
            ..
        }) => {
            let start = u64::from(source.address);
            vec![(start, start + u64::from(*size))]
        }
        Event::Loop(Loop {
            stores: Some(stores),
            ..
        }) => (stores.iter())
            .filter_map(|store| {
                let (address, step) = store.source?;
                let first = i64::from(address);
                let last = first + i64::from(step) * i64::from(store.times.saturating_sub(1));
                let start = u64::try_from(first.min(last)).ok()?;
                let end = u64::try_from(first.max(last)).ok()? + u64::from(store.size);
                Some((start, end))
            })
            .collect(),
        _ => Vec::new(),
    }
}
