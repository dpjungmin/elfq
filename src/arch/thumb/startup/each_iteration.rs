//! Going through each iteration of a loop over a table of sections, with
//! its calls and the loops inside ([`Walker::walk_each_iteration`]), and
//! where each iteration reads its row of the table
//! ([`Iteration::row`](crate::arch::startup::Iteration::row)).

use std::collections::HashMap;

use crate::arch::startup::{Event, How, Iteration, LoopStore, Source, Store};
use crate::arch::thumb::PC;
use crate::arch::thumb::decode::{Effect, Operation, Registers, each};

use super::count::{Mode, first_iteration, line_through};
use super::loops::FoundLoop;
use super::walk_state::Known;
use super::{Frame, Run, WALK_EACH_BUDGET, WALK_EACH_COUNT_BUDGET, Walker};

/// What a loop that the walk went through each iteration of leaves (see
/// [`Walker::walk_each_iteration`]).
pub(super) struct Walked {
    pub(super) known: Known,
    pub(super) exit: u64,
    pub(super) iterations: Vec<Iteration>,
}

impl Walker<'_> {
    /// Goes through the code of the loop that the walk `found` each of the
    /// `runs` times it runs, as [`Walker::count`] counted them, from what
    /// the walk knew the first time it got to the head. Each iteration, it
    /// follows the calls the code makes and counts the loops inside, as
    /// outside loops: for a loop that runs a few times and makes calls or
    /// has loops inside, as one over a table of sections that copies each
    /// with a function or a loop. The events of each iteration take the
    /// place of those that the walk found the first time through the code.
    ///
    /// `None`, and the walk keeps the events of the first iteration, when:
    ///
    /// - an iteration goes past a branch that the walk can't tell;
    /// - the loop goes back or leaves other than it counted;
    /// - it calls through a register a function that the walk can't tell,
    ///   or another than in the first iteration, as a loop over a table of
    ///   functions does, such as the constructors that `__libc_init_array`
    ///   calls, which set up no sections;
    /// - or the iterations go through more instructions than
    ///   [`WALK_EACH_BUDGET`] lets.
    pub(super) fn walk_each_iteration(
        &mut self,
        run: &Run,
        frame: Frame,
        found: &FoundLoop,
        runs: u32,
    ) -> Option<Walked> {
        let FoundLoop { head, before, .. } = *found;
        let open = self.open_loop(run, found, Mode::WalkEach)?;
        let first = self.events.split_off(before.events);
        let lowest = self.lowest;
        // The iterations spend budgets of their own, but in a loop inside
        // one that the walk goes through each iteration of.
        let outermost = !(self.open_loops.iter()).any(|open| open.mode == Mode::WalkEach);
        let walk_budgets = (self.budget, self.count_budget);
        let given = (
            WALK_EACH_BUDGET.min(self.walk_each_budget),
            WALK_EACH_COUNT_BUDGET.min(self.walk_each_count_budget),
        );
        if outermost {
            (self.budget, self.count_budget) = given;
        }
        self.open_loops.push(open);
        let mut known = Known {
            sources: before.sources,
            ..first_iteration(before)
        };
        let mut iterations: Vec<Iteration> = Vec::new();
        let mut loads: Vec<Vec<u32>> = Vec::new();
        // Whether the walk can tell each iteration's row.
        let mut tells = true;
        // What the iterations so far copy from.
        let mut copied: Vec<(u64, u64)> = Vec::new();
        let mut exit = None;
        let mut called = HashMap::new();
        for n in 1..=runs {
            let first_event = self.events.len();
            let Some(ended) = self.walk_iteration(frame, head, known.clone()) else {
                break;
            };
            if n == 1 {
                self.learn_code(&ended);
            }
            if ended.again != (n < runs)
                || !same_calls(&self.events[first_event..], frame.depth, &mut called)
            {
                break;
            }
            // The words of a row of a table that the iteration reads: those
            // it loads, other than what the iterations so far copy. Its row
            // starts at the lowest, where a register that it loads through
            // must point: when such a register points lower, as to a row
            // whose first word the iteration doesn't read, while it reads
            // the next row's start, as a loop that tests the next row at
            // its end does, or none points there, as when registers hold
            // the row's words from the iteration before, the walk can't
            // tell where its row starts.
            copied.extend(self.events[first_event..].iter().flat_map(copied_from));
            let row: Vec<(u32, u32)> = (ended.loads.into_iter())
                .filter(|&(at, _)| {
                    !(copied.iter()).any(|(start, end)| (start..end).contains(&&u64::from(at)))
                })
                .collect();
            let start = row.first().map(|&(at, _)| at);
            tells &= row.iter().map(|&(_, pointer)| pointer).min() == start;
            iterations.push(Iteration {
                events: first_event..self.events.len(),
                row: start,
            });
            loads.push(row.into_iter().map(|(at, _)| at).collect());
            known = ended.known;
            if n == runs {
                exit = Some(ended.exit);
            }
        }
        self.open_loops.pop();
        if outermost {
            let spent = (
                given.0.saturating_sub(self.budget),
                given.1.saturating_sub(self.count_budget),
            );
            self.walk_each_budget = self.walk_each_budget.saturating_sub(spent.0);
            self.walk_each_count_budget = self.walk_each_count_budget.saturating_sub(spent.1);
            (self.budget, self.count_budget) = walk_budgets;
        }
        let Some(exit) = exit else {
            self.events.truncate(before.events);
            self.events.extend(first);
            self.lowest = lowest;
            return None;
        };
        // The rows of a table, one after another.
        let rows: Vec<(u32, u32)> = (iterations.iter().zip(1..))
            .filter_map(|(iteration, n)| Some((n, iteration.row?)))
            .collect();
        let steady = match line_through(rows.iter().map(|&(n, row)| (n, Some(row)))) {
            Some((_, step)) => step != 0 || rows.len() == 1,
            None => false,
        };
        if !tells || !steady || !same_words(&loads) {
            (iterations.iter_mut()).for_each(|iteration| iteration.row = None);
        }
        Some(Walked {
            known,
            exit,
            iterations,
        })
    }

    /// Keeps where the load at `site` that does `effect`, in the iteration
    /// of the loop at `index` in [`Walker::open_loops`] that the walk goes
    /// through, loads from (see
    /// [`OpenLoop::loads`](super::count::OpenLoop::loads)).
    pub(super) fn row_load(&mut self, run: &Run, index: usize, site: u64, effect: &Effect) {
        let Operation::Load { base, .. } = effect.operation else {
            return;
        };
        let Some(at) = self.access(&run.known, site, effect) else {
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
            None if base != PC && !before => self.register(&run.known, base, site),
            _ => None,
        };
        let pointer = pointer.map_or(at, |pointer| pointer.min(at));
        self.open_loops[index].loads.push((site, at, pointer));
    }

    /// Keeps where the words that registers held at the head of the
    /// iteration of the loop at `index` in [`Walker::open_loops`] were
    /// loaded from, of those that an instruction that `reads` and `writes`
    /// these registers reads, as loads of the iteration, unless it is
    /// `conditional`, when the loop's function loaded them, rather than a
    /// function it calls; and forgets those of the registers it writes.
    pub(super) fn carried_reads(
        &mut self,
        index: usize,
        reads: Registers,
        writes: Registers,
        conditional: bool,
    ) {
        let open = &mut self.open_loops[index];
        let read: Vec<Source> = (each(if conditional { 0 } else { reads }))
            .filter_map(|register| open.carried[usize::from(register)].take())
            .collect();
        for register in each(writes) {
            open.carried[usize::from(register)] = None;
        }
        let bounds = open.bounds;
        for Source { site, address } in read {
            if bounds.is_some_and(|(start, end)| (start..end).contains(&site))
                && !self.is_stack(Some(address))
                && self.program.word_at_reset(u64::from(address)).is_some()
            {
                self.open_loops[index].loads.push((site, address, u32::MAX));
            }
        }
    }
}

/// Whether the calls through registers that the code of a loop `depth`
/// calls deep makes in one iteration, whose `events` these are, call
/// functions that the walk can tell, and what they called in the iterations
/// before, as `called` has them by call instruction, which it keeps those
/// of this iteration in.
fn same_calls(events: &[Event], depth: u32, called: &mut HashMap<u64, u64>) -> bool {
    (events.iter()).all(|event| match event {
        Event::Call(call) if call.depth == depth && matches!(call.how, How::Register(_)) => {
            (call.target).is_some_and(|target| *called.entry(call.site).or_insert(target) == target)
        }
        _ => true,
    })
}

/// Whether each iteration of a loop, whose `loads` these are, as
/// `IterationEnd::loads` says, the first iteration's first, reads the same
/// words of the row of a table that the lowest of its loads starts as the
/// first iteration, but the last, which may read fewer, as one that reads
/// only the word that ends the table. An iteration that loads the start of
/// the next row at its end, and not the start of its own, as GCC's code for
/// a loop over a table that ends at a zero word may in the first iteration,
/// doesn't.
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
    others.iter().all(|iteration| iteration == first)
        && last.iter().all(|word| first.contains(word))
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
        Event::Loop(l) => (l.stores().into_iter().flatten())
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
