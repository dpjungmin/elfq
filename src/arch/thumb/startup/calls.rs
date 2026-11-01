//! Calls and tail calls ([`Walker::call`] and [`Walker::tail_call`]): the
//! walk follows one into the function it calls, and works out what a
//! call that it doesn't follow to a return may change
//! ([`Walker::clobbers`]).

use crate::arch::startup::{Call, End, Event, How, Through};
use crate::arch::thumb::decode::{Effect, Flow, LowBit, Operation, Registers};
use crate::arch::thumb::{LR, SP, is_wide};

use super::walk_state::{Alternative, Flags, Path, forget_changes, set_unknown};
use super::{CALL_LIMIT, CLOBBERED, Frame, Run, Step, Walker};

/// What code may change, as far as the walk can tell without going
/// through it (see [`Walker::writes`]).
pub(super) struct Changes {
    /// The registers its instructions write.
    pub(super) registers: Registers,
    /// Whether it stores, or calls a function that may.
    pub(super) stores: bool,
    /// Of r0 to r3, those that may hold what it leaves (see
    /// `Path::results`).
    pub(super) results: u8,
}

impl Walker<'_> {
    /// What the code from `start` up to `end` may change, as far as the
    /// walk can tell without going through it. With `whole`, the code is a
    /// function's, and a jump out of it goes to another function, which may
    /// change r0 to r3, ip and lr.
    pub(super) fn writes(&mut self, start: u64, end: u64, whole: bool) -> Changes {
        let mut changes = Changes {
            registers: 0,
            stores: false,
            results: 0,
        };
        let mut address = start;
        while address < end {
            if whole && self.is_data(Some((start, end)), address) {
                address += 2;
                continue;
            }
            let Some((first, _, effect)) = self.decode(address) else {
                return Changes {
                    registers: Registers::MAX,
                    stores: true,
                    results: 0xf,
                };
            };
            changes.registers |= effect.writes;
            changes.results |= results(&effect);
            let leaves = match effect.flow {
                Flow::Call | Flow::CallRegister(..) => true,
                Flow::Branch { target, .. } => whole && !(start..end).contains(&target),
                Flow::JumpRegister(..) | Flow::JumpRelative(_) | Flow::JumpMemory => whole,
                _ => false,
            };
            if leaves {
                changes.registers |= CLOBBERED;
                changes.results = 0xf;
                changes.stores = true;
            }
            changes.stores |= matches!(
                effect.operation,
                Operation::Store { .. } | Operation::Push { .. }
            );
            address += if is_wide(first) { 4 } else { 2 };
        }
        changes
    }

    /// What a call to `target` may change, when the walk doesn't follow it
    /// to a return: of r0 to r3, ip and lr, the registers that the
    /// function's code writes, as code built with GCC's `-fipa-ra` counts
    /// on, or all of them; and of r0 to r3, those that may hold what it
    /// leaves (see `Path::results`).
    fn clobbers(&mut self, target: Option<u64>) -> (Registers, u8) {
        let function = target.and_then(|target| self.program.function_at(target));
        let Some((start, end)) = function.filter(|&(start, _)| Some(start) == target) else {
            return (CLOBBERED, 0xf);
        };
        if let Some(&clobbers) = self.clobbers.get(&start) {
            return clobbers;
        }
        let changes = self.writes(start, end, true);
        let clobbers = ((changes.registers | 1 << LR) & CLOBBERED, changes.results);
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
            None => Step::End(End::Branches { site, through }),
            Some(target) if self.program.code(target).is_none() => {
                Step::End(End::LeavesFile { site, target })
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
            arguments: arguments(&run.path),
            left: run.path.left,
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
                && let Some(end) = self.dead_end(run, site, target)
            {
                return Step::End(end);
            }
            // It returns with what the function it jumps to leaves in the
            // registers that pass arguments and results.
            let (clobbers, results) = self.clobbers(Some(target));
            set_unknown(&mut run.path, clobbers);
            run.path.results |= results;
            return Step::End(End::Returns(site));
        }
        if self.program.code(target).is_none() {
            return Step::End(End::LeavesFile { site, target });
        }
        run.bounds = self.program.function_at(target);
        Step::Next(target)
    }

    /// How the code that the jump at `site` to `target` goes to ends, when
    /// it ends where the walk can't read on and the walk has a branch it
    /// couldn't tell to go back to: walked as a function the walk calls,
    /// whose events are then dropped.
    fn dead_end(&mut self, run: &Run, site: u64, target: u64) -> Option<End> {
        let untried = |alternative: &Alternative| !run.visits.contains_key(&alternative.target);
        if !run.alternatives.iter().any(untried) {
            return None;
        }
        if self.program.code(target).is_none() {
            return Some(End::LeavesFile { site, target });
        }
        let (events, budget, lowest) = (self.events.len(), self.budget, self.lowest);
        let frame = Frame {
            depth: 1,
            follow: 0,
            limit: CALL_LIMIT,
            stop: None,
            nesting: 0,
            pass: None,
            rows: None,
        };
        let (end, _) = self.walk(target, frame, run.path.clone());
        self.events.truncate(events);
        (self.budget, self.lowest) = (budget, lowest);
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
            arguments: arguments(&run.path),
            left: run.path.left,
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
                let callee = Frame {
                    depth,
                    follow: frame.follow - 1,
                    limit: CALL_LIMIT,
                    stop: None,
                    nesting: 0,
                    pass: None,
                    rows: None,
                };
                // The call puts the address to return to in lr, which the
                // walk doesn't follow.
                let mut path = run.path.clone();
                path.registers[usize::from(LR)] = None;
                path.results = 0;
                let (end, path) = self.walk(target, callee, path);
                match end {
                    Some(End::Main(at)) => return Some(End::Main(at)),
                    Some(End::Returns(_)) => after = Some((path, true)),
                    Some(end) => {
                        if end.stops_short() {
                            self.events.push(Event::Stop { site, depth, end });
                        }
                        after = Some((path, false));
                    }
                    None => {}
                }
            }
        }
        let sp = run.path.registers[usize::from(SP)];
        let results = run.path.results;
        // Of r0 to r3, those that may hold what the call leaves.
        let called = match after {
            Some((mut path, true)) => {
                if path.forks {
                    forget_changes(&mut path, &run.path.registers, &run.path.memory);
                    path.flags = Flags::default();
                }
                let called = path.results;
                path.forks = run.path.forks;
                run.path = path;
                run.path.registers[usize::from(SP)] = sp;
                run.path.registers[usize::from(LR)] = None;
                called
            }
            Some((mut path, false)) => {
                forget_changes(&mut path, &run.path.registers, &run.path.memory);
                run.path.memory = path.memory;
                let (clobbers, called) = self.clobbers(target);
                set_unknown(&mut run.path, clobbers);
                run.path.flags = Flags::default();
                called
            }
            None => {
                let (clobbers, called) = self.clobbers(target);
                set_unknown(&mut run.path, clobbers);
                run.path.flags = Flags::default();
                called
            }
        };
        run.path.results = results | called;
        run.path.left |= called;
        None
    }
}

/// Where a branch through a register goes with `number` in it, as in
/// `values`: Cortex-M cores only run Thumb code, so `bx` and `blx` fault
/// when the low bit is 0.
pub(super) fn target(number: u32, low_bit: LowBit) -> Option<u64> {
    match low_bit {
        LowBit::Thumb if number & 1 == 0 => None,
        _ => Some(u64::from(number & !1)),
    }
}

/// What r0 to r3 hold.
pub(super) fn arguments(path: &Path) -> [Option<u32>; 4] {
    [0, 1, 2, 3].map(|register| path.registers[register])
}

/// Of r0 to r3, as bits, those that may hold what a call leaves after an
/// instruction that does what `effect` says (see `Path::results`): those
/// it writes, and r0 when it reads it, other than to push it.
pub(super) fn results(effect: &Effect) -> u8 {
    let reads_r0 = effect.reads & 1 != 0 && !matches!(effect.operation, Operation::Push { .. });
    (effect.writes & 0xf) as u8 | u8::from(reads_r0)
}
