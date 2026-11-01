//! Loops: walking a loop's code once ([`Walker::loop_body`]), also a body
//! that the compiler put after the function's other code
//! ([`Walker::out_of_line_body`]), whether its branch back goes back
//! ([`Walker::goes_back`]), and what the walk knows after the loop
//! ([`Walker::close_loop`]), with `count.rs` and `each_time.rs`. Also
//! where a call goes to a loop's code that a function finder took for a
//! function ([`Walker::went_through`] and [`Walker::runs_into`]).

use std::rc::Rc;

use crate::arch::startup::{End, Event, Loop, Runs};
use crate::arch::thumb::decode::{Flow, Operation, When};
use crate::arch::thumb::{SP, is_wide};

use super::calls::Changes;
use super::count::{Counted, forget_stored};
use super::each_time::knows_sources;
use super::execute::holds;
use super::walk_state::{
    Flags, Path, Visit, forget_changes, forget_words, set_unknown, stored_since,
};
use super::{Frame, LOOP_LIMIT, MOST_TIMES, NESTING, RUNS_INTO, Run, Walker};

impl Walker<'_> {
    /// Whether the walk went through code of the function that holds
    /// `target`, as a loop's code that a function finder took for another
    /// function.
    pub(super) fn went_through(&self, run: &Run, target: u64) -> bool {
        let Some((start, end)) = self.program.function_at(target) else {
            return false;
        };
        run.order
            .iter()
            .any(|address| (start..end).contains(address))
    }

    /// Whether the code from `target` runs on into the function the walk
    /// is in, as a loop's code that comes just before the function does
    /// when a function finder takes the loop's test, which a call goes to,
    /// for the function's start.
    pub(super) fn runs_into(&mut self, run: &Run, target: u64) -> bool {
        let Some((start, _)) = run.bounds.filter(|&(start, _)| target < start) else {
            return false;
        };
        let bounds = self.program.function_at(target);
        let mut address = target;
        while address < start && start - address <= RUNS_INTO {
            let Some((first, _, effect)) = self.decode(address) else {
                return false;
            };
            if !matches!(effect.flow, Flow::Next) || self.is_data(bounds, address) {
                return false;
            }
            address += if is_wide(first) { 4 } else { 2 };
        }
        address == start
    }

    /// Walks a loop's code once, from `head` to the branch at `back`, with
    /// what the walk knows now, and goes on with what it knows after.
    /// Returns where the walk of the code stopped before `back`, if it did.
    pub(super) fn loop_body(
        &mut self,
        run: &mut Run,
        frame: Frame,
        head: u64,
        back: u64,
    ) -> Option<End> {
        if frame.nesting >= NESTING {
            return Some(End::Budget(head));
        }
        let body = Frame {
            depth: frame.depth,
            follow: frame.follow,
            limit: LOOP_LIMIT,
            stop: Some(back),
            nesting: frame.nesting + 1,
            pass: None,
            rows: frame.rows,
        };
        let (end, path) = self.walk(head, body, run.path.clone());
        run.path = path;
        end
    }

    /// Records the loop from `head` to `back`, whose code the walk went
    /// through from `before`, and goes on with what it knows where the
    /// loop leaves, when the walk can count how many times it runs, which
    /// it tries when its code `runs` at least once: then it returns where
    /// that is. A loop that runs at most [`MOST_TIMES`] times and makes
    /// calls or has loops inside, whose stores the walk can't tell from
    /// three times through its code, it goes through each time (see
    /// [`Walker::walk_times`]). Otherwise it goes on with what the
    /// code writes unknown, and when the walk of the code stopped short, at
    /// `end`, the words in memory too.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn close_loop(
        &mut self,
        run: &mut Run,
        frame: Frame,
        head: u64,
        back: u64,
        before: &Visit,
        end: Option<End>,
        runs: bool,
    ) -> Option<u64> {
        let counted = match (end, runs) {
            (None, true) => self.count(run, frame, head, back, before),
            _ => None,
        };
        if let Some(counted) = &counted
            && counted.others
            && counted.passes <= MOST_TIMES
            && let Some(walked) = self.walk_times(run, frame, head, back, before, counted.passes)
        {
            run.path = Path {
                forks: before.forks,
                ..walked.path
            };
            self.events.push(Event::Loop(Loop {
                head,
                back,
                depth: frame.depth,
                events: before.events..self.events.len(),
                runs: Runs::EachIteration(walked.iterations),
            }));
            return Some(walked.exit);
        }
        let exit = counted.as_ref().map(|counted| counted.exit);
        let runs = match counted {
            Some(Counted {
                passes,
                path,
                stores,
                every,
                ..
            }) => {
                let first = std::mem::replace(
                    &mut run.path,
                    Path {
                        forks: before.forks,
                        ..path
                    },
                );
                // What code that the times through the loop don't follow,
                // as a call, stored the first time.
                if stores.is_none() {
                    forget_words(&mut run.path, stored_since(&first.memory, &before.memory));
                }
                if !every {
                    forget_stored(&mut run.path, passes, stores.as_deref(), before);
                }
                Runs::Counted {
                    iterations: passes,
                    stores,
                }
            }
            None => {
                forget_changes(&mut run.path, &before.registers, &before.memory);
                let Changes {
                    registers,
                    stores,
                    results,
                } = self.writes(head, back, false);
                set_unknown(&mut run.path, registers & !(1 << SP));
                run.path.results |= results;
                if end.is_some() && stores && run.path.memory.values().any(Option::is_some) {
                    (Rc::make_mut(&mut run.path.memory).values_mut()).for_each(|word| *word = None);
                }
                run.path.flags = Flags::default();
                Runs::Uncounted { stopped: end }
            }
        };
        // The loads of a loop inside one that the walk goes through each
        // time may read what it copies, which can't be told from the rows
        // of a table when the walk can't tell what its stores copy from.
        let stores = match &runs {
            Runs::Counted { stores, .. } => stores.as_deref(),
            _ => None,
        };
        if let Some(index) = frame.rows
            && !knows_sources(stores)
        {
            (self.counting[index].loads).retain(|&(site, ..)| !(head..=back).contains(&site));
        }
        self.events.push(Event::Loop(Loop {
            head,
            back,
            depth: frame.depth,
            events: before.events..self.events.len(),
            runs,
        }));
        exit
    }

    /// Whether the branch back at `back` goes back to the loop's head, as
    /// the walk is there at the end of `run`, when it can tell.
    pub(super) fn goes_back(&self, run: &Run, back: u64) -> Option<bool> {
        let (_, _, effect) = self.decode(back)?;
        let Flow::Branch { when, .. } = effect.flow else {
            return None;
        };
        // In an `it` block, as `blt` at the end of `ittt lt` is.
        let runs = match run.it & 0xf {
            0 => Some(true),
            _ => holds(u16::from(run.it >> 4), run.path.flags),
        };
        let taken = match when {
            When::Always => Some(true),
            When::Flags(condition) => holds(condition, run.path.flags),
            When::Zero(register) => self.register(&run.path, register, back).map(|n| n == 0),
            When::NotZero(register) => self.register(&run.path, register, back).map(|n| n != 0),
        };
        match (runs, taken) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        }
    }

    /// Where a loop body that the compiler put after the function's other
    /// code ends: the block at `start`, which a branch at `site` goes to,
    /// ends with a branch back to the loop's test, which leads to `site`,
    /// in the function.
    pub(super) fn out_of_line_body(
        &self,
        start: u64,
        site: u64,
        bounds: Option<(u64, u64)>,
    ) -> Option<u64> {
        let lowest = bounds.map_or(site.saturating_sub(0x400), |(start, _)| start);
        let mut address = start;
        for _ in 0..16 {
            if address >= start + 64 {
                return None;
            }
            let (first, _, effect) = self.decode(address)?;
            match effect.flow {
                Flow::Branch {
                    target,
                    when: When::Always,
                } => {
                    let back = address > start
                        && (lowest..=site).contains(&target)
                        && self.is_test(target, site, start);
                    return back.then_some(address);
                }
                Flow::Return
                | Flow::Pop
                | Flow::JumpRegister(..)
                | Flow::JumpRelative(_)
                | Flow::JumpMemory
                | Flow::Table
                | Flow::Stop => return None,
                _ if matches!(effect.operation, Operation::Pop { .. }) => return None,
                _ => {}
            }
            address += if is_wide(first) { 4 } else { 2 };
        }
        None
    }

    /// Whether the code from `test` up to the branch at `site` is a loop's
    /// test: no calls, jumps or returns, and only branches out of the loop,
    /// forward to before its code at `body`. That rules out an `if`'s join
    /// or an outer loop's test.
    fn is_test(&self, test: u64, site: u64, body: u64) -> bool {
        let mut address = test;
        for _ in 0..16 {
            if address >= site {
                return address == site;
            }
            let Some((first, _, effect)) = self.decode(address) else {
                return false;
            };
            let goes_on = match effect.flow {
                Flow::Next | Flow::It { .. } => true,
                Flow::Branch { target, when } => {
                    when != When::Always && site < target && target <= body
                }
                _ => false,
            };
            if !goes_on {
                return false;
            }
            address += if is_wide(first) { 4 } else { 2 };
        }
        false
    }
}
