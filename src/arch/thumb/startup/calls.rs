//! Calls and tail calls ([`Walker::call`] and [`Walker::tail_call`]): the
//! walk follows one into the function it calls, and works out what a
//! call that it doesn't follow to a return may change
//! ([`Walker::clobbers`]).

use crate::arch::startup::{Call, End, Event, How, Through};
use crate::arch::thumb::decode::{Effect, Flow, Operation, Registers};
use crate::arch::thumb::{ARGUMENTS, CALL_CLOBBERS, LR, SP};

use super::walk_state::{Alternative, Flags, Known, forget_changes, set_unknown};
use super::{Frame, Run, Step, Walker, decode_at};

/// What code may change, as far as the walk can tell without going
/// through it (see [`Walker::loop_writes`]).
pub(super) struct Changes {
    /// The registers its instructions write.
    pub(super) registers: Registers,
    /// Whether it stores, or calls a function that may.
    pub(super) stores: bool,
    /// Of r0 to r3, those that may hold what it leaves (see
    /// `Known::results`).
    pub(super) results: u8,
}

impl Walker<'_> {
    /// What the code of the loop from `head` to its branch back at `back`
    /// may change, as far as the walk can tell without going through it.
    pub(super) fn loop_writes(&mut self, head: u64, back: u64) -> Changes {
        self.writes(head, back, false)
    }

    /// What the code of the function from `start` to `end` may change, as
    /// far as the walk can tell without going through it, past the data in
    /// it, such as a literal pool. A jump out of it goes to another
    /// function, which may change r0 to r3, ip and lr.
    fn function_writes(&mut self, start: u64, end: u64) -> Changes {
        self.writes(start, end, true)
    }

    /// What the code from `start` up to `end` may change, as
    /// [`Walker::loop_writes`] and, with `function`, as
    /// [`Walker::function_writes`] say.
    fn writes(&mut self, start: u64, end: u64, function: bool) -> Changes {
        let mut changes = Changes {
            registers: 0,
            stores: false,
            results: 0,
        };
        let mut address = start;
        while address < end {
            if function && self.is_data(Some((start, end)), address) {
                address += 2;
                continue;
            }
            let Some(instruction) = decode_at(self.program, address) else {
                return Changes {
                    registers: Registers::MAX,
                    stores: true,
                    results: ARGUMENTS as u8,
                };
            };
            let effect = instruction.effect;
            changes.registers |= effect.writes;
            changes.results |= results(&effect);
            let leaves = match effect.flow {
                Flow::Call | Flow::CallRegister(..) => true,
                Flow::Branch { target, .. } => function && !(start..end).contains(&target),
                Flow::JumpRegister(..) | Flow::JumpRelative(_) | Flow::JumpMemory => function,
                _ => false,
            };
            if leaves {
                changes.registers |= CALL_CLOBBERS;
                changes.results = ARGUMENTS as u8;
                changes.stores = true;
            }
            changes.stores |= matches!(
                effect.operation,
                Operation::Store { .. } | Operation::Push { .. }
            );
            address = instruction.next();
        }
        changes
    }

    /// What a call to `target` may change, when the walk doesn't follow it
    /// to a return: of r0 to r3, ip and lr, the registers that the
    /// function's code writes, as code built with GCC's `-fipa-ra` counts
    /// on, or all of them; and of r0 to r3, those that may hold what it
    /// leaves (see `Known::results`).
    fn clobbers(&mut self, target: Option<u64>) -> (Registers, u8) {
        let function = target.and_then(|target| self.program.function_at(target));
        let Some((start, end)) = function.filter(|&(start, _)| Some(start) == target) else {
            return (CALL_CLOBBERS, ARGUMENTS as u8);
        };
        if let Some(&clobbers) = self.clobbers.get(&start) {
            return clobbers;
        }
        let changes = self.function_writes(start, end);
        let clobbers = (
            (changes.registers | 1 << LR) & CALL_CLOBBERS,
            changes.results,
        );
        self.clobbers.insert(start, clobbers);
        clobbers
    }

    /// A branch or jump at `site` to `to`, through what `through` says,
    /// which the walk can tell or not.
    pub(super) fn jump_to(
        &mut self,
        run: &mut Run,
        frame: Frame,
        site: u64,
        to: Option<u64>,
        through: Through,
    ) -> Step {
        match to {
            None => Step::MayBacktrack(End::Branches { site, through }),
            Some(target) if self.program.code(target).is_none() => {
                Step::MayBacktrack(End::LeavesFile { site, target })
            }
            Some(target) if run.leaves(target) => self.tail_call(run, frame, site, target),
            Some(target) => Step::Next(target),
        }
    }

    /// A jump at `site` to `target` in another function, which then
    /// returns to the caller: the walk goes on there, at the same depth.
    pub(super) fn tail_call(
        &mut self,
        run: &mut Run,
        frame: Frame,
        site: u64,
        target: u64,
    ) -> Step {
        self.events.push(Event::Call(Call {
            site,
            depth: frame.depth,
            target: Some(target),
            how: How::Jump,
            arguments: arguments(&run.known),
            left: run.known.left,
            conditional: false,
            followed: frame.follow > 0,
        }));
        if self.main == Some(target) {
            return Step::Final(End::Main(site));
        }
        if frame.follow == 0 {
            // In the code that a walk following no calls starts from, a
            // jump to code that ends where the walk can't read on is a dead
            // end, as core 1's way past the Pico SDK's check of which core
            // runs is: the walk goes back to a branch it couldn't tell.
            if frame.depth == 0
                && frame.stop.is_none()
                && let Some(end) = self.dead_end(run, frame, site, target)
            {
                return Step::MayBacktrack(end);
            }
            // It returns with what the function it jumps to leaves in the
            // registers that pass arguments and results.
            let (clobbers, results) = self.clobbers(Some(target));
            set_unknown(&mut run.known, clobbers);
            run.known.results |= results;
            return Step::MayBacktrack(End::Returns(site));
        }
        if self.program.code(target).is_none() {
            return Step::MayBacktrack(End::LeavesFile { site, target });
        }
        run.bounds = self.program.function_at(target);
        Step::Next(target)
    }

    /// How the code that the jump at `site` to `target`, in the code of
    /// `frame`, goes to ends, when it ends where the walk can't read on and
    /// the walk has a branch it couldn't tell to go back to: walked as a
    /// function that the code calls, whose events are then dropped.
    fn dead_end(&mut self, run: &Run, frame: Frame, site: u64, target: u64) -> Option<End> {
        let untried = |alternative: &Alternative| !run.visits.contains_key(&alternative.target);
        if !run.alternatives.iter().any(untried) {
            return None;
        }
        if self.program.code(target).is_none() {
            return Some(End::LeavesFile { site, target });
        }
        let mark = self.mark();
        let ((end, _), spent) = self.with_budget(self.budget, |walker| {
            walker.walk(target, frame.callee(), run.known.clone())
        });
        // The instructions it goes through don't count for the walk, but
        // what it spends to count loops does.
        self.budget.count -= spent.count;
        self.rewind(mark);
        end.filter(|end| end.is_dead())
    }

    /// Records a call at `site`, which runs only when the flags say so
    /// when `conditional`, and walks the function it calls. Where that
    /// returns, the caller goes on with what it knew there, but for sp and
    /// lr; when the function went past a branch the walk couldn't tell,
    /// with what it changed unknown. Without a return, r0 to r3, ip, lr
    /// and the words it stored are unknown. Returns how the walk ends,
    /// when it gets to `main`.
    pub(super) fn call(
        &mut self,
        run: &mut Run,
        frame: Frame,
        site: u64,
        target: Option<u64>,
        how: How,
        conditional: bool,
    ) -> Option<End> {
        let follows = target.is_some() && frame.follow > 0 && !conditional;
        self.events.push(Event::Call(Call {
            site,
            depth: frame.depth,
            target,
            how,
            arguments: arguments(&run.known),
            left: run.known.left,
            conditional,
            followed: follows,
        }));
        if target.is_some() && self.main == target {
            return Some(End::Main(site));
        }
        let mut after = None;
        if let Some(target) = target.filter(|_| follows) {
            let depth = frame.depth + 1;
            if self.program.code(target).is_none() {
                let end = End::LeavesFile { site, target };
                self.events.push(Event::Stop { site, depth, end });
            } else {
                // The call puts the address to return to in lr, which the
                // walk doesn't follow.
                let mut known = run.known.clone();
                known.registers[usize::from(LR)] = None;
                known.results = 0;
                let (end, known) = self.walk(target, frame.callee(), known);
                match end {
                    Some(End::Main(at)) => return Some(End::Main(at)),
                    Some(End::Returns(_)) => after = Some((known, true)),
                    Some(end) => {
                        if end.stops_short() {
                            self.events.push(Event::Stop { site, depth, end });
                        }
                        after = Some((known, false));
                    }
                    None => {}
                }
            }
        }
        let sp = run.known.registers[usize::from(SP)];
        let results = run.known.results;
        // Of r0 to r3, those that may hold what the call leaves.
        let called = match after {
            Some((mut known, true)) => {
                if known.forks {
                    forget_changes(&mut known, &run.known.registers, &run.known.memory);
                    known.flags = Flags::default();
                }
                let called = known.results;
                known.forks = run.known.forks;
                run.known = known;
                run.known.registers[usize::from(SP)] = sp;
                run.known.registers[usize::from(LR)] = None;
                called
            }
            Some((mut known, false)) => {
                forget_changes(&mut known, &run.known.registers, &run.known.memory);
                run.known.memory = known.memory;
                let (clobbers, called) = self.clobbers(target);
                set_unknown(&mut run.known, clobbers);
                run.known.flags = Flags::default();
                called
            }
            None => {
                let (clobbers, called) = self.clobbers(target);
                set_unknown(&mut run.known, clobbers);
                run.known.flags = Flags::default();
                called
            }
        };
        run.known.results = results | called;
        run.known.left |= called;
        None
    }
}

/// What r0 to r3 hold.
pub(super) fn arguments(known: &Known) -> [Option<u32>; 4] {
    [0, 1, 2, 3].map(|register| known.registers[register])
}

/// Of r0 to r3, as bits, those that may hold what a call leaves after an
/// instruction that does what `effect` says (see `Known::results`): those
/// it writes, and r0 when it reads it, other than to push it.
pub(super) fn results(effect: &Effect) -> u8 {
    let reads_r0 = effect.reads & 1 != 0 && !matches!(effect.operation, Operation::Push { .. });
    (effect.writes & ARGUMENTS) as u8 | u8::from(reads_r0)
}
