//! Loops: walking a loop's code once ([`Walker::loop_body`]), also a body
//! that the compiler put after the function's other code
//! ([`Walker::out_of_line_body`]), whether its branch back goes back
//! ([`Walker::goes_back`]), and what the walk knows after the loop
//! ([`Walker::close_loop`], for a [`FoundLoop`]), with `count.rs` and
//! `each_iteration.rs`. Also where a call goes to a loop's code that a
//! function finder took for a function ([`Walker::went_through`] and
//! [`Walker::runs_into`]).

use crate::arch::startup::{End, Event, FunctionBounds, Loop, LoopStore, Runs};
use crate::arch::thumb::SP;
use crate::arch::thumb::decode::{Flow, Operation, When};

use super::calls::Changes;
use super::count::Counted;
use super::each_iteration::Walked;
use super::each_iteration::knows_sources;
use super::execute::holds;
use super::walk_state::{Known, Visit, stored_since};
use super::{
    Branch, Finish, Frame, MOST_WALKED_ITERATIONS, NESTING, RUNS_INTO, Run, Step, Walker,
    decode_at, straight_line,
};

impl Walker<'_> {
    /// Whether the walk went through code of the function that holds
    /// `target`, as a loop's code that a function finder took for another
    /// function.
    pub(super) fn went_through(&self, run: &Run, target: u64) -> bool {
        let Some(function) = self.program.function_at(target) else {
            return false;
        };
        run.order.iter().any(|&address| function.contains(address))
    }

    /// Whether the code from `target` runs on into the function the walk
    /// is in, as a loop's code that comes just before the function does
    /// when a function finder takes the loop's test, which a call goes to,
    /// for the function's start.
    pub(super) fn runs_into(&mut self, run: &Run, target: u64) -> bool {
        let Some(FunctionBounds { start, .. }) = run.bounds.filter(|bounds| target < bounds.start)
        else {
            return false;
        };
        let bounds = self.program.function_at(target);
        let mut address = target;
        while address < start && start - address <= RUNS_INTO {
            let Some(instruction) = decode_at(self.program, address) else {
                return false;
            };
            if !matches!(instruction.effect.flow, Flow::Next) || self.is_data(bounds, address) {
                return false;
            }
            address = instruction.next();
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
        let (finish, known) = self.walk(head, frame.loop_body(back), run.known.clone());
        run.known = known;
        match finish {
            Finish::Ended(end) => Some(end),
            Finish::AtStop => None,
        }
    }

    /// Records the loop that the walk `found`, and goes on with what it
    /// knows where the loop leaves. When the loop's code surely runs and
    /// the walk of it didn't stop short, the walk tries to count how many
    /// times the loop runs, and when it can, returns where it leaves. A
    /// loop that runs at most [`MOST_WALKED_ITERATIONS`] times and makes
    /// calls or has loops inside, whose stores the walk can't tell from
    /// three iterations, it goes through each iteration of (see
    /// [`Walker::walk_each_iteration`]).
    pub(super) fn close_loop(
        &mut self,
        run: &mut Run,
        frame: Frame,
        found: FoundLoop,
    ) -> Option<u64> {
        let counted = match (found.stopped, found.known_to_run) {
            (None, true) => self.count(run, frame, &found),
            _ => None,
        };
        let (runs, exit) = match counted {
            Some(counted) if counted.others && counted.iterations <= MOST_WALKED_ITERATIONS => {
                match self.walk_each_iteration(run, frame, &found, counted.iterations) {
                    Some(walked) => leave_walked(run, &found, walked),
                    None => self.leave_counted(run, frame, &found, counted),
                }
            }
            Some(counted) => self.leave_counted(run, frame, &found, counted),
            None => self.leave_uncounted(run, frame, &found),
        };
        self.events.push(Event::Loop(Loop {
            head: found.head,
            back: found.back,
            depth: frame.depth,
            events: found.before.events..self.events.len(),
            runs,
        }));
        exit
    }

    /// Goes on from a loop that the walk `counted`, with what it leaves.
    fn leave_counted(
        &mut self,
        run: &mut Run,
        frame: Frame,
        found: &FoundLoop,
        counted: Counted,
    ) -> (Runs, Option<u64>) {
        let Counted {
            iterations,
            known,
            exit,
            stores,
            every,
            ..
        } = counted;
        let before = found.before;
        let first = std::mem::replace(
            &mut run.known,
            Known {
                forks: before.known.forks,
                ..known
            },
        );
        // What code that the iterations don't follow, as a call, stored the
        // first time through the code.
        if stores.is_none() {
            let stored = stored_since(&first.memory, &before.known.memory);
            run.known.forget_words(stored);
        }
        if !every {
            run.known.forget_stored(stores.as_deref(), before);
        }
        self.forget_row_loads(frame, found, stores.as_deref());
        let runs = Runs::Counted { iterations, stores };
        (runs, Some(exit))
    }

    /// Goes on from a loop that the walk couldn't count, with what its
    /// code writes unknown, and when the walk of the code stopped short,
    /// the words in memory too.
    fn leave_uncounted(
        &mut self,
        run: &mut Run,
        frame: Frame,
        found: &FoundLoop,
    ) -> (Runs, Option<u64>) {
        let before = &found.before.known;
        (run.known).forget_changes_since(&before.registers, &before.memory);
        let Changes {
            registers,
            stores,
            results,
        } = self.loop_writes(found.head, found.back);
        run.known.forget_registers(registers & !(1 << SP));
        run.known.results |= results;
        if found.stopped.is_some() && stores {
            run.known.forget_all_words();
        }
        run.known.forget_flags();
        self.forget_row_loads(frame, found, None);
        let runs = Runs::Uncounted {
            stopped: found.stopped,
        };
        (runs, None)
    }

    /// Drops the loads of the code of the loop that the walk `found` from
    /// the words that an iteration of an outer loop reads as a row of a
    /// table (see [`OpenLoop::loads`](super::count::OpenLoop::loads)), when
    /// the walk goes through each of that loop's iterations, unless it can
    /// tell what the loop's `stores` copy from: else those loads may read
    /// what the loop copies.
    fn forget_row_loads(&mut self, frame: Frame, found: &FoundLoop, stores: Option<&[LoopStore]>) {
        if let Some(index) = frame.rows
            && !knows_sources(stores)
        {
            let code = found.head..=found.back;
            (self.open_loops[index].loads).retain(|load| !code.contains(&load.site));
        }
    }

    /// A branch back, as a loop's: the walk goes through the loop's code
    /// once (see [`Walker::loop_body`]), unless it went through it on its
    /// way to the branch. When the code was jumped over to the loop's test
    /// at its end, the walk goes through it from the branch's target, with
    /// what it knows at the test.
    pub(super) fn branch_back(&mut self, run: &mut Run, frame: Frame, branch: Branch) -> Step {
        let Branch {
            site,
            target,
            next,
            taken,
        } = branch;
        let (before, stopped, known_to_run) = match run.visits.get(&target) {
            Some(visit) => (visit.clone(), None, true),
            // The code was jumped over to a test that doesn't go back:
            // the loop runs no times, as for an empty section.
            _ if taken == Some(false) => return Step::Next(next),
            _ => {
                let before = run.visit(self.events.len());
                match self.loop_body(run, frame, target, site) {
                    Some(End::Main(at)) => return Step::Final(End::Main(at)),
                    stopped => (before, stopped, taken == Some(true)),
                }
            }
        };
        let found = FoundLoop {
            head: target,
            back: site,
            before: &before,
            stopped,
            known_to_run,
        };
        let exit = self.close_loop(run, frame, found);
        Step::Next(exit.unwrap_or(next))
    }

    /// When `branch`, whose condition the walk can't tell, goes to a loop's
    /// code that the compiler put after the function's other code, as GCC
    /// -Os does, which branches back to the loop's test, which leads to
    /// `branch`: walks the loop there and then, and goes on after the
    /// branch.
    pub(super) fn out_of_line_loop(
        &mut self,
        run: &mut Run,
        frame: Frame,
        branch: Branch,
    ) -> Option<Step> {
        let head = branch.target;
        let back = self.out_of_line_body(head, branch.site, run.bounds)?;
        let before = run.visit(self.events.len());
        let stopped = self.loop_body(run, frame, head, back);
        if let Some(End::Main(at)) = stopped {
            return Some(Step::Final(End::Main(at)));
        }
        let found = FoundLoop {
            head,
            back,
            before: &before,
            stopped,
            known_to_run: false,
        };
        self.close_loop(run, frame, found);
        Some(Step::Next(branch.next))
    }

    /// Whether the branch back at `back` goes back to the loop's head, as
    /// the walk is there at the end of `run`, when it can tell.
    pub(super) fn goes_back(&self, run: &Run, back: u64) -> Option<bool> {
        let Flow::Branch { when, .. } = decode_at(self.program, back)?.effect.flow else {
            return None;
        };
        // In an `it` block, as `blt` at the end of `ittt lt` is.
        let runs = match run.condition() {
            None => Some(true),
            Some(condition) => holds(condition, run.known.flags),
        };
        match (runs, self.taken(&run.known, when, back)) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        }
    }

    /// Where a loop body that the compiler put after the function's other
    /// code ends: the block at `start`, which a branch at `site` goes to,
    /// ends with a branch back to the loop's test, which leads to `site`,
    /// in the function.
    fn out_of_line_body(
        &self,
        start: u64,
        site: u64,
        bounds: Option<FunctionBounds>,
    ) -> Option<u64> {
        let lowest = bounds.map_or(site.saturating_sub(0x400), |bounds| bounds.start);
        for instruction in straight_line(self.program, start).take(16) {
            let address = instruction.address;
            if address >= start + 64 {
                return None;
            }
            let effect = instruction.effect;
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
            let Some(instruction) = decode_at(self.program, address) else {
                return false;
            };
            let goes_on = match instruction.effect.flow {
                Flow::Next | Flow::It { .. } => true,
                Flow::Branch { target, when } => {
                    when != When::Always && site < target && target <= body
                }
                _ => false,
            };
            if !goes_on {
                return false;
            }
            address = instruction.next();
        }
        false
    }
}

/// A loop that the walk found and went through the code of once, before it
/// works out what it knows after the loop (see [`Walker::close_loop`]).
#[derive(Clone, Copy)]
pub(super) struct FoundLoop<'v> {
    /// Where each iteration starts.
    pub(super) head: u64,
    /// The branch back.
    pub(super) back: u64,
    /// What the walk knew the first time it got to the head.
    pub(super) before: &'v Visit,
    /// Where the walk of the loop's code stopped before `back`, if it did.
    pub(super) stopped: Option<End>,
    /// Whether the loop's code surely runs at least once, as it doesn't
    /// when the walk can't tell whether a branch to the loop's test at its
    /// end goes back, nor for a body that the compiler put after the
    /// function's other code. Only then does the walk count how many times
    /// the loop runs.
    pub(super) known_to_run: bool,
}

/// Goes on from a loop that the walk went through each iteration of, with
/// what the last one leaves.
fn leave_walked(run: &mut Run, found: &FoundLoop, walked: Walked) -> (Runs, Option<u64>) {
    run.known = Known {
        forks: found.before.known.forks,
        ..walked.known
    };
    (Runs::EachIteration(walked.iterations), Some(walked.exit))
}
