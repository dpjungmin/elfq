//! The startup walk (see `arch::startup`) through Thumb code.
//!
//! It is a second reader of what `decode` says each instruction does,
//! beside `values`. `values` follows every path through one function at
//! once, with every number a register may hold on any of them, to find the
//! addresses that code branches through. The walk goes one way, as the
//! processor would from reset, through the functions that the startup
//! code calls, so a register holds one number or one it can't tell, and
//! memory is at the addresses the code uses, the stack among them.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use object::{Endian, Endianness};

use super::decode::{self, Effect, Flow, LowBit, Operation, Registers, When};
use super::{LR, PC, SP, Scanner, branch, is_wide, literal, sweep};
use crate::arch::CodeReferenceKind;
use crate::arch::startup::{
    Call, DEPTH, End, Event, How, Loop, Program, Start, Store, Through, Walk,
};
/// How many instructions the walk goes through, in all.
const BUDGET: usize = 20_000;
/// How many it goes through in the reset handler's own code, in each
/// function it calls, and in a loop's code.
const RESET_LIMIT: usize = 600;
const CALL_LIMIT: usize = 300;
const LOOP_LIMIT: usize = 60;
/// How many loops inside loops it walks the code of.
const NESTING: u32 = 8;

/// ip (r12), the scratch register that veneers and long calls use.
const IP: u16 = 12;
/// The registers that a called function may change and not put back: r0
/// to r3, which pass arguments and results, ip and lr.
const CLOBBERED: Registers = 0b1111 | 1 << IP | 1 << LR;

/// Walks the code from `start`; see `arch::startup::walk`.
pub(in crate::arch) fn walk(endianness: Endianness, program: &dyn Program, start: &Start) -> Walk {
    let mut walker = Walker {
        program,
        endianness,
        top: start.stack_pointer,
        lowest: start.stack_pointer,
        main: start.main,
        events: Vec::new(),
        budget: BUDGET,
        data: HashMap::new(),
        clobbers: HashMap::new(),
    };
    let mut path = Path::default();
    path.registers[usize::from(SP)] = start.stack_pointer;
    let frame = Frame {
        depth: 0,
        follow: start.follow.min(DEPTH),
        limit: RESET_LIMIT,
        stop: None,
        nesting: 0,
    };
    let (end, _) = walker.walk(start.address & !1, frame, path);
    Walk {
        events: walker.events,
        // Only the walk of a loop's code stops without an end.
        end: end.unwrap_or(End::Budget(start.address & !1)),
    }
}

/// What the walk knows on the way it goes.
#[derive(Debug, Clone, Default)]
struct Path {
    /// What each register holds, when the walk can tell; pc's is unused.
    registers: [Option<u32>; 16],
    /// The words that code stored on the way, by address: what the walk
    /// can tell of one, or `None` when the code stored something it can't
    /// tell there. Shared with the snapshots of earlier instructions until
    /// a store changes it.
    memory: Memory,
    flags: Flags,
    /// Whether the walk went past a branch or return whose condition it
    /// couldn't tell, so that what it knows holds only on the way it went.
    forks: bool,
    /// Of r0 to r3, as bits, those that hold what a call left there (see
    /// `results`), which code hasn't written since.
    left: u8,
    /// Of r0 to r3, as bits, those that may hold what the call it is in
    /// leaves, since the walk of that call began, in the function or in
    /// what it called: those that code wrote, and r0 when code read it
    /// other than to push it, as a function such as `memset` returns its
    /// first argument there without writing it. A register that the call
    /// only read keeps what the caller put there, as code built with GCC's
    /// `-fipa-ra` counts on for its next call.
    results: u8,
}

type Memory = Rc<BTreeMap<u32, Option<u32>>>;

/// The condition flags, each when the walk can tell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Flags {
    negative: Option<bool>,
    zero: Option<bool>,
    carry: Option<bool>,
    overflow: Option<bool>,
}

/// What the walk knew when it first got to an instruction, for a loop
/// that goes back there.
#[derive(Debug, Clone)]
struct Visit {
    registers: [Option<u32>; 16],
    memory: Memory,
    /// How many events the walk had found.
    events: usize,
}

/// A branch whose condition the walk couldn't tell, and what it knew there,
/// to take the branch if the way on doesn't go on.
#[derive(Debug, Clone)]
struct Alternative {
    site: u64,
    target: u64,
    events: usize,
    path: Path,
    /// How many instructions the walk had gone through.
    visited: usize,
    pending: Vec<(u64, u64)>,
    bounds: Option<(u64, u64)>,
}

/// One walk through a function's code, or through a loop's.
#[derive(Debug, Clone, Copy)]
struct Frame {
    depth: u32,
    /// How many calls deeper it follows.
    follow: u32,
    /// How many instructions it goes through.
    limit: usize,
    /// For a loop's code: the branch back, where its walk stops.
    stop: Option<u64>,
    /// How many loops' code it is in.
    nesting: u32,
}

/// Where one walk is.
struct Run {
    path: Path,
    /// The instructions it went through, in order.
    order: Vec<u64>,
    /// What the walk knew at each instruction it went through, when it got
    /// there.
    visits: HashMap<u64, Visit>,
    /// Where branches go that the walk didn't take, and where code goes on
    /// after branches it took, with where each branch is: the ways out of
    /// a loop.
    pending: Vec<(u64, u64)>,
    alternatives: Vec<Alternative>,
    /// The `it` block that the next instructions are in, as the processor
    /// keeps it: the condition in the top 4 bits and what is left of the
    /// mask in the bottom ones. 0 outside one.
    it: u8,
    /// The function the walk is in.
    bounds: Option<(u64, u64)>,
    /// How many instructions it went through.
    count: usize,
    /// Whether the walk got here from a call that returned, through
    /// instructions that only go on to the next.
    after_call: bool,
}

/// What an instruction leads to.
enum Step {
    /// On to the instruction at this address.
    Next(u64),
    /// A loop's walk got to its branch back.
    Stopped,
    /// The walk ends here, unless it goes back to a branch it didn't take.
    End(End),
    /// The walk ends here.
    Final(End),
}

struct Walker<'a> {
    program: &'a dyn Program,
    endianness: Endianness,
    /// The stack pointer at reset: the top of the stack.
    top: Option<u32>,
    /// The lowest the stack pointer has been.
    lowest: Option<u32>,
    main: Option<u64>,
    events: Vec<Event>,
    /// How many more instructions the walk may go through.
    budget: usize,
    /// The data in each function the walk got to, by where the function
    /// starts: the constants and jump tables its code reads, as offsets
    /// from its start.
    data: HashMap<u64, BTreeMap<usize, usize>>,
    /// What [`Walker::clobbers`] found for each function, by where it
    /// starts.
    clobbers: HashMap<u64, (Registers, u8)>,
}

/// What code may change, as far as the walk can tell without going
/// through it (see [`Walker::writes`]).
struct Changes {
    /// The registers its instructions write.
    registers: Registers,
    /// Whether it stores, or calls a function that may.
    stores: bool,
    /// Of r0 to r3, those that may hold what it leaves (see
    /// `Path::results`).
    results: u8,
}

impl Walker<'_> {
    /// Walks from `start` with what `path` knows, until it ends, or for a
    /// loop's code, until it gets to `frame.stop`. Returns how it ended,
    /// or `None` when it got there, and what it knew then.
    fn walk(&mut self, start: u64, frame: Frame, path: Path) -> (Option<End>, Path) {
        let mut run = Run {
            path,
            order: Vec::new(),
            visits: HashMap::new(),
            pending: Vec::new(),
            alternatives: Vec::new(),
            it: 0,
            bounds: self.program.function_at(start),
            count: 0,
            after_call: false,
        };
        let mut address = start;
        loop {
            let end = match self.step(&mut run, frame, address) {
                Step::Next(next) => {
                    address = next;
                    continue;
                }
                Step::Stopped => return (None, run.path),
                Step::Final(end) => end,
                Step::End(end) => match self.backtrack(&mut run, frame, end) {
                    Some(target) => {
                        address = target;
                        continue;
                    }
                    None => end,
                },
            };
            return (Some(end), run.path);
        }
    }

    /// Runs the instruction at `address`.
    fn step(&mut self, run: &mut Run, frame: Frame, address: u64) -> Step {
        if frame.stop.is_some_and(|stop| address >= stop) {
            return Step::Stopped;
        }
        if run.leaves(address) {
            if run.after_call {
                return Step::Final(End::RunsOff(address));
            }
            // Code that runs on into the next function, as hand-written
            // startup code may: the walk goes on there.
            run.bounds = self.program.function_at(address);
        }
        if run.count >= frame.limit || self.budget == 0 {
            return Step::Final(End::Budget(address));
        }
        run.count += 1;
        self.budget -= 1;
        match run.visits.entry(address) {
            Entry::Occupied(_) => return Step::Final(End::Rejoins(address)),
            Entry::Vacant(entry) => entry.insert(Visit {
                registers: run.path.registers,
                memory: Rc::clone(&run.path.memory),
                events: self.events.len(),
            }),
        };
        run.order.push(address);
        if self.main == Some(address) {
            return Step::Final(End::Main(address));
        }
        let after_call = std::mem::take(&mut run.after_call);
        let decoded = self.decode(address);
        let Some((first, second, effect)) = decoded.filter(|_| !self.is_data(run.bounds, address))
        else {
            return no_code(address, after_call);
        };
        // `bkpt` straight after a call, as after the call to `exit` in the
        // Pico SDK's startup, which faults without a debugger, but for
        // `bkpt 0xab`, which asks a debugger for semihosting.
        if after_call && first & 0xff00 == 0xbe00 && first & 0xff != 0xab {
            return no_code(address, after_call);
        }
        let next = address + if is_wide(first) { 4 } else { 2 };
        if let Flow::It { firstcond, mask } = effect.flow {
            run.it = (firstcond << 4 | mask) as u8;
            return Step::Next(next);
        }
        let condition = run.next_condition();
        let runs = condition.map_or(Some(true), |condition| holds(condition, run.path.flags));
        if runs == Some(false) {
            return Step::Next(next);
        }
        // 16-bit instructions that start below 0x4400 set the flags
        // outside an `it` block too.
        let changes_flags =
            effect.sets_flags || (!is_wide(first) && first < 0x4400 && condition.is_none());
        let conditional = runs.is_none();
        let depth = frame.depth;
        match effect.flow {
            Flow::Next | Flow::It { .. } => {
                self.execute(
                    &mut run.path,
                    address,
                    &effect,
                    changes_flags,
                    conditional,
                    depth,
                );
                run.after_call = after_call;
                Step::Next(next)
            }
            Flow::Call | Flow::CallRegister(..) => {
                let (target, how) = match effect.flow {
                    Flow::CallRegister(register, low_bit) => (
                        self.register(&run.path, register, address)
                            .and_then(|number| target(number, low_bit)),
                        How::Register(register),
                    ),
                    // svc
                    _ if !is_wide(first) => (None, How::Supervisor),
                    _ => match branch(address + 4, first, second) {
                        Some(CodeReferenceKind::Call(target)) => (Some(target), How::Direct),
                        _ => (None, How::Direct),
                    },
                };
                if let Some(end) = self.call(run, frame, address, target, how, conditional) {
                    return Step::Final(end);
                }
                run.after_call = true;
                Step::Next(next)
            }
            Flow::Branch { target, when } => {
                let taken = match when {
                    When::Always => runs,
                    When::Flags(condition) => holds(condition, run.path.flags),
                    When::Zero(register) => {
                        self.register(&run.path, register, address).map(|n| n == 0)
                    }
                    When::NotZero(register) => {
                        self.register(&run.path, register, address).map(|n| n != 0)
                    }
                };
                if when == When::Always && condition.is_none() {
                    self.jump(run, frame, address, target)
                } else {
                    self.branch(run, frame, address, next, target, taken)
                }
            }
            _ if conditional => {
                // It may jump or return, or go on with what it writes
                // unknown; the walk goes on.
                self.execute(&mut run.path, address, &effect, changes_flags, true, depth);
                run.path.forks = true;
                Step::Next(next)
            }
            Flow::Return => Step::End(End::Returns(address)),
            Flow::Pop => {
                // A return, unless the walk can tell the word it pops into
                // pc, as LLD's Cortex-M0 veneers push the address they go
                // to and pop it: a called function starts with lr unknown.
                let to = self.popped_pc(&run.path, effect.operation);
                self.execute(&mut run.path, address, &effect, false, false, depth);
                match to.and_then(|number| target(number, LowBit::Thumb)) {
                    None => Step::End(End::Returns(address)),
                    Some(to) => self.jump_to(run, frame, address, Some(to), Through::Memory),
                }
            }
            Flow::Stop => no_code(address, after_call),
            Flow::Table => Step::End(End::Branches {
                site: address,
                through: Through::Table,
            }),
            Flow::JumpRegister(register, low_bit) => {
                let to = self.register(&run.path, register, address);
                let through = Through::Register(register);
                self.jump_to(
                    run,
                    frame,
                    address,
                    to.and_then(|n| target(n, low_bit)),
                    through,
                )
            }
            Flow::JumpRelative(register) => {
                let pc = (address as u32).wrapping_add(4);
                let to = self.register(&run.path, register, address);
                let to = to.map(|offset| u64::from(pc.wrapping_add(offset) & !1));
                self.jump_to(run, frame, address, to, Through::Register(register))
            }
            Flow::JumpMemory => {
                // ldr pc, [sp, #imm] returns.
                if let Operation::Load { base: SP, .. } = effect.operation {
                    return Step::End(End::Returns(address));
                }
                let at = match effect.operation {
                    Operation::Load {
                        base,
                        offset: Some(offset),
                        ..
                    } => self
                        .register(&run.path, base, address)
                        .map(|base| base.wrapping_add_signed(offset)),
                    // ldr.w pc, [pc, #imm], as in a veneer.
                    Operation::Nothing if first & 0xff7f == 0xf85f => {
                        Some(literal(address + 4, first, u64::from(second & 0xfff)) as u32)
                    }
                    _ => None,
                };
                let to = at.and_then(|at| self.load(&run.path, at, true));
                let to = to.and_then(|number| target(number, LowBit::Thumb));
                self.jump_to(run, frame, address, to, Through::Memory)
            }
        }
    }

    /// The word that `operation`, a `pop` with pc, pops into pc, when the
    /// walk can tell it.
    fn popped_pc(&self, path: &Path, operation: Operation) -> Option<u32> {
        let Operation::Pop { registers, .. } = operation else {
            return None;
        };
        // pc is the highest register, so its word is the last.
        let sp = path.registers[usize::from(SP)]?;
        self.load(
            path,
            sp.wrapping_add(4 * (registers.count_ones() - 1)),
            false,
        )
    }

    /// An unconditional branch at `site` to `target`.
    fn jump(&mut self, run: &mut Run, frame: Frame, site: u64, target: u64) -> Step {
        if target == site {
            return Step::End(End::Spins(site));
        }
        if let Some(visit) = run.visits.get(&target).filter(|_| target < site) {
            let visit = visit.clone();
            self.close_loop(run, frame, target, site, &visit, None);
            return match run.way_out(target, site) {
                Some(exit) => Step::Next(exit),
                None => Step::End(End::Loops(site)),
            };
        }
        if run.leaves(target) {
            return self.tail_call(run, frame, site, target);
        }
        Step::Next(target)
    }

    /// A branch at `site` that goes to `target` when `taken`, or to `next`.
    fn branch(
        &mut self,
        run: &mut Run,
        frame: Frame,
        site: u64,
        next: u64,
        target: u64,
        taken: Option<bool>,
    ) -> Step {
        if run.leaves(target) && !(target <= site && self.went_through(run, target)) {
            // A tail call when the branch is taken, as Clang makes of `if
            // (a) f(); else g();`.
            return match taken {
                Some(true) => self.tail_call(run, frame, site, target),
                Some(false) => Step::Next(next),
                None => {
                    self.events.push(Event::Call(Call {
                        site,
                        depth: frame.depth,
                        target: Some(target),
                        how: How::Jump,
                        arguments: arguments(&run.path),
                        left: run.path.left,
                        conditional: true,
                        followed: false,
                    }));
                    run.path.forks = true;
                    Step::Next(next)
                }
            };
        }
        if target <= site {
            // A loop: the walk goes through its code once. When the code
            // was jumped over to the test at its end, it is walked from
            // here, with what the walk knows at the test.
            let (visit, end) = match run.visits.get(&target) {
                Some(visit) => (visit.clone(), None),
                // The code was jumped over to a test that doesn't go back:
                // the loop runs no times, as for an empty section.
                _ if taken == Some(false) => return Step::Next(next),
                _ => {
                    let before = run.visit(self.events.len());
                    match self.loop_body(run, frame, target, site) {
                        Some(End::Main(at)) => return Step::Final(End::Main(at)),
                        end => (before, end),
                    }
                }
            };
            self.close_loop(run, frame, target, site, &visit, end);
            return Step::Next(next);
        }
        if taken == Some(true) {
            run.pending.push((site, next));
            return Step::Next(target);
        }
        if taken.is_none() && frame.stop.is_none() {
            // A loop's code that the compiler put after the function's
            // other code, as GCC -Os does, which branches back to here.
            if let Some(back) = self.out_of_line_body(target, site, run.bounds) {
                let before = run.visit(self.events.len());
                let end = self.loop_body(run, frame, target, back);
                if let Some(End::Main(at)) = end {
                    return Step::Final(End::Main(at));
                }
                self.close_loop(run, frame, target, back, &before, end);
                return Step::Next(next);
            }
            run.alternatives.push(Alternative {
                site,
                target,
                events: self.events.len(),
                path: run.path.clone(),
                visited: run.order.len(),
                pending: run.pending.clone(),
                bounds: run.bounds,
            });
        }
        if taken.is_none() {
            run.path.forks = true;
        }
        run.pending.push((site, target));
        Step::Next(next)
    }

    /// Whether the walk went through code of the function that holds
    /// `target`, as a loop's code that a function finder took for another
    /// function.
    fn went_through(&self, run: &Run, target: u64) -> bool {
        let Some((start, end)) = self.program.function_at(target) else {
            return false;
        };
        run.order
            .iter()
            .any(|address| (start..end).contains(address))
    }

    /// Walks a loop's code once, from `head` to the branch at `back`, with
    /// what the walk knows now, and goes on with what it knows after.
    /// Returns where the walk of the code stopped before `back`, if it did.
    fn loop_body(&mut self, run: &mut Run, frame: Frame, head: u64, back: u64) -> Option<End> {
        if frame.nesting >= NESTING {
            return Some(End::Budget(head));
        }
        let body = Frame {
            depth: frame.depth,
            follow: frame.follow,
            limit: LOOP_LIMIT,
            stop: Some(back),
            nesting: frame.nesting + 1,
        };
        let (end, path) = self.walk(head, body, run.path.clone());
        run.path = path;
        end
    }

    /// Records the loop from `head` to `back`, and forgets what its code
    /// changed since `before`, at its head. When the walk of its code
    /// stopped short, at `end`, it forgets too what the rest of the code
    /// may change.
    fn close_loop(
        &mut self,
        run: &mut Run,
        frame: Frame,
        head: u64,
        back: u64,
        before: &Visit,
        end: Option<End>,
    ) {
        self.events.push(Event::Loop(Loop {
            head,
            back,
            depth: frame.depth,
            first_event: before.events,
            end,
        }));
        forget_changes(&mut run.path, &before.registers, &before.memory);
        if end.is_some() {
            let Changes {
                registers,
                stores,
                results,
            } = self.writes(head, back, false);
            set_unknown(&mut run.path, registers & !(1 << SP));
            run.path.results |= results;
            if stores && run.path.memory.values().any(Option::is_some) {
                (Rc::make_mut(&mut run.path.memory).values_mut()).for_each(|word| *word = None);
            }
        }
        run.path.flags = Flags::default();
    }

    /// What the code from `start` up to `end` may change, as far as the
    /// walk can tell without going through it. With `whole`, the code is a
    /// function's, and a jump out of it goes to another function, which may
    /// change r0 to r3, ip and lr.
    fn writes(&mut self, start: u64, end: u64, whole: bool) -> Changes {
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
    fn jump_to(
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
    fn tail_call(&mut self, run: &mut Run, frame: Frame, site: u64, target: u64) -> Step {
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
    fn call(
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
                        if end.is_dead() || matches!(end, End::Budget(_)) {
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

    /// Goes back to the latest branch whose condition the walk couldn't
    /// tell and that it hasn't been to, after the walk ended with `end`, in
    /// the reset handler's code: when `end` is dead, or a return or a spin
    /// before `main`. Returns where the branch goes.
    fn backtrack(&mut self, run: &mut Run, frame: Frame, end: End) -> Option<u64> {
        let stops_short = self.main.is_some() && matches!(end, End::Spins(_) | End::Returns(_));
        let back = frame.depth == 0 && frame.stop.is_none() && (end.is_dead() || stops_short);
        if !back || self.budget == 0 {
            return None;
        }
        while let Some(alternative) = run.alternatives.pop() {
            if run.visits.contains_key(&alternative.target) {
                continue;
            }
            self.events.truncate(alternative.events);
            self.events.push(Event::Backtrack {
                site: alternative.site,
                target: alternative.target,
                because: end,
            });
            for address in run.order.drain(alternative.visited..) {
                run.visits.remove(&address);
            }
            run.path = alternative.path;
            run.pending = alternative.pending;
            run.bounds = alternative.bounds;
            run.it = 0;
            return Some(alternative.target);
        }
        None
    }

    /// Where a loop body that the compiler put after the function's other
    /// code ends: the block at `start`, which a branch at `site` goes to,
    /// ends with a branch back to the loop's test, which leads to `site`,
    /// in the function.
    fn out_of_line_body(&self, start: u64, site: u64, bounds: Option<(u64, u64)>) -> Option<u64> {
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

    /// Changes `path` as the instruction at `site` does, other than where
    /// it goes next. When it is `conditional`, in an `it` block whose
    /// condition the walk can't tell, what it writes becomes unknown.
    fn execute(
        &mut self,
        path: &mut Path,
        site: u64,
        effect: &Effect,
        changes_flags: bool,
        conditional: bool,
        depth: u32,
    ) {
        path.results |= results(effect);
        path.left &= !(effect.writes & 0xf) as u8;
        let operation = with_writeback_offset(effect);
        if let Operation::Store {
            register,
            base,
            offset,
            size,
        } = operation
        {
            self.store(path, site, depth, register, base, offset, size, conditional);
        }
        let flags = match (changes_flags, conditional) {
            (false, _) => path.flags,
            (true, true) => Flags::default(),
            (true, false) => self.flags(path, site, effect),
        };
        if conditional {
            set_unknown(path, effect.writes);
            path.flags = flags;
            return;
        }
        let result = self.result(path, site, operation);
        // The base register after a load or store that adds to it.
        let moved = match (operation, effect.writeback) {
            (Operation::Load { base, .. } | Operation::Store { base, .. }, Some(writeback)) => {
                let after = self.register(path, base, site);
                Some((base, after.map(|n| n.wrapping_add_signed(writeback.amount))))
            }
            _ => None,
        };
        let sp = path.registers[usize::from(SP)];
        let mut pushed = Vec::new();
        if let (Operation::Push { registers, size }, Some(sp)) = (effect.operation, sp) {
            let bottom = sp.wrapping_sub(size);
            for (n, register) in each(registers).enumerate() {
                let word = self.register(path, register, site);
                pushed.push((bottom.wrapping_add(4 * n as u32), word));
            }
            // The floating-point registers that `vpush` stores.
            for at in (bottom..sp).step_by(4).skip(pushed.len()) {
                pushed.push((at, None));
            }
        }
        set_unknown(path, effect.writes);
        match (effect.operation, sp) {
            (Operation::Push { size, .. }, Some(sp)) => {
                let memory = Rc::make_mut(&mut path.memory);
                for (at, word) in pushed {
                    memory.insert(at, word);
                }
                path.registers[usize::from(SP)] = Some(sp.wrapping_sub(size));
            }
            (Operation::Pop { registers, size }, Some(sp)) => {
                for (n, register) in each(registers).enumerate() {
                    if register != PC {
                        let at = sp.wrapping_add(4 * n as u32);
                        path.registers[usize::from(register)] = self.load(path, at, false);
                    }
                }
                path.registers[usize::from(SP)] = Some(sp.wrapping_add(size));
            }
            _ => {}
        }
        for (register, number) in result.into_iter().chain(moved) {
            if register != PC {
                path.registers[usize::from(register)] = number;
            }
        }
        if let (Some(lowest), Some(sp)) = (self.lowest, path.registers[usize::from(SP)]) {
            self.lowest = Some(lowest.min(sp));
        }
        path.flags = flags;
    }

    /// The register that `operation` writes a number to, and the number,
    /// when the walk can tell it.
    fn result(&self, path: &Path, site: u64, operation: Operation) -> Option<(u16, Option<u32>)> {
        let get = |register| self.register(path, register, site);
        let number = |register, number| Some((register, number));
        match operation {
            Operation::Constant { register, value } => number(register, Some(value)),
            Operation::Literal { register, address } => {
                number(register, self.load(path, address as u32, true))
            }
            Operation::Address { register, value } => number(register, Some(value)),
            Operation::HighHalf { register, high } => {
                number(register, get(register).map(|low| low & 0xffff | high << 16))
            }
            Operation::Copy { register, source } => number(register, get(source)),
            Operation::AddConstant {
                register,
                source,
                amount,
            } => number(register, get(source).map(|n| n.wrapping_add(amount))),
            Operation::Add { register, a, b } => {
                number(register, get(a).zip(get(b)).map(|(a, b)| a.wrapping_add(b)))
            }
            Operation::Subtract { register, a, b } => {
                number(register, get(a).zip(get(b)).map(|(a, b)| a.wrapping_sub(b)))
            }
            Operation::AndConstant {
                register,
                source,
                mask,
            } => number(register, get(source).map(|n| n & mask)),
            Operation::And {
                register,
                a,
                b,
                complement,
            } => number(register, and(get(a), get(b), complement)),
            Operation::ShiftLeft {
                register,
                source,
                shift,
            } => number(register, get(source).map(|n| n << shift)),
            Operation::ShiftRight {
                register,
                source,
                shift,
                signed,
            } => number(register, get(source).map(|n| shift_right(n, shift, signed))),
            Operation::Load {
                register: Some(register),
                base,
                offset: Some(offset),
            } => {
                let at = get(base).map(|base| base.wrapping_add_signed(offset));
                number(register, at.and_then(|at| self.load(path, at, false)))
            }
            Operation::Nothing
            | Operation::Load { .. }
            | Operation::Store { .. }
            | Operation::Push { .. }
            | Operation::Pop { .. }
            | Operation::Compare { .. }
            | Operation::CompareRegisters { .. } => None,
        }
    }

    /// The flags after the instruction at `site`, which changes them.
    fn flags(&self, path: &Path, site: u64, effect: &Effect) -> Flags {
        let get = |register| self.register(path, register, site);
        let old = path.flags;
        // Moves and logic leave the overflow flag as it was.
        let logic = |result: Option<u32>, carry| Flags {
            carry,
            overflow: old.overflow,
            ..negative_and_zero(result)
        };
        match effect.operation {
            Operation::Compare { register, value } => subtract(get(register), Some(value)),
            Operation::CompareRegisters { a, b } | Operation::Subtract { a, b, .. } => {
                subtract(get(a), get(b))
            }
            Operation::Add { a, b, .. } => add(get(a), get(b)),
            // `adds` and `subs` both come as an amount to add, and set the
            // carry flag apart only for 0, and the overflow flag only for
            // 0x80000000.
            Operation::AddConstant { source, amount, .. } => {
                let flags = add(get(source), Some(amount));
                match amount {
                    0 => Flags {
                        carry: None,
                        ..flags
                    },
                    0x8000_0000 => Flags {
                        overflow: None,
                        ..flags
                    },
                    _ => flags,
                }
            }
            // movs keeps the carry flag; mov.w and mvn with an immediate
            // may set it.
            Operation::Constant { value, .. } => {
                let wide = effect.sets_flags;
                logic(Some(value), if wide { None } else { old.carry })
            }
            Operation::ShiftLeft { source, shift, .. } => {
                let n = get(source);
                let carry = match shift {
                    0 => old.carry,
                    _ => n.map(|n| n >> (32 - shift) & 1 == 1),
                };
                logic(n.map(|n| n << shift), carry)
            }
            Operation::ShiftRight {
                source,
                shift,
                signed,
                ..
            } => {
                let n = get(source);
                let carry = n.map(|n| (u64::from(n) >> (shift - 1)) & 1 == 1);
                logic(n.map(|n| shift_right(n, shift, signed)), carry)
            }
            Operation::AndConstant { source, mask, .. } => {
                logic(get(source).map(|n| n & mask), None)
            }
            Operation::And {
                a, b, complement, ..
            } => logic(and(get(a), get(b), complement), old.carry),
            _ => Flags::default(),
        }
    }

    /// Records a store at `site` of `size` bytes at `base` + `offset`, from
    /// `register` for a word, and changes the memory that `path` knows.
    #[allow(clippy::too_many_arguments)]
    fn store(
        &mut self,
        path: &mut Path,
        site: u64,
        depth: u32,
        register: Option<u16>,
        base: u16,
        offset: Option<i32>,
        size: u32,
        conditional: bool,
    ) {
        let from = self.register(path, base, site);
        let address = from
            .zip(offset)
            .map(|(from, offset)| from.wrapping_add_signed(offset));
        let value = register.and_then(|register| self.register(path, register, site));
        if let Some(address) = address {
            let memory = Rc::make_mut(&mut path.memory);
            match register {
                Some(_) if size == 4 && address.is_multiple_of(4) && !conditional => {
                    memory.insert(address, value);
                }
                // Part of a word, more than one, or what the walk can't
                // tell: the words it stores in are unknown.
                _ => {
                    let last = address.wrapping_add(size.max(1) - 1) & !3;
                    let mut word = address & !3;
                    memory.insert(word, None);
                    while word != last {
                        word = word.wrapping_add(4);
                        memory.insert(word, None);
                    }
                }
            }
        }
        // Stores to the stack are judged by where they go, not by the
        // register they go through: unoptimized code keeps a pointer to
        // its locals in r7.
        let stack = base == SP || self.is_stack(address.or(from));
        if !stack {
            self.events.push(Event::Store(Store {
                site,
                depth,
                address,
                base: from,
                size,
                value,
                conditional,
            }));
        }
    }

    /// Whether `address` is on the stack: from the lowest the stack pointer
    /// has been up to the top.
    fn is_stack(&self, address: Option<u32>) -> bool {
        match (address, self.lowest, self.top) {
            (Some(address), Some(lowest), Some(top)) => (lowest..top).contains(&address),
            _ => false,
        }
    }

    /// The word at `address`: what code stored there on the way, or what
    /// is there at reset. A literal pool is read as the code is, when
    /// `literal`, as if copied into place.
    fn load(&self, path: &Path, address: u32, literal: bool) -> Option<u32> {
        if !address.is_multiple_of(4) {
            return None;
        }
        if let Some(&word) = path.memory.get(&address) {
            return word;
        }
        let code = self.program.code(u64::from(address)).filter(|_| literal);
        match code.and_then(|bytes| bytes.get(..4)) {
            Some(bytes) => Some(self.endianness.read_u32(bytes.try_into().ok()?)),
            None => self.program.word_at_reset(u64::from(address)),
        }
    }

    /// What `register` holds at the instruction at `site`: for pc, the
    /// instruction's address + 4.
    fn register(&self, path: &Path, register: u16, site: u64) -> Option<u32> {
        match register {
            PC => Some((site as u32).wrapping_add(4)),
            _ => path.registers[usize::from(register)],
        }
    }

    /// The instruction at `address`: its two halves and what it does.
    fn decode(&self, address: u64) -> Option<(u16, u16, Effect)> {
        decode_at(self.program, address)
    }

    /// Whether `address`, in the function at `bounds`, holds data that the
    /// function's code reads, such as a literal pool, which compilers put
    /// after its last instruction, as after a call that doesn't return.
    fn is_data(&mut self, bounds: Option<(u64, u64)>, address: u64) -> bool {
        let Some((start, end)) = bounds else {
            return false;
        };
        let (program, endianness) = (self.program, self.endianness);
        let data = self.data.entry(start).or_insert_with(|| {
            let code = program.code(start).unwrap_or_default();
            let code = &code[..code.len().min((end - start) as usize)];
            sweep(Scanner::new(code, start, endianness)).data
        });
        let offset = (address - start) as usize;
        data.range(..=offset)
            .next_back()
            .is_some_and(|(_, &end)| offset < end)
    }
}

/// How the walk ends at `address`, where there is no code it can run:
/// data, `udf` or an instruction elfq can't decode. Straight after a call,
/// the call can't return, as compilers put `udf` or a literal pool after a
/// call to a function that doesn't, and hand-written code `bkpt`.
fn no_code(address: u64, after_call: bool) -> Step {
    match after_call {
        true => Step::Final(End::RunsOff(address)),
        false => Step::End(End::Undecoded(address)),
    }
}

impl Run {
    /// What the walk knows now, as it would be at an instruction it got to.
    fn visit(&self, events: usize) -> Visit {
        Visit {
            registers: self.path.registers,
            memory: Rc::clone(&self.path.memory),
            events,
        }
    }

    /// The condition of the next instruction in an `it` block, which moves
    /// the block on by one; `None` outside one.
    fn next_condition(&mut self) -> Option<u16> {
        if self.it & 0xf == 0 {
            return None;
        }
        let condition = u16::from(self.it >> 4);
        self.it = match self.it & 7 {
            0 => 0,
            _ => self.it & 0xe0 | (self.it << 1) & 0x1f,
        };
        Some(condition)
    }

    /// Whether `target` is outside the function the walk is in.
    fn leaves(&self, target: u64) -> bool {
        self.bounds
            .is_some_and(|(start, end)| !(start..end).contains(&target))
    }

    /// Where the walk goes on after a loop from `head` to a branch back at
    /// `site`, through a branch in it: the first place after it that the
    /// walk hasn't been to, or the latest one before it.
    fn way_out(&mut self, head: u64, site: u64) -> Option<u64> {
        let open = |&&(from, to): &&(u64, u64)| {
            (head..=site).contains(&from) && !self.visits.contains_key(&to)
        };
        let after = self
            .pending
            .iter()
            .filter(open)
            .filter(|&&(_, to)| to > site);
        let exit = after.min_by_key(|&&(_, to)| to);
        let exit = *exit.or_else(|| self.pending.iter().rfind(open))?;
        let index = self.pending.iter().position(|&pending| pending == exit)?;
        self.pending.remove(index);
        Some(exit.1)
    }
}

/// Whether condition `condition`, the field of `b<cond>` and `it`, holds
/// with `flags`, when the walk can tell.
fn holds(condition: u16, flags: Flags) -> Option<bool> {
    let Flags {
        negative,
        zero,
        carry,
        overflow,
    } = flags;
    let same = |a: Option<bool>, b: Option<bool>| Some(a? == b?);
    let base = match condition >> 1 {
        0 => zero,
        1 => carry,
        2 => negative,
        3 => overflow,
        // hi and ls
        4 => both(carry, not(zero)),
        // ge and lt
        5 => same(negative, overflow),
        // gt and le
        6 => both(not(zero), same(negative, overflow)),
        _ => return Some(true),
    };
    if condition & 1 == 1 { not(base) } else { base }
}

fn not(a: Option<bool>) -> Option<bool> {
    a.map(|a| !a)
}

/// Whether `a` and `b` both hold: false when either is, even if the walk
/// can't tell the other.
fn both(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

/// The flags after `a` - `b`, as `cmp` and `subs` set them.
fn subtract(a: Option<u32>, b: Option<u32>) -> Flags {
    let (Some(a), Some(b)) = (a, b) else {
        return Flags::default();
    };
    let result = a.wrapping_sub(b);
    Flags {
        carry: Some(a >= b),
        overflow: Some(((a ^ b) & (a ^ result)) >> 31 == 1),
        ..negative_and_zero(Some(result))
    }
}

/// The flags after `a` + `b`, as `adds` and `cmn` set them.
fn add(a: Option<u32>, b: Option<u32>) -> Flags {
    let (Some(a), Some(b)) = (a, b) else {
        return Flags::default();
    };
    let (result, carry) = a.overflowing_add(b);
    Flags {
        carry: Some(carry),
        overflow: Some((!(a ^ b) & (a ^ result)) >> 31 == 1),
        ..negative_and_zero(Some(result))
    }
}

/// The negative and zero flags for `result`, and no others.
fn negative_and_zero(result: Option<u32>) -> Flags {
    Flags {
        negative: result.map(|n| n >> 31 == 1),
        zero: result.map(|n| n == 0),
        ..Flags::default()
    }
}

/// `a` & `b`, or with `complement`, `a` & !`b`.
fn and(a: Option<u32>, b: Option<u32>, complement: bool) -> Option<u32> {
    let (a, b) = (a?, b?);
    Some(if complement { a & !b } else { a & b })
}

/// `number` >> `shift`, from 1 to 32, as `lsr` does, or `asr` when
/// `signed`.
fn shift_right(number: u32, shift: u32, signed: bool) -> u32 {
    if signed {
        ((number as i32) >> shift.min(31)) as u32
    } else {
        number.checked_shr(shift).unwrap_or(0)
    }
}

/// Where a branch through a register goes with `number` in it, as in
/// `values`: Cortex-M cores only run Thumb code, so `bx` and `blx` fault
/// when the low bit is 0.
fn target(number: u32, low_bit: LowBit) -> Option<u64> {
    match low_bit {
        LowBit::Thumb if number & 1 == 0 => None,
        _ => Some(u64::from(number & !1)),
    }
}

/// What r0 to r3 hold.
fn arguments(path: &Path) -> [Option<u32>; 4] {
    [0, 1, 2, 3].map(|register| path.registers[register])
}

/// Of r0 to r3, as bits, those that may hold what a call leaves after an
/// instruction that does what `effect` says (see `Path::results`): those
/// it writes, and r0 when it reads it, other than to push it.
fn results(effect: &Effect) -> u8 {
    let reads_r0 = effect.reads & 1 != 0 && !matches!(effect.operation, Operation::Push { .. });
    (effect.writes & 0xf) as u8 | u8::from(reads_r0)
}

/// Forgets what changed in `path`, but for the stack pointer, since it
/// held `registers` and `memory`.
fn forget_changes(path: &mut Path, registers: &[Option<u32>; 16], memory: &Memory) {
    for register in (0..16).filter(|&register| register != usize::from(SP)) {
        if path.registers[register] != registers[register] {
            path.registers[register] = None;
        }
    }
    if Rc::ptr_eq(&path.memory, memory) {
        return;
    }
    let changed: Vec<u32> = (path.memory.iter())
        .filter(|&(address, &word)| memory.get(address) != Some(&word))
        .map(|(&address, _)| address)
        .chain(
            (memory.keys())
                .filter(|address| !path.memory.contains_key(address))
                .copied(),
        )
        .collect();
    let words = Rc::make_mut(&mut path.memory);
    for address in changed {
        words.insert(address, None);
    }
}

/// What `effect` does, where a load or store that adds to its base
/// register gives the offset it loads or stores at: what it adds when it
/// adds first, or else 0.
fn with_writeback_offset(effect: &Effect) -> Operation {
    let Some(writeback) = effect.writeback else {
        return effect.operation;
    };
    let offset = Some(if writeback.before {
        writeback.amount
    } else {
        0
    });
    match effect.operation {
        Operation::Load { register, base, .. } => Operation::Load {
            register,
            base,
            offset,
        },
        Operation::Store {
            register,
            base,
            size,
            ..
        } => Operation::Store {
            register,
            base,
            offset,
            size,
        },
        operation => operation,
    }
}

/// Forgets what `registers` hold.
fn set_unknown(path: &mut Path, registers: Registers) {
    for register in each(registers) {
        path.registers[usize::from(register)] = None;
    }
}

/// The registers in `set`, from r0 up.
fn each(set: Registers) -> impl Iterator<Item = u16> {
    (0..16).filter(move |register| set & (1 << register) != 0)
}

/// See `arch::startup::passes_result`.
pub(in crate::arch) fn passes_result(program: &dyn Program, first: u64, second: u64) -> bool {
    if !(first < second && second - first <= 16) {
        return false;
    }
    let Some((first_half, _, _)) = decode_at(program, first) else {
        return false;
    };
    let mut address = first + if is_wide(first_half) { 4 } else { 2 };
    // The registers that hold what the first call returns in r0.
    let mut result: u16 = 1;
    while address < second {
        let Some((half, _, effect)) = decode_at(program, address) else {
            return false;
        };
        match effect.operation {
            Operation::Copy { register, source } if register < 16 && source < 16 => {
                match result & 1 << source {
                    0 => result &= !(1 << register),
                    _ => result |= 1 << register,
                }
            }
            Operation::Literal { register, .. } if register < 16 => result &= !(1 << register),
            _ => return false,
        }
        if effect.flow != Flow::Next {
            return false;
        }
        address += if is_wide(half) { 4 } else { 2 };
    }
    address == second && result & 1 != 0
}

/// See `arch::startup::branches_past`.
pub(in crate::arch) fn branches_past(program: &dyn Program, start: u64, call: u64) -> bool {
    let Some((half, _, _)) = decode_at(program, call) else {
        return false;
    };
    let after = call + if is_wide(half) { 4 } else { 2 };
    let mut address = start;
    while address < call {
        let Some((half, _, effect)) = decode_at(program, address) else {
            address += 2;
            continue;
        };
        if let Flow::Branch { target, .. } = effect.flow
            && target == after
        {
            return true;
        }
        address += if is_wide(half) { 4 } else { 2 };
    }
    false
}

/// The instruction at `address` in `program`: its two halves and what it
/// does.
fn decode_at(program: &dyn Program, address: u64) -> Option<(u16, u16, Effect)> {
    let code = program.code(address)?;
    let half = |at: usize| Some(u16::from_le_bytes([*code.get(at)?, *code.get(at + 1)?]));
    let first = half(0)?;
    let second = if is_wide(first) { half(2)? } else { 0 };
    Some((first, second, decode::decode(address, first, second)?))
}

#[cfg(test)]
mod tests {
    use object::Endianness::Little;

    use super::{Flags, add, holds, subtract, walk};
    use crate::arch::startup::{DEPTH, Event, How, Program, Start};

    /// Where the code of each test is, as in flash.
    const FLASH: u64 = 0x0800_0000;

    /// Code in flash from 0x8000000, made of 16-bit units as a disassembler
    /// shows them: a 32-bit instruction as its first 16 bits, then the
    /// second, and a word as its low half, then the high. Flash holds the
    /// same at reset.
    struct Flash {
        code: Vec<u8>,
        /// Where each function starts and ends.
        functions: Vec<(u64, u64)>,
    }

    impl Program for Flash {
        fn code(&self, address: u64) -> Option<&[u8]> {
            let offset = usize::try_from(address.checked_sub(FLASH)?).ok()?;
            self.code.get(offset..).filter(|rest| !rest.is_empty())
        }

        fn word_at_reset(&self, address: u64) -> Option<u32> {
            let bytes = self.code(address)?.get(..4)?;
            Some(u32::from_le_bytes(bytes.try_into().ok()?))
        }

        fn function_at(&self, address: u64) -> Option<(u64, u64)> {
            (self.functions.iter().copied()).find(|&(start, end)| (start..end).contains(&address))
        }
    }

    /// What the walk from 0x8000000 finds in `units`, with the functions
    /// that start at `functions` and run to the next one, and to the end
    /// of the code for the last one: a line for each event, with its depth
    /// first, and one for how the walk ends.
    fn walk_lines(units: &[u16], functions: &[u64], main: Option<u64>) -> Vec<String> {
        walk_lines_following(units, functions, main, DEPTH)
    }

    /// As `walk_lines`, for a walk that follows calls `follow` deep.
    fn walk_lines_following(
        units: &[u16],
        functions: &[u64],
        main: Option<u64>,
        follow: u32,
    ) -> Vec<String> {
        let code: Vec<u8> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
        let end = FLASH + code.len() as u64;
        let ends = functions.iter().skip(1).copied().chain([end]);
        let flash = Flash {
            functions: functions.iter().copied().zip(ends).collect(),
            code,
        };
        let start = Start {
            address: FLASH | 1,
            stack_pointer: Some(0x2000_1000),
            main,
            follow,
        };
        let walk = walk(Little, &flash, &start);
        let number = |number: Option<u32>| number.map_or("?".to_string(), |n| format!("{n:#x}"));
        let mut lines: Vec<String> = (walk.events.iter())
            .map(|event| match event {
                Event::Call(call) => {
                    let how = match call.how {
                        How::Direct => "call".to_string(),
                        How::Register(register) => format!("call through r{register}"),
                        How::Jump => "tail call".to_string(),
                        How::Supervisor => "svc".to_string(),
                    };
                    let arguments: Vec<String> =
                        call.arguments.iter().map(|&n| number(n)).collect();
                    let target = call.target.map_or("?".to_string(), |n| format!("{n:#x}"));
                    let arguments = arguments.join(" ");
                    let condition = match (call.conditional, call.followed) {
                        (true, _) => ", if",
                        (false, false) if call.target.is_some() => ", not followed",
                        _ => "",
                    };
                    let left: Vec<String> = (0..4)
                        .filter(|&n| call.left & 1 << n != 0 && call.arguments[n].is_some())
                        .map(|n| format!(" r{n}"))
                        .collect();
                    let left = match left.is_empty() {
                        true => String::new(),
                        false => format!(", left by a call:{}", left.concat()),
                    };
                    format!(
                        "{} {:#x} {how} {target} ({arguments}){condition}{left}",
                        call.depth, call.site
                    )
                }
                Event::Store(store) => {
                    let place = match (store.address, store.base) {
                        (Some(address), _) => format!("{address:#x}"),
                        (None, Some(base)) => format!("{base:#x} on"),
                        (None, None) => "?".to_string(),
                    };
                    let value = number(store.value);
                    let size = store.size;
                    let condition = if store.conditional { ", if" } else { "" };
                    let (depth, site) = (store.depth, store.site);
                    format!("{depth} {site:#x} store {value} at {place}, {size} bytes{condition}")
                }
                Event::Loop(l) => {
                    let end = l
                        .end
                        .map_or(String::new(), |end| format!(", stops {end:x?}"));
                    format!(
                        "{} {:#x} loop back at {:#x}, from event {}{end}",
                        l.depth, l.head, l.back, l.first_event
                    )
                }
                Event::Backtrack {
                    site,
                    target,
                    because,
                } => format!("0 {site:#x} back to {target:#x}, since {because:x?}"),
                Event::Stop { site, depth, end } => format!("{depth} {site:#x} stop: {end:x?}"),
            })
            .collect();
        lines.push(format!("end {:x?}", walk.end));
        lines
    }

    #[test]
    fn a_copy_loop_and_a_call_to_zero_memory() {
        // The loop is entered at its test, at the end of its code:
        //   8000000: f04f 5000  mov.w r0, #0x20000000
        //   8000004: 4908       ldr   r1, [pc, #32]  ; 0x20000010
        //   8000006: 4a09       ldr   r2, [pc, #36]  ; 0x8000100
        //   8000008: 2300       movs  r3, #0
        //   800000a: e002       b.n   8000012
        //   800000c: 58d4       ldr   r4, [r2, r3]
        //   800000e: 50c4       str   r4, [r0, r3]
        //   8000010: 3304       adds  r3, #4
        //   8000012: 18c4       adds  r4, r0, r3
        //   8000014: 428c       cmp   r4, r1
        //   8000016: d3f9       bcc.n 800000c
        //   8000018: 4803       ldr   r0, [pc, #12]  ; 0x20000010
        //   800001a: 2100       movs  r1, #0
        //   800001c: 2220       movs  r2, #32
        //   800001e: f000 f807  bl    8000030 <memset>
        //   8000022: f000 f80c  bl    800003e <main>
        //   8000026: 0000
        //   8000028: 20000010   .word
        //   800002c: 08000100   .word
        // memset:
        //   8000030: 1882       adds  r2, r0, r2
        //   8000032: 4290       cmp   r0, r2
        //   8000034: d002       beq.n 800003c
        //   8000036: f800 1b01  strb.w r1, [r0], #1
        //   800003a: e7fa       b.n   8000032
        //   800003c: 4770       bx    lr
        // main:
        //   800003e: e7fe       b.n   800003e
        let code = [
            0xf04f, 0x5000, 0x4908, 0x4a09, 0x2300, 0xe002, 0x58d4, 0x50c4, 0x3304, 0x18c4, 0x428c,
            0xd3f9, 0x4803, 0x2100, 0x2220, 0xf000, 0xf807, 0xf000, 0xf80c, 0x0000, 0x0010, 0x2000,
            0x0100, 0x0800, 0x1882, 0x4290, 0xd002, 0xf800, 0x1b01, 0xe7fa, 0x4770, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0030, 0x0800_003e];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_003e)),
            [
                "0 0x800000e store ? at 0x20000000 on, 4 bytes",
                "0 0x800000c loop back at 0x8000016, from event 0",
                "0 0x800001e call 0x8000030 (0x20000010 0x0 0x20 ?)",
                "1 0x8000036 store ? at 0x20000010, 1 bytes",
                "1 0x8000032 loop back at 0x800003a, from event 3",
                "0 0x8000022 call 0x800003e (? 0x0 0x20000030 ?), left by a call: r2",
                "end Main(8000022)",
            ]
        );
    }

    #[test]
    fn it_blocks_run_as_the_flags_say() {
        // Where the walk can tell the flags, an instruction in an `it`
        // block runs or not; where it can't, what it writes is unknown, and
        // a store is conditional:
        //   8000000: 2210       movs  r2, #16
        //   8000002: 2a20       cmp   r2, #32
        //   8000004: bf88       it    hi
        //   8000006: 2200       movhi r2, #0
        //   8000008: bf94       ite   ls
        //   800000a: 2101       movls r1, #1
        //   800000c: 2102       movhi r1, #2
        //   800000e: f04f 4080  mov.w r0, #0x40000000
        //   8000012: 6803       ldr   r3, [r0, #0]
        //   8000014: 2b00       cmp   r3, #0
        //   8000016: bf04       itt   eq
        //   8000018: 2230       moveq r2, #48
        //   800001a: 6041       streq r1, [r0, #4]
        //   800001c: f000 f802  bl    8000024 <func>
        //   8000020: f000 f801  bl    8000026 <main>
        // func:
        //   8000024: 4770       bx    lr
        // main:
        //   8000026: e7fe       b.n   8000026
        let code = [
            0x2210, 0x2a20, 0xbf88, 0x2200, 0xbf94, 0x2101, 0x2102, 0xf04f, 0x4080, 0x6803, 0x2b00,
            0xbf04, 0x2230, 0x6041, 0xf000, 0xf802, 0xf000, 0xf801, 0x4770, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0024, 0x0800_0026];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0026)),
            [
                "0 0x800001a store 0x1 at 0x40000004, 4 bytes, if",
                "0 0x800001c call 0x8000024 (0x40000000 0x1 ? ?)",
                "0 0x8000020 call 0x8000026 (0x40000000 0x1 ? ?)",
                "end Main(8000020)",
            ]
        );
    }

    #[test]
    fn a_compare_that_skips_a_loop_for_an_empty_section() {
        // The first loop is skipped, since r0 and r1 hold the same address;
        // the second runs:
        //   8000000: f04f 5000  mov.w r0, #0x20000000
        //   8000004: f04f 5100  mov.w r1, #0x20000000
        //   8000008: 2200       movs  r2, #0
        //   800000a: 4288       cmp   r0, r1
        //   800000c: d203       bcs.n 8000016
        //   800000e: f840 2b04  str.w r2, [r0], #4
        //   8000012: 4288       cmp   r0, r1
        //   8000014: d3fb       bcc.n 800000e
        //   8000016: f04f 5000  mov.w r0, #0x20000000
        //   800001a: 4904       ldr   r1, [pc, #16]  ; 0x20000008
        //   800001c: 4288       cmp   r0, r1
        //   800001e: d203       bcs.n 8000028
        //   8000020: f840 2b04  str.w r2, [r0], #4
        //   8000024: 4288       cmp   r0, r1
        //   8000026: d3fb       bcc.n 8000020
        //   8000028: f000 f802  bl    8000030 <main>
        //   800002c: 20000008   .word
        // main:
        //   8000030: e7fe       b.n   8000030
        let code = [
            0xf04f, 0x5000, 0xf04f, 0x5100, 0x2200, 0x4288, 0xd203, 0xf840, 0x2b04, 0x4288, 0xd3fb,
            0xf04f, 0x5000, 0x4904, 0x4288, 0xd203, 0xf840, 0x2b04, 0x4288, 0xd3fb, 0xf000, 0xf802,
            0x0008, 0x2000, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0030];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0030)),
            [
                "0 0x8000020 store 0x0 at 0x20000000, 4 bytes",
                "0 0x8000020 loop back at 0x8000026, from event 0",
                "0 0x8000028 call 0x8000030 (? 0x20000008 0x0 ?)",
                "end Main(8000028)",
            ]
        );
    }

    #[test]
    fn a_tail_call_goes_on_at_the_depth_of_the_function_that_makes_it() {
        // The reset handler goes on after a call with what the function
        // left in the registers:
        //   8000000: f000 f801  bl    8000006 <init>
        //   8000004: e005       b.n   8000012 <main>
        // init:
        //   8000006: f04f 5100  mov.w r1, #0x20000000
        //   800000a: 2001       movs  r0, #1
        //   800000c: e7ff       b.n   800000e <helper>
        // helper:
        //   800000e: 6008       str   r0, [r1, #0]
        //   8000010: 4770       bx    lr
        // main:
        //   8000012: e7fe       b.n   8000012
        let code = [
            0xf000, 0xf801, 0xe005, 0xf04f, 0x5100, 0x2001, 0xe7ff, 0x6008, 0x4770, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0006, 0x0800_000e, 0x0800_0012];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0012)),
            [
                "0 0x8000000 call 0x8000006 (? ? ? ?)",
                "1 0x800000c tail call 0x800000e (0x1 0x20000000 ? ?)",
                "1 0x800000e store 0x1 at 0x20000000, 4 bytes",
                "0 0x8000004 tail call 0x8000012 (0x1 0x20000000 ? ?), left by a call: r0 r1",
                "end Main(8000004)",
            ]
        );
    }

    #[test]
    fn a_call_that_doesnt_return_runs_past_the_end_of_its_function() {
        //   8000000: 2000       movs  r0, #0
        //   8000002: f000 f800  bl    8000006 <fatal>
        // fatal:
        //   8000006: e7fe       b.n   8000006
        let code = [0x2000, 0xf000, 0xf800, 0xe7fe];
        let functions = [0x0800_0000, 0x0800_0006];
        assert_eq!(
            walk_lines(&code, &functions, None),
            [
                "0 0x8000002 call 0x8000006 (0x0 ? ? ?)",
                "end RunsOff(8000006)"
            ]
        );
    }

    #[test]
    fn a_call_that_doesnt_return_is_followed_by_udf_or_data() {
        //   8000000: f000 f801  bl    8000006 <fatal>
        //   8000004: de00       udf   #0
        // fatal:
        //   8000006: e7fe       b.n   8000006
        let code = [0xf000, 0xf801, 0xde00, 0xe7fe];
        let functions = [0x0800_0000, 0x0800_0006];
        assert_eq!(
            walk_lines(&code, &functions, None),
            [
                "0 0x8000000 call 0x8000006 (? ? ? ?)",
                "end RunsOff(8000004)"
            ]
        );
        // So does a bkpt, as some compilers put after a call to abort, but
        // not bkpt 0xab, which asks a debugger to do semihosting, and after
        // which the walk goes on, here into fatal's code:
        //   8000004: be00       bkpt  0x0000
        let code = [0xf000, 0xf801, 0xbe00, 0xe7fe];
        assert_eq!(
            walk_lines(&code, &functions, None),
            [
                "0 0x8000000 call 0x8000006 (? ? ? ?)",
                "end RunsOff(8000004)"
            ]
        );
        //   8000004: beab       bkpt  0x00ab
        let code = [0xf000, 0xf801, 0xbeab, 0xe7fe];
        assert_eq!(
            walk_lines(&code, &functions, None),
            [
                "0 0x8000000 call 0x8000006 (? ? ? ?)",
                "end RunsOff(8000006)"
            ]
        );
        // The walk doesn't run the literal pool's first word, a store, as
        // code:
        //   8000000: 4901       ldr   r1, [pc, #4]  ; 0x12346000
        //   8000002: 4802       ldr   r0, [pc, #8]  ; fatal
        //   8000004: 4780       blx   r0
        //   8000006: bf00       nop
        //   8000008: 12346000   .word
        //   800000c: 08000011   .word
        // fatal:
        //   8000010: e7fe       b.n   8000010
        let code = [
            0x4901, 0x4802, 0x4780, 0xbf00, 0x6000, 0x1234, 0x0011, 0x0800, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0010];
        assert_eq!(
            walk_lines(&code, &functions, None),
            [
                "0 0x8000004 call through r0 0x8000010 (0x8000011 0x12346000 ? ?)",
                "end RunsOff(8000008)"
            ]
        );
    }

    #[test]
    fn a_veneer_that_pops_the_address_it_goes_to_into_pc() {
        // LLD's veneer for Cortex-M0, which has no ip to spare: the walk
        // goes on to init, with r0 as the reset handler left it.
        //   8000000: 2005       movs  r0, #5
        //   8000002: f000 f802  bl    800000a <veneer>
        //   8000006: f000 f808  bl    800001a <main>
        // veneer:
        //   800000a: b403       push  {r0, r1}
        //   800000c: 4801       ldr   r0, [pc, #4]  ; init + 1
        //   800000e: 9001       str   r0, [sp, #4]
        //   8000010: bd01       pop   {r0, pc}
        //   8000012: bf00       nop
        //   8000014: 08000019   .word
        // init:
        //   8000018: 4770       bx    lr
        // main:
        //   800001a: e7fe       b.n   800001a
        let code = [
            0x2005, 0xf000, 0xf802, 0xf000, 0xf808, 0xb403, 0x4801, 0x9001, 0xbd01, 0xbf00, 0x0019,
            0x0800, 0x4770, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_000a, 0x0800_0018, 0x0800_001a];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_001a)),
            [
                "0 0x8000002 call 0x800000a (0x5 ? ? ?)",
                "1 0x8000010 tail call 0x8000018 (0x5 ? ? ?)",
                "0 0x8000006 call 0x800001a (0x5 ? ? ?), left by a call: r0",
                "end Main(8000006)"
            ]
        );
    }

    #[test]
    fn a_loop_entered_at_its_test_that_runs_no_times() {
        // GCC -O0 jumps to a loop's test, which here finds no functions in
        // an empty table to call:
        //   8000000: 4c04       ldr   r4, [pc, #16]  ; 0x8000100
        //   8000002: 4d04       ldr   r5, [pc, #16]  ; 0x8000100
        //   8000004: e002       b.n   800000c
        //   8000006: 6820       ldr   r0, [r4, #0]
        //   8000008: 4780       blx   r0
        //   800000a: 3404       adds  r4, #4
        //   800000c: 42ac       cmp   r4, r5
        //   800000e: d1fa       bne.n 8000006
        //   8000010: f000 f802  bl    8000018 <main>
        //   8000014: 08000100   .word
        // main:
        //   8000018: e7fe       b.n   8000018
        let code = [
            0x4c04, 0x4d04, 0xe002, 0x6820, 0x4780, 0x3404, 0x42ac, 0xd1fa, 0xf000, 0xf802, 0x0100,
            0x0800, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0018];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0018)),
            ["0 0x8000010 call 0x8000018 (? ? ? ?)", "end Main(8000010)"]
        );
    }

    #[test]
    fn a_length_rounded_to_words_with_bics() {
        // As Cortex-M0 code does, which has no bic with an immediate:
        //   8000000: f04f 5000  mov.w r0, #0x20000000
        //   8000004: 4a04       ldr   r2, [pc, #16]  ; 0x20000107
        //   8000006: 1a12       subs  r2, r2, r0
        //   8000008: 3203       adds  r2, #3
        //   800000a: 2103       movs  r1, #3
        //   800000c: 438a       bics  r2, r1
        //   800000e: 2100       movs  r1, #0
        //   8000010: f000 f804  bl    800001c <memset>
        //   8000014: f000 f803  bl    800001e <main>
        //   8000018: 20000107   .word
        // memset:
        //   800001c: 4770       bx    lr
        // main:
        //   800001e: e7fe       b.n   800001e
        let code = [
            0xf04f, 0x5000, 0x4a04, 0x1a12, 0x3203, 0x2103, 0x438a, 0x2100, 0xf000, 0xf804, 0xf000,
            0xf803, 0x0107, 0x2000, 0x4770, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_001c, 0x0800_001e];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_001e)),
            [
                "0 0x8000010 call 0x800001c (0x20000000 0x0 0x108 ?)",
                "0 0x8000014 call 0x800001e (0x20000000 0x0 0x108 ?)",
                "end Main(8000014)"
            ]
        );
    }

    #[test]
    fn a_call_leaves_what_its_code_uses_in_r0_to_r3() {
        // init doesn't use r0, so it still holds what the reset handler
        // set for copy, as code built with GCC's -fipa-ra counts on; copy
        // reads r0 and writes r3, which main then finds there:
        //   8000000: 4803       ldr   r0, [pc, #12]  ; 0x20000000
        //   8000002: f000 f807  bl    8000014 <init>
        //   8000006: 2210       movs  r2, #16
        //   8000008: f000 f806  bl    8000018 <copy>
        //   800000c: f000 f806  bl    800001c <main>
        //   8000010: 20000000   .word
        // init:
        //   8000014: 2301       movs  r3, #1
        //   8000016: 4770       bx    lr
        // copy:
        //   8000018: 4603       mov   r3, r0
        //   800001a: 4770       bx    lr
        // main:
        //   800001c: e7fe       b.n   800001c
        let code = [
            0x4803, 0xf000, 0xf807, 0x2210, 0xf000, 0xf806, 0xf000, 0xf806, 0x0000, 0x2000, 0x2301,
            0x4770, 0x4603, 0x4770, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0014, 0x0800_0018, 0x0800_001c];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_001c)),
            [
                "0 0x8000002 call 0x8000014 (0x20000000 ? ? ?)",
                "0 0x8000008 call 0x8000018 (0x20000000 ? 0x10 0x1), left by a call: r3",
                "0 0x800000c call 0x800001c (0x20000000 ? 0x10 0x20000000), left by a call: r0 r3",
                "end Main(800000c)"
            ]
        );
    }

    #[test]
    fn a_register_that_a_call_only_reads_isnt_left_by_it() {
        // check reads r1 and writes r2, so main finds the 5 the reset
        // handler put in r1, and 6 in r2 that check left:
        //   8000000: 2105       movs  r1, #5
        //   8000002: f000 f803  bl    800000c <check>
        //   8000006: f000 f803  bl    8000010 <main>
        //   800000a: bf00       nop
        // check:
        //   800000c: 1c4a       adds  r2, r1, #1
        //   800000e: 4770       bx    lr
        // main:
        //   8000010: e7fe       b.n   8000010
        let code = [
            0x2105, 0xf000, 0xf803, 0xf000, 0xf803, 0xbf00, 0x1c4a, 0x4770, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_000c, 0x0800_0010];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0010)),
            [
                "0 0x8000002 call 0x800000c (? 0x5 ? ?)",
                "0 0x8000006 call 0x8000010 (? 0x5 0x6 ?), left by a call: r2",
                "end Main(8000006)"
            ]
        );
    }

    #[test]
    fn a_tail_call_to_a_dead_end_takes_the_other_way_without_following_calls() {
        // As in Pico SDK's crt0, core 1 tail-calls code that branches
        // through a register, and core 0 takes the beq. A walk that doesn't
        // follow calls still looks where the tail call goes, to go back
        // to the beq:
        //   8000000: 6800       ldr   r0, [r0, #0]
        //   8000002: 2800       cmp   r0, #0
        //   8000004: d001       beq.n 800000a
        //   8000006: 2000       movs  r0, #0
        //   8000008: e003       b.n   8000012 <enter>
        //   800000a: f000 f801  bl    8000010 <main>
        //   800000e: e7fe       b.n   800000e
        // main:
        //   8000010: e7fe       b.n   8000010
        // enter:
        //   8000012: 4710       bx    r2
        let code = [
            0x6800, 0x2800, 0xd001, 0x2000, 0xe003, 0xf000, 0xf801, 0xe7fe, 0xe7fe, 0x4710,
        ];
        let functions = [0x0800_0000, 0x0800_0010, 0x0800_0012];
        assert_eq!(
            walk_lines_following(&code, &functions, None, 0),
            [
                "0 0x8000004 back to 0x800000a, since Branches { site: 8000012, through: Register(2) }",
                "0 0x800000a call 0x8000010 (? ? ? ?), not followed",
                "end Spins(800000e)"
            ]
        );
    }

    #[test]
    fn the_walk_goes_through_so_many_instructions() {
        // 700 nops in the reset handler.
        let code = [0xbf00; 700];
        assert_eq!(walk_lines(&code, &[FLASH], None), ["end Budget(80004b0)"]);
        // A call to a function of 403 instructions, which the walk leaves
        // after 300 for the code after the call, so it doesn't list the
        // second store:
        //   8000000: f000 f803  bl    800000a <slow>
        //   8000004: f000 f800  bl    8000008 <main>
        // main:
        //   8000008: e7fe       b.n   8000008
        // slow:
        //   800000a: bf00       nop
        //   ...
        //   800025e: 6000       str   r0, [r0, #0]
        //   8000260: bf00       nop
        //   ...
        //   800032c: 6000       str   r0, [r0, #0]
        //   800032e: 4770       bx    lr
        let mut code = vec![0xf000, 0xf803, 0xf000, 0xf800, 0xe7fe];
        code.extend([0xbf00; 298]);
        code.push(0x6000);
        code.extend([0xbf00; 102]);
        code.extend([0x6000, 0x4770]);
        let functions = [0x0800_0000, 0x0800_0008, 0x0800_000a];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0008)),
            [
                "0 0x8000000 call 0x800000a (? ? ? ?)",
                "1 0x800025e store ? at ?, 4 bytes",
                "1 0x8000000 stop: Budget(8000262)",
                "0 0x8000004 call 0x8000008 (? ? ? ?)",
                "end Main(8000004)",
            ]
        );
    }

    #[test]
    fn a_loop_body_after_the_function_s_other_code() {
        // As GCC -Os puts it, when the walk can't tell whether the loop
        // runs:
        //   8000000: f04f 4380  mov.w r3, #0x40000000
        //   8000004: 6819       ldr   r1, [r3, #0]
        //   8000006: f04f 5000  mov.w r0, #0x20000000
        //   800000a: 2200       movs  r2, #0
        //   800000c: 4288       cmp   r0, r1
        //   800000e: d301       bcc.n 8000014
        //   8000010: f000 f803  bl    800001a <main>
        //   8000014: f840 2b04  str.w r2, [r0], #4
        //   8000018: e7f8       b.n   800000c
        // main:
        //   800001a: e7fe       b.n   800001a
        let code = [
            0xf04f, 0x4380, 0x6819, 0xf04f, 0x5000, 0x2200, 0x4288, 0xd301, 0xf000, 0xf803, 0xf840,
            0x2b04, 0xe7f8, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_001a];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_001a)),
            [
                "0 0x8000014 store 0x0 at 0x20000000, 4 bytes",
                "0 0x8000014 loop back at 0x8000018, from event 0",
                "0 0x8000010 call 0x800001a (? ? 0x0 0x40000000)",
                "end Main(8000010)",
            ]
        );
    }

    #[test]
    fn a_pointer_on_the_stack_that_two_loops_share() {
        // As GCC -O0 builds `for (p = &_sdata; p < &_edata;) *p++ = 1;
        // for (; p < &_ebss;) *p++ = 0;`: the second loop starts where the
        // first ends, which the walk can't tell, not at 0x20000000. Stores
        // through r7 to the stack aren't listed:
        //   8000000: b580       push  {r7, lr}
        //   8000002: b082       sub   sp, #8
        //   8000004: af00       add   r7, sp, #0
        //   8000006: f04f 5300  mov.w r3, #0x20000000
        //   800000a: 607b       str   r3, [r7, #4]
        //   800000c: e004       b.n   8000018
        //   800000e: 687b       ldr   r3, [r7, #4]
        //   8000010: 1d1a       adds  r2, r3, #4
        //   8000012: 607a       str   r2, [r7, #4]
        //   8000014: 2201       movs  r2, #1
        //   8000016: 601a       str   r2, [r3, #0]
        //   8000018: 687b       ldr   r3, [r7, #4]
        //   800001a: 4a07       ldr   r2, [pc, #28]  ; 0x20000010
        //   800001c: 4293       cmp   r3, r2
        //   800001e: d3f6       bcc.n 800000e
        //   8000020: e004       b.n   800002c
        //   8000022: 687b       ldr   r3, [r7, #4]
        //   8000024: 1d1a       adds  r2, r3, #4
        //   8000026: 607a       str   r2, [r7, #4]
        //   8000028: 2200       movs  r2, #0
        //   800002a: 601a       str   r2, [r3, #0]
        //   800002c: 687b       ldr   r3, [r7, #4]
        //   800002e: 4a03       ldr   r2, [pc, #12]  ; 0x20000020
        //   8000030: 4293       cmp   r3, r2
        //   8000032: d3f6       bcc.n 8000022
        //   8000034: f000 f804  bl    8000040 <main>
        //   8000038: 20000010   .word
        //   800003c: 20000020   .word
        // main:
        //   8000040: e7fe       b.n   8000040
        let code = [
            0xb580, 0xb082, 0xaf00, 0xf04f, 0x5300, 0x607b, 0xe004, 0x687b, 0x1d1a, 0x607a, 0x2201,
            0x601a, 0x687b, 0x4a07, 0x4293, 0xd3f6, 0xe004, 0x687b, 0x1d1a, 0x607a, 0x2200, 0x601a,
            0x687b, 0x4a03, 0x4293, 0xd3f6, 0xf000, 0xf804, 0x0010, 0x2000, 0x0020, 0x2000, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0040];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0040)),
            [
                "0 0x8000016 store 0x1 at 0x20000000, 4 bytes",
                "0 0x800000e loop back at 0x800001e, from event 0",
                "0 0x800002a store 0x0 at ?, 4 bytes",
                "0 0x8000022 loop back at 0x8000032, from event 2",
                "0 0x8000034 call 0x8000040 (? ? 0x20000020 ?)",
                "end Main(8000034)",
            ]
        );
    }

    #[test]
    fn a_branch_the_walk_cant_tell_is_taken_when_the_other_way_branches_through_a_register() {
        // A check of which core runs the code, whose fall-through goes
        // where the walk can't follow; what it found that way is dropped:
        //   8000000: 4808       ldr   r0, [pc, #32]  ; 0xe000ed00
        //   8000002: 6800       ldr   r0, [r0, #0]
        //   8000004: f04f 5100  mov.w r1, #0x20000000
        //   8000008: 0900       lsrs  r0, r0, #4
        //   800000a: 28c2       cmp   r0, #194
        //   800000c: d005       beq.n 800001a
        //   800000e: 2201       movs  r2, #1
        //   8000010: 600a       str   r2, [r1, #0]
        //   8000012: f04f 4280  mov.w r2, #0x40000000
        //   8000016: 6812       ldr   r2, [r2, #0]
        //   8000018: 4710       bx    r2
        //   800001a: 2202       movs  r2, #2
        //   800001c: 600a       str   r2, [r1, #0]
        //   800001e: f000 f803  bl    8000028 <main>
        //   8000022: 0000
        //   8000024: e000ed00   .word
        // main:
        //   8000028: e7fe       b.n   8000028
        let code = [
            0x4808, 0x6800, 0xf04f, 0x5100, 0x0900, 0x28c2, 0xd005, 0x2201, 0x600a, 0xf04f, 0x4280,
            0x6812, 0x4710, 0x2202, 0x600a, 0xf000, 0xf803, 0x0000, 0xed00, 0xe000, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0028];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0028)),
            [
                "0 0x800000c back to 0x800001a, since Branches { site: 8000018, through: Register(2) }",
                "0 0x800001c store 0x2 at 0x20000000, 4 bytes",
                "0 0x800001e call 0x8000028 (? 0x20000000 0x2 ?)",
                "end Main(800001e)",
            ]
        );
    }

    #[test]
    fn a_region_table_loop_that_calls_through_a_register() {
        // As Arm Compiler 6's __scatterload goes through its table of
        // regions, calling a function for each to copy or zero it, which
        // the walk can't tell:
        //   8000000: a404       adr   r4, 8000014 <table>
        //   8000002: a508       adr   r5, 8000024 <table_end>
        //   8000004: e001       b.n   800000a
        //   8000006: cc0f       ldmia r4!, {r0, r1, r2, r3}
        //   8000008: 4798       blx   r3
        //   800000a: 42ac       cmp   r4, r5
        //   800000c: d3fb       bcc.n 8000006
        //   800000e: f000 f809  bl    8000024 <main>
        //   8000012: bf00       nop
        //   8000014: 08000100   .word
        //   8000018: 20000000   .word
        //   800001c: 00000010   .word
        //   8000020: 08000041   .word
        // main:
        //   8000024: e7fe       b.n   8000024
        let code = [
            0xa404, 0xa508, 0xe001, 0xcc0f, 0x4798, 0x42ac, 0xd3fb, 0xf000, 0xf809, 0xbf00, 0x0100,
            0x0800, 0x0000, 0x2000, 0x0010, 0x0000, 0x0041, 0x0800, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0024];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0024)),
            [
                "0 0x8000008 call through r3 ? (? ? ? ?)",
                "0 0x8000006 loop back at 0x800000c, from event 0",
                "0 0x800000e call 0x8000024 (? ? ? ?)",
                "end Main(800000e)",
            ]
        );
    }

    #[test]
    fn a_table_of_functions_read_with_a_load_that_moves_its_base() {
        // As newlib's __libc_init_array calls the functions in .init_array.
        // main's r0 is what init left there:
        //   8000000: 4d03       ldr   r5, [pc, #12]  ; 0x8000010
        //   8000002: f855 3b04  ldr.w r3, [r5], #4
        //   8000006: 4798       blx   r3
        //   8000008: 4629       mov   r1, r5
        //   800000a: f000 f807  bl    800001c <main>
        //   800000e: bf00       nop
        //   8000010: 08000014   .word  ; the table
        //   8000014: 08000019   .word  ; init + 1
        // init:
        //   8000018: 2001       movs  r0, #1
        //   800001a: 4770       bx    lr
        // main:
        //   800001c: e7fe       b.n   800001c
        let code = [
            0x4d03, 0xf855, 0x3b04, 0x4798, 0x4629, 0xf000, 0xf807, 0xbf00, 0x0014, 0x0800, 0x0019,
            0x0800, 0x2001, 0x4770, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0018, 0x0800_001c];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_001c)),
            [
                "0 0x8000006 call through r3 0x8000018 (? ? ? 0x8000019)",
                "0 0x800000a call 0x800001c (0x1 0x8000018 ? 0x8000019), left by a call: r0",
                "end Main(800000a)",
            ]
        );
    }

    #[test]
    fn calls_are_followed_three_deep() {
        // d, four calls deep, isn't followed, so its store isn't listed. A
        // function the walk can't follow to its end stops there:
        //   8000000: f000 f804  bl    800000c <a>
        //   8000004: f000 f812  bl    800002c <broken>
        //   8000008: f000 f811  bl    800002e <main>
        // a:
        //   800000c: b510       push  {r4, lr}
        //   800000e: f000 f801  bl    8000014 <b>
        //   8000012: bd10       pop   {r4, pc}
        // b:
        //   8000014: b510       push  {r4, lr}
        //   8000016: f000 f801  bl    800001c <c>
        //   800001a: bd10       pop   {r4, pc}
        // c:
        //   800001c: b510       push  {r4, lr}
        //   800001e: f000 f801  bl    8000024 <d>
        //   8000022: bd10       pop   {r4, pc}
        // d:
        //   8000024: f04f 5000  mov.w r0, #0x20000000
        //   8000028: 6000       str   r0, [r0, #0]
        //   800002a: 4770       bx    lr
        // broken:
        //   800002c: de00       udf   #0
        // main:
        //   800002e: e7fe       b.n   800002e
        let code = [
            0xf000, 0xf804, 0xf000, 0xf812, 0xf000, 0xf811, 0xb510, 0xf000, 0xf801, 0xbd10, 0xb510,
            0xf000, 0xf801, 0xbd10, 0xb510, 0xf000, 0xf801, 0xbd10, 0xf04f, 0x5000, 0x6000, 0x4770,
            0xde00, 0xe7fe,
        ];
        let functions = [
            0x0800_0000,
            0x0800_000c,
            0x0800_0014,
            0x0800_001c,
            0x0800_0024,
            0x0800_002c,
            0x0800_002e,
        ];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_002e)),
            [
                "0 0x8000000 call 0x800000c (? ? ? ?)",
                "1 0x800000e call 0x8000014 (? ? ? ?)",
                "2 0x8000016 call 0x800001c (? ? ? ?)",
                "3 0x800001e call 0x8000024 (? ? ? ?), not followed",
                "0 0x8000004 call 0x800002c (? ? ? ?)",
                "1 0x8000004 stop: Undecoded(800002c)",
                "0 0x8000008 call 0x800002e (? ? ? ?)",
                "end Main(8000008)",
            ]
        );
    }

    #[test]
    fn loads_from_flash_from_memory_the_code_wrote_and_from_ram() {
        // Flash holds its words at reset, and RAM holds nothing the walk
        // can tell until code stores there:
        //   8000000: 4805       ldr   r0, [pc, #20]  ; 0x8000020 <table>
        //   8000002: 6801       ldr   r1, [r0, #0]
        //   8000004: 6842       ldr   r2, [r0, #4]
        //   8000006: 4b05       ldr   r3, [pc, #20]  ; 0x20000004
        //   8000008: 601a       str   r2, [r3, #0]
        //   800000a: 681b       ldr   r3, [r3, #0]
        //   800000c: f04f 5400  mov.w r4, #0x20000000
        //   8000010: 6824       ldr   r4, [r4, #0]
        //   8000012: 608c       str   r4, [r1, #8]
        //   8000014: f000 f808  bl    8000028 <main>
        //   8000018: 08000020   .word
        //   800001c: 20000004   .word
        // table:
        //   8000020: 20000000   .word
        //   8000024: 00000100   .word
        // main:
        //   8000028: e7fe       b.n   8000028
        let code = [
            0x4805, 0x6801, 0x6842, 0x4b05, 0x601a, 0x681b, 0xf04f, 0x5400, 0x6824, 0x608c, 0xf000,
            0xf808, 0x0020, 0x0800, 0x0004, 0x2000, 0x0000, 0x2000, 0x0100, 0x0000, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0028];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0028)),
            [
                "0 0x8000008 store 0x100 at 0x20000004, 4 bytes",
                "0 0x8000012 store ? at 0x20000008, 4 bytes",
                "0 0x8000014 call 0x8000028 (0x8000020 0x20000000 0x100 0x100)",
                "end Main(8000014)",
            ]
        );
    }

    #[test]
    fn the_walk_ends_where_it_gets_to_main_s_code() {
        // As where a compiler inlined main into the reset handler:
        //   8000000: 2000       movs  r0, #0
        //   8000002: e7fe       b.n   8000002
        assert_eq!(
            walk_lines(&[0x2000, 0xe7fe], &[FLASH], Some(0x0800_0002)),
            ["end Main(8000002)"]
        );
    }

    #[test]
    fn a_call_returns_what_the_function_left_in_registers_and_memory() {
        // A result in r0 and r1, a word stored through a pointer to the
        // caller's stack, and one stored in RAM:
        //   8000000: b082       sub   sp, #8
        //   8000002: 4668       mov   r0, sp
        //   8000004: f000 f806  bl    8000014 <divmod>
        //   8000008: 9b00       ldr   r3, [sp, #0]
        //   800000a: f04f 5200  mov.w r2, #0x20000000
        //   800000e: 6812       ldr   r2, [r2, #0]
        //   8000010: f000 f809  bl    8000026 <main>
        // divmod:
        //   8000014: 2109       movs  r1, #9
        //   8000016: 6001       str   r1, [r0, #0]
        //   8000018: f04f 5000  mov.w r0, #0x20000000
        //   800001c: 2107       movs  r1, #7
        //   800001e: 6001       str   r1, [r0, #0]
        //   8000020: 2002       movs  r0, #2
        //   8000022: 2103       movs  r1, #3
        //   8000024: 4770       bx    lr
        // main:
        //   8000026: e7fe       b.n   8000026
        let code = [
            0xb082, 0x4668, 0xf000, 0xf806, 0x9b00, 0xf04f, 0x5200, 0x6812, 0xf000, 0xf809, 0x2109,
            0x6001, 0xf04f, 0x5000, 0x2107, 0x6001, 0x2002, 0x2103, 0x4770, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0014, 0x0800_0026];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0026)),
            [
                "0 0x8000004 call 0x8000014 (0x20000ff8 ? ? ?)",
                "1 0x800001e store 0x7 at 0x20000000, 4 bytes",
                "0 0x8000010 call 0x8000026 (0x2 0x3 0x7 0x9), left by a call: r0 r1",
                "end Main(8000010)",
            ]
        );
    }

    #[test]
    fn a_call_in_an_it_block_isnt_followed_and_changes_only_what_it_writes() {
        //   8000000: 2101       movs  r1, #1
        //   8000002: 2202       movs  r2, #2
        //   8000004: 2303       movs  r3, #3
        //   8000006: 2800       cmp   r0, #0
        //   8000008: bf08       it    eq
        //   800000a: f000 f802  bleq  8000012 <set_r0>
        //   800000e: f000 f802  bl    8000016 <main>
        // set_r0:
        //   8000012: 2005       movs  r0, #5
        //   8000014: 4770       bx    lr
        // main:
        //   8000016: e7fe       b.n   8000016
        let code = [
            0x2101, 0x2202, 0x2303, 0x2800, 0xbf08, 0xf000, 0xf802, 0xf000, 0xf802, 0x2005, 0x4770,
            0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0012, 0x0800_0016];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0016)),
            [
                "0 0x800000a call 0x8000012 (? 0x1 0x2 0x3), if",
                "0 0x800000e call 0x8000016 (? 0x1 0x2 0x3)",
                "end Main(800000e)",
            ]
        );
    }

    #[test]
    fn a_branch_to_another_function_is_a_tail_call_when_taken() {
        // first branches to second where the walk can't tell, so what it
        // changes is unknown after the call; third always does:
        //   8000000: 2202       movs  r2, #2
        //   8000002: f000 f804  bl    800000e <first>
        //   8000006: f000 f80b  bl    8000020 <third>
        //   800000a: f000 f80c  bl    8000026 <main>
        // first:
        //   800000e: 2100       movs  r1, #0
        //   8000010: 2900       cmp   r1, #0
        //   8000012: d103       bne.n 800001c <second>
        //   8000014: 2800       cmp   r0, #0
        //   8000016: d001       beq.n 800001c <second>
        //   8000018: 2203       movs  r2, #3
        //   800001a: 4770       bx    lr
        // second:
        //   800001c: 2301       movs  r3, #1
        //   800001e: 4770       bx    lr
        // third:
        //   8000020: 2100       movs  r1, #0
        //   8000022: 2900       cmp   r1, #0
        //   8000024: d0fa       beq.n 800001c <second>
        // main:
        //   8000026: e7fe       b.n   8000026
        let code = [
            0x2202, 0xf000, 0xf804, 0xf000, 0xf80b, 0xf000, 0xf80c, 0x2100, 0x2900, 0xd103, 0x2800,
            0xd001, 0x2203, 0x4770, 0x2301, 0x4770, 0x2100, 0x2900, 0xd0fa, 0xe7fe,
        ];
        let functions = [
            0x0800_0000,
            0x0800_000e,
            0x0800_001c,
            0x0800_0020,
            0x0800_0026,
        ];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0026)),
            [
                "0 0x8000002 call 0x800000e (? ? 0x2 ?)",
                "1 0x8000016 tail call 0x800001c (? 0x0 0x2 ?), if",
                "0 0x8000006 call 0x8000020 (? ? ? ?)",
                "1 0x8000024 tail call 0x800001c (? 0x0 ? ?)",
                "0 0x800000a call 0x8000026 (? 0x0 ? 0x1), left by a call: r1 r3",
                "end Main(800000a)",
            ]
        );
    }

    #[test]
    fn a_loop_whose_code_is_too_long_forgets_what_it_writes() {
        // 70 adds in the loop's code, more than the walk goes through:
        //   8000000: 2101       movs  r1, #1
        //   8000002: 2202       movs  r2, #2
        //   8000004: f04f 5000  mov.w r0, #0x20000000
        //   8000008: e046       b.n   8000098
        //   800000a: 3101       adds  r1, #1
        //   ...
        //   8000094: 3101       adds  r1, #1
        //   8000096: 6001       str   r1, [r0, #0]
        //   8000098: 6843       ldr   r3, [r0, #4]
        //   800009a: 2b00       cmp   r3, #0
        //   800009c: d1b5       bne.n 800000a
        //   800009e: f000 f800  bl    80000a2 <main>
        // main:
        //   80000a2: e7fe       b.n   80000a2
        let mut code = vec![0x2101, 0x2202, 0xf04f, 0x5000, 0xe046];
        code.extend([0x3101; 70]);
        code.extend([0x6001, 0x6843, 0x2b00, 0xd1b5, 0xf000, 0xf800, 0xe7fe]);
        let functions = [0x0800_0000, 0x0800_00a2];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_00a2)),
            [
                "0 0x800000a loop back at 0x800009c, from event 0, stops Budget(8000082)",
                "0 0x800009e call 0x80000a2 (0x20000000 ? 0x2 ?)",
                "end Main(800009e)",
            ]
        );
    }

    #[test]
    fn loops_inside_loops_are_walked_8_deep() {
        // Nine loops, each entered at its test:
        //   8000000: e018       b.n   8000034
        //   8000002: e015       b.n   8000030
        //   ...
        //   8000010: e000       b.n   8000014
        //   8000012: bf00       nop
        //   8000014: 2800       cmp   r0, #0
        //   8000016: d1fc       bne.n 8000012
        //   8000018: 2800       cmp   r0, #0
        //   800001a: d1f9       bne.n 8000010
        //   ...
        //   8000034: 2800       cmp   r0, #0
        //   8000036: d1e4       bne.n 8000002
        //   8000038: f000 f800  bl    800003c <main>
        // main:
        //   800003c: e7fe       b.n   800003c
        let code = [
            0xe018, 0xe015, 0xe012, 0xe00f, 0xe00c, 0xe009, 0xe006, 0xe003, 0xe000, 0xbf00, 0x2800,
            0xd1fc, 0x2800, 0xd1f9, 0x2800, 0xd1f6, 0x2800, 0xd1f3, 0x2800, 0xd1f0, 0x2800, 0xd1ed,
            0x2800, 0xd1ea, 0x2800, 0xd1e7, 0x2800, 0xd1e4, 0xf000, 0xf800, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_003c];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_003c)),
            [
                "0 0x8000012 loop back at 0x8000016, from event 0, stops Budget(8000012)",
                "0 0x8000010 loop back at 0x800001a, from event 0",
                "0 0x800000e loop back at 0x800001e, from event 0",
                "0 0x800000c loop back at 0x8000022, from event 0",
                "0 0x800000a loop back at 0x8000026, from event 0",
                "0 0x8000008 loop back at 0x800002a, from event 0",
                "0 0x8000006 loop back at 0x800002e, from event 0",
                "0 0x8000004 loop back at 0x8000032, from event 0",
                "0 0x8000002 loop back at 0x8000036, from event 0",
                "0 0x8000038 call 0x800003c (? ? ? ?)",
                "end Main(8000038)",
            ]
        );
    }

    #[test]
    fn a_loop_goes_on_through_its_own_way_out() {
        // Not through the branch before it, which goes further on:
        //   8000000: f04f 5000  mov.w r0, #0x20000000
        //   8000004: 6801       ldr   r1, [r0, #0]
        //   8000006: 2900       cmp   r1, #0
        //   8000008: d003       beq.n 8000012
        //   800000a: 6842       ldr   r2, [r0, #4]
        //   800000c: 2a00       cmp   r2, #0
        //   800000e: d002       beq.n 8000016
        //   8000010: e7fb       b.n   800000a
        //   8000012: 2301       movs  r3, #1
        //   8000014: e7fe       b.n   8000014
        //   8000016: f000 f800  bl    800001a <main>
        // main:
        //   800001a: e7fe       b.n   800001a
        let code = [
            0xf04f, 0x5000, 0x6801, 0x2900, 0xd003, 0x6842, 0x2a00, 0xd002, 0xe7fb, 0x2301, 0xe7fe,
            0xf000, 0xf800, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_001a];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_001a)),
            [
                "0 0x800000a loop back at 0x8000010, from event 0",
                "0 0x8000016 call 0x800001a (0x20000000 ? ? ?)",
                "end Main(8000016)",
            ]
        );
    }

    #[test]
    fn code_after_the_function_s_other_code_that_goes_back_past_a_call_isnt_a_loop_s_code() {
        // The branch back goes to an outer loop, past a call, and not to
        // a test that leads to the branch to it:
        //   8000000: f04f 5000  mov.w r0, #0x20000000
        //   8000004: f000 f807  bl    8000016 <poll>
        //   8000008: 6801       ldr   r1, [r0, #0]
        //   800000a: 2900       cmp   r1, #0
        //   800000c: d001       beq.n 8000012
        //   800000e: f000 f803  bl    8000018 <main>
        //   8000012: 2201       movs  r2, #1
        //   8000014: e7f6       b.n   8000004
        // poll:
        //   8000016: 4770       bx    lr
        // main:
        //   8000018: e7fe       b.n   8000018
        let code = [
            0xf04f, 0x5000, 0xf000, 0xf807, 0x6801, 0x2900, 0xd001, 0xf000, 0xf803, 0x2201, 0xe7f6,
            0x4770, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0016, 0x0800_0018];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0018)),
            [
                "0 0x8000004 call 0x8000016 (0x20000000 ? ? ?)",
                "0 0x800000e call 0x8000018 (0x20000000 ? ? ?)",
                "end Main(800000e)",
            ]
        );
    }

    #[test]
    fn the_walk_goes_on_into_the_next_function_where_no_call_went_before() {
        // As where a function finder split the reset handler in two:
        //   8000000: 2001       movs  r0, #1
        // next:
        //   8000002: 2102       movs  r1, #2
        //   8000004: f000 f800  bl    8000008 <main>
        // main:
        //   8000008: e7fe       b.n   8000008
        let code = [0x2001, 0x2102, 0xf000, 0xf800, 0xe7fe];
        let functions = [0x0800_0000, 0x0800_0002, 0x0800_0008];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0008)),
            [
                "0 0x8000004 call 0x8000008 (0x1 0x2 ? ?)",
                "end Main(8000004)",
            ]
        );
    }

    #[test]
    fn a_branch_back_into_code_the_walk_went_through_is_a_loop() {
        // Though a function finder took the loop's test for another
        // function:
        //   8000000: f04f 5000  mov.w r0, #0x20000000
        //   8000004: 2100       movs  r1, #0
        //   8000006: 3101       adds  r1, #1
        //   8000008: 6001       str   r1, [r0, #0]
        // split:
        //   800000a: 6842       ldr   r2, [r0, #4]
        //   800000c: 2a00       cmp   r2, #0
        //   800000e: d1fa       bne.n 8000006
        //   8000010: f000 f800  bl    8000014 <main>
        // main:
        //   8000014: e7fe       b.n   8000014
        let code = [
            0xf04f, 0x5000, 0x2100, 0x3101, 0x6001, 0x6842, 0x2a00, 0xd1fa, 0xf000, 0xf800, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_000a, 0x0800_0014];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0014)),
            [
                "0 0x8000008 store 0x1 at 0x20000000, 4 bytes",
                "0 0x8000006 loop back at 0x800000e, from event 0",
                "0 0x8000010 call 0x8000014 (0x20000000 ? ? ?)",
                "end Main(8000010)",
            ]
        );
    }

    #[test]
    fn the_walk_goes_through_20_000_instructions_in_all() {
        // 70 calls of a function of 299 instructions:
        //   8000000: 4c23       ldr   r4, [pc, #140]  ; 0x8000097
        //   8000002: 47a0       blx   r4
        //   ...
        //   800008c: 47a0       blx   r4
        //   800008e: e001       b.n   8000094 <main>
        //   8000090: 08000097   .word
        // main:
        //   8000094: e7fe       b.n   8000094
        // slow:
        //   8000096: bf00       nop
        //   ...
        //   80002e8: bf00       nop
        //   80002ea: 4770       bx    lr
        let mut code = vec![0x4c23];
        code.extend([0x47a0; 70]);
        code.extend([0xe001, 0x0097, 0x0800, 0xe7fe]);
        code.extend([0xbf00; 298]);
        code.push(0x4770);
        let functions = [0x0800_0000, 0x0800_0094, 0x0800_0096];
        let lines = walk_lines(&code, &functions, Some(0x0800_0094));
        // The ldr, 66 calls of 300 instructions with the blx, and the 67th
        // blx make 19,802; the walk goes 198 instructions into slow.
        assert_eq!(lines.len(), 69);
        assert_eq!(
            lines[66..],
            [
                "0 0x8000086 call through r4 0x8000096 (? ? ? ?)",
                "1 0x8000086 stop: Budget(8000222)",
                "end Budget(8000088)",
            ]
        );
    }

    #[test]
    fn the_walk_of_a_function_it_calls_doesnt_take_branches_back() {
        // f branches through r2, which the walk can't tell, on one way;
        // the walk doesn't go back to take the other:
        //   8000000: f000 f802  bl    8000008 <f>
        //   8000004: f000 f807  bl    8000016 <main>
        // f:
        //   8000008: f04f 5000  mov.w r0, #0x20000000
        //   800000c: 6801       ldr   r1, [r0, #0]
        //   800000e: 2900       cmp   r1, #0
        //   8000010: d000       beq.n 8000014
        //   8000012: 4710       bx    r2
        //   8000014: 4770       bx    lr
        // main:
        //   8000016: e7fe       b.n   8000016
        let code = [
            0xf000, 0xf802, 0xf000, 0xf807, 0xf04f, 0x5000, 0x6801, 0x2900, 0xd000, 0x4710, 0x4770,
            0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0008, 0x0800_0016];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0016)),
            [
                "0 0x8000000 call 0x8000008 (? ? ? ?)",
                "1 0x8000000 stop: Branches { site: 8000012, through: Register(2) }",
                "0 0x8000004 call 0x8000016 (? ? ? ?)",
                "end Main(8000004)",
            ]
        );
    }

    #[test]
    fn a_branch_is_taken_when_the_other_way_returns_or_spins_before_main() {
        //   8000000: f04f 5000  mov.w r0, #0x20000000
        //   8000004: 6801       ldr   r1, [r0, #0]
        //   8000006: 2900       cmp   r1, #0
        //   8000008: d000       beq.n 800000c
        //   800000a: 4770       bx    lr
        //   800000c: 2901       cmp   r1, #1
        //   800000e: d000       beq.n 8000012
        //   8000010: e7fe       b.n   8000010
        //   8000012: f000 f800  bl    8000016 <main>
        // main:
        //   8000016: e7fe       b.n   8000016
        let code = [
            0xf04f, 0x5000, 0x6801, 0x2900, 0xd000, 0x4770, 0x2901, 0xd000, 0xe7fe, 0xf000, 0xf800,
            0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0016];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0016)),
            [
                "0 0x8000008 back to 0x800000c, since Returns(800000a)",
                "0 0x800000e back to 0x8000012, since Spins(8000010)",
                "0 0x8000012 call 0x8000016 (0x20000000 ? ? ?)",
                "end Main(8000012)",
            ]
        );
        // Without main, the walk ends where the code spins.
        //   8000000: f04f 5000  mov.w r0, #0x20000000
        //   8000004: 6801       ldr   r1, [r0, #0]
        //   8000006: 2900       cmp   r1, #0
        //   8000008: d000       beq.n 800000c
        //   800000a: e7fe       b.n   800000a
        //   800000c: 4770       bx    lr
        let code = [0xf04f, 0x5000, 0x6801, 0x2900, 0xd000, 0xe7fe, 0x4770];
        assert_eq!(walk_lines(&code, &[FLASH], None), ["end Spins(800000a)"]);
    }

    #[test]
    fn a_branch_taken_later_starts_from_what_the_walk_knew_there() {
        // The walk goes through the code at tail on the way on, so it
        // doesn't go back to take the branch there; it takes the one to
        // good, with r1 as it was then:
        //   8000000: 2101       movs  r1, #1
        //   8000002: f04f 5000  mov.w r0, #0x20000000
        //   8000006: 6803       ldr   r3, [r0, #0]
        //   8000008: 2b00       cmp   r3, #0
        //   800000a: d004       beq.n 8000016 <good>
        //   800000c: 2b01       cmp   r3, #1
        //   800000e: d000       beq.n 8000012 <tail>
        //   8000010: 2104       movs  r1, #4
        // tail:
        //   8000012: 2105       movs  r1, #5
        //   8000014: 4710       bx    r2
        // good:
        //   8000016: f000 f800  bl    800001a <main>
        // main:
        //   800001a: e7fe       b.n   800001a
        let code = [
            0x2101, 0xf04f, 0x5000, 0x6803, 0x2b00, 0xd004, 0x2b01, 0xd000, 0x2104, 0x2105, 0x4710,
            0xf000, 0xf800, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_001a];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_001a)),
            [
                "0 0x800000a back to 0x8000016, since Branches { site: 8000014, through: Register(2) }",
                "0 0x8000016 call 0x800001a (0x20000000 0x1 ? ?)",
                "end Main(8000016)",
            ]
        );
    }

    #[test]
    fn the_flags_that_compares_and_adds_set_and_the_conditions_they_meet() {
        // For each condition, in the order eq, ne, cs, cc, mi, pl, vs, vc,
        // hi, ls, ge, lt, gt, le and al: T where it holds, F where it
        // doesn't, ? where the walk can't tell.
        let conditions = |flags: Flags| -> String {
            (0..15)
                .map(|condition| match holds(condition, flags) {
                    Some(true) => 'T',
                    Some(false) => 'F',
                    None => '?',
                })
                .collect()
        };
        let one = Some(1);
        assert_eq!(conditions(subtract(one, Some(2))), "FTFTTFFTFTFTFTT");
        assert_eq!(conditions(subtract(Some(2), Some(2))), "TFTFFTFTFTTFFTT");
        assert_eq!(conditions(subtract(Some(3), one)), "FTTFFTFTTFTFTFT");
        // The smallest signed number less 1 overflows.
        assert_eq!(conditions(subtract(Some(1 << 31), one)), "FTTFFTTFTFFTFTT");
        assert_eq!(conditions(add(Some(u32::MAX), one)), "TFTFFTFTFTTFFTT");
        assert_eq!(conditions(add(Some(0x7fff_ffff), one)), "FTFTTFTFFTTFTFT");
        assert_eq!(conditions(subtract(None, one)), "??????????????T");
        // A condition of two flags fails when either does, if the walk
        // can't tell the other.
        let flags = Flags {
            carry: Some(false),
            ..Flags::default()
        };
        assert_eq!(conditions(flags), "??FT????FT????T");
        let flags = Flags {
            zero: Some(true),
            ..Flags::default()
        };
        assert_eq!(conditions(flags), "TF??????FT??FTT");
    }

    #[test]
    fn subs_sets_the_flags_that_a_branch_reads() {
        //   8000000: 2102       movs  r1, #2
        //   8000002: 3901       subs  r1, #1
        //   8000004: d100       bne.n 8000008
        //   8000006: e7fe       b.n   8000006
        //   8000008: f000 f800  bl    800000c <main>
        // main:
        //   800000c: e7fe       b.n   800000c
        let code = [0x2102, 0x3901, 0xd100, 0xe7fe, 0xf000, 0xf800, 0xe7fe];
        let functions = [0x0800_0000, 0x0800_000c];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_000c)),
            [
                "0 0x8000008 call 0x800000c (? 0x1 ? ?)",
                "end Main(8000008)"
            ]
        );
    }

    #[test]
    fn numbers_from_movw_and_movt_and_shifts() {
        //   8000000: f241 2034  movw  r0, #0x1234
        //   8000004: f2c2 0000  movt  r0, #0x2000
        //   8000008: 2384       movs  r3, #0x84
        //   800000a: 085b       lsrs  r3, r3, #1
        //   800000c: 2100       movs  r1, #0
        //   800000e: 3908       subs  r1, #8
        //   8000010: 1089       asrs  r1, r1, #2
        //   8000012: 091a       lsrs  r2, r3, #4
        //   8000014: f000 f800  bl    8000018 <main>
        // main:
        //   8000018: e7fe       b.n   8000018
        let code = [
            0xf241, 0x2034, 0xf2c2, 0x0000, 0x2384, 0x085b, 0x2100, 0x3908, 0x1089, 0x091a, 0xf000,
            0xf800, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0018];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0018)),
            [
                "0 0x8000014 call 0x8000018 (0x20001234 0xfffffffe 0x4 0x42)",
                "end Main(8000014)",
            ]
        );
    }

    #[test]
    fn calls_and_jumps_that_leave_the_file_s_code() {
        //   8000000: 4801       ldr   r0, [pc, #4]  ; 0x10000001
        //   8000002: 4780       blx   r0
        //   8000004: 4901       ldr   r1, [pc, #4]  ; 0x10000101
        //   8000006: 4708       bx    r1
        //   8000008: 10000001   .word
        //   800000c: 10000101   .word
        let code = [
            0x4801, 0x4780, 0x4901, 0x4708, 0x0001, 0x1000, 0x0101, 0x1000,
        ];
        assert_eq!(
            walk_lines(&code, &[FLASH], None),
            [
                "0 0x8000002 call through r0 0x10000000 (0x10000001 ? ? ?)",
                "1 0x8000002 stop: LeavesFile { site: 8000002, target: 10000000 }",
                "end LeavesFile { site: 8000006, target: 10000100 }",
            ]
        );
    }

    #[test]
    fn a_loop_with_no_way_out_and_code_the_walk_gets_back_to() {
        //   8000000: f04f 5000  mov.w r0, #0x20000000
        //   8000004: 6801       ldr   r1, [r0, #0]
        //   8000006: 6041       str   r1, [r0, #4]
        //   8000008: e7fc       b.n   8000004
        let code = [0xf04f, 0x5000, 0x6801, 0x6041, 0xe7fc];
        assert_eq!(
            walk_lines(&code, &[FLASH], None),
            [
                "0 0x8000006 store ? at 0x20000004, 4 bytes",
                "0 0x8000004 loop back at 0x8000008, from event 0",
                "end Loops(8000008)",
            ]
        );
        //   8000000: e000       b.n   8000004
        //   8000002: 2101       movs  r1, #1
        //   8000004: 2000       movs  r0, #0
        //   8000006: e7fc       b.n   8000002
        let code = [0xe000, 0x2101, 0x2000, 0xe7fc];
        assert_eq!(walk_lines(&code, &[FLASH], None), ["end Rejoins(8000004)"]);
    }

    #[test]
    fn a_veneer_that_loads_the_address_it_goes_to_into_pc() {
        //   8000000: 2003       movs  r0, #3
        //   8000002: f000 f803  bl    800000c <veneer>
        //   8000006: f000 f807  bl    8000018 <main>
        //   800000a: bf00       nop
        // veneer:
        //   800000c: f8df f000  ldr.w pc, [pc, #0]  ; 0x8000015
        //   8000010: 08000015   .word
        // func:
        //   8000014: 2104       movs  r1, #4
        //   8000016: 4770       bx    lr
        // main:
        //   8000018: e7fe       b.n   8000018
        let code = [
            0x2003, 0xf000, 0xf803, 0xf000, 0xf807, 0xbf00, 0xf8df, 0xf000, 0x0015, 0x0800, 0x2104,
            0x4770, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_000c, 0x0800_0014, 0x0800_0018];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0018)),
            [
                "0 0x8000002 call 0x800000c (0x3 ? ? ?)",
                "1 0x800000c tail call 0x8000014 (0x3 ? ? ?)",
                "0 0x8000006 call 0x8000018 (0x3 0x4 ? ?), left by a call: r1",
                "end Main(8000006)",
            ]
        );
    }

    #[test]
    fn an_and_in_an_it_block_doesnt_set_the_flags() {
        // So bne reads the flags of the cmp:
        //   8000000: 2001       movs  r0, #1
        //   8000002: 2102       movs  r1, #2
        //   8000004: 2200       movs  r2, #0
        //   8000006: 4288       cmp   r0, r1
        //   8000008: bf38       it    cc
        //   800000a: 400a       andcc r2, r1
        //   800000c: d100       bne.n 8000010
        //   800000e: e7fe       b.n   800000e
        //   8000010: f000 f800  bl    8000014 <main>
        // main:
        //   8000014: e7fe       b.n   8000014
        let code = [
            0x2001, 0x2102, 0x2200, 0x4288, 0xbf38, 0x400a, 0xd100, 0xe7fe, 0xf000, 0xf800, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0014];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0014)),
            [
                "0 0x8000010 call 0x8000014 (0x1 0x2 0x0 ?)",
                "end Main(8000010)"
            ]
        );
    }
}
