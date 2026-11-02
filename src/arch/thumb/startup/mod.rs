//! The startup walk (see `arch::startup`) through Thumb code.
//! docs/design.md's "Startup, end to end" shows where it fits in
//! `elfq FILE startup`.
//!
//! It is a second reader of what `decode` says each instruction does,
//! beside `values`. `values` follows every path through one function at
//! once, with every number a register may hold on any of them, to find the
//! addresses that code branches through. The walk goes one way, as the
//! processor would from reset, through the functions that the startup
//! code calls, so a register holds one number or one it can't tell, and
//! memory is at the addresses the code uses, the stack among them.
//!
//! # How the walk is organized
//!
//! - [`walk`] makes a [`Walker`], which keeps what the whole walk finds and
//!   spends: the events, the budgets, and the loops it is going through
//!   iteration by iteration ([`Walker::open_loops`]).
//! - [`Walker::run`] walks one function's code, or one loop's: a [`Run`],
//!   within the limits of a [`Frame`]. [`Walker::step`] runs each
//!   instruction, with one method for each way it goes on, such as
//!   [`Walker::on_branch`]. `execute.rs` works out what an instruction
//!   does to what the walk knows there, a [`Known`] (in `walk_state.rs`,
//!   with the [`Visit`] that keeps what it knew when it first got to an
//!   instruction), and `calls.rs` follows a call. When the way the walk
//!   went ends where it can't go on, [`Walker::backtrack`] goes back to a
//!   branch whose condition it couldn't tell.
//! - A branch back is a loop ([`Walker::branch_back`]). `loops.rs` finds
//!   the loop's code, and [`Walker::close_loop`] decides what the walk
//!   knows after it: `count.rs` counts how many times the loop runs by
//!   going through its code iteration by iteration ([`OpenLoop`]), and
//!   `each_iteration.rs` goes through each iteration of a loop over a
//!   table, with its calls and the loops inside.
//! - `queries.rs` answers two questions that `commands/startup` asks about
//!   the code around a call, and `tests.rs` walks small programs.
//!
//! The limits below say how far the walk goes.

mod calls;
mod count;
mod each_iteration;
mod execute;
mod loops;
mod queries;
#[cfg(test)]
mod tests;
mod walk_state;

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};

use object::Endianness;

use super::decode::{self, Addressing, Effect, Flow, LowBit, Operation, Registers, Transfer, When};
use super::{ARGUMENTS, CALL_CLOBBERS, SP, Scanner, branch, is_wide, literal};
use crate::arch::CodeReferenceKind;
use crate::arch::startup::{
    Call, DEPTH, End, Event, FunctionBounds, How, Program, Start, Through, Walk,
};

use calls::arguments;
use count::{LoopCode, Mode, OpenLoop, StoppedAt};
use execute::holds;
use loops::FoundLoop;
pub(in crate::arch) use queries::{branches_past, passes_result};
use walk_state::{Alternative, Budget, Compare, Known, Visit};

// The limits. The walk spends instructions from two budgets for all of it,
// one for what it walks and one, on top, for counting loops. Each
// function's code and each loop's has a limit of its own, as does how deep
// loops nest. Counting a loop has limits on how many times it may run and
// how many it tries. A loop that the walk goes through each iteration of
// spends budgets of its own in place of the two for all of it, taken from
// two totals for all such loops. How many calls deep it follows is
// `arch::startup::DEPTH`. `arch::startup::Code::walk` lists them with what
// each means for what the walk finds, and links to them.

/// How many instructions the walk goes through in all, but for those it
/// goes through to count loops ([`COUNT_BUDGET`]) and in loops that it goes
/// through each iteration of ([`WALK_EACH_TOTAL`]).
pub(in crate::arch) const BUDGET: usize = 20_000;

/// How many instructions it goes through in all to count how many times
/// loops run, on top of [`BUDGET`].
pub(in crate::arch) const COUNT_BUDGET: usize = 50_000;

/// How many instructions it goes through in the reset handler's own code.
pub(in crate::arch) const RESET_LIMIT: usize = 600;

/// How many it goes through in each function it calls.
pub(in crate::arch) const CALL_LIMIT: usize = 300;

/// How many it goes through in a loop's code, each time through it.
pub(in crate::arch) const LOOP_LIMIT: usize = 60;

/// How many loops inside loops it walks the code of.
pub(in crate::arch) const NESTING: u32 = 8;

/// How many times at most a loop that the walk can tell runs: more is
/// not a loop of startup code.
pub(in crate::arch) const MOST_ITERATIONS: u32 = 1 << 24;

/// How many of the iterations after which a loop may leave, as its compares
/// tell, it tries.
pub(in crate::arch) const CANDIDATES: usize = 12;

/// How many times at most a loop runs that the walk goes through each
/// iteration of with the calls it makes and the loops inside, as a loop
/// over a table of sections; and how many times it goes through a loop's
/// code to count it when its compares don't tell.
pub(in crate::arch) const MOST_WALKED_ITERATIONS: u32 = 16;

/// How many instructions the walk goes through in the iterations of one
/// loop that it goes through each iteration of, in place of [`BUDGET`]. A
/// loop that costs more is walked once, as others, and leaves the rest of
/// the walk as it would be had the walk not tried (see
/// [`Walker::walk_each_iteration`]).
pub(in crate::arch) const WALK_EACH_BUDGET: usize = 5_000;

/// How many it goes through to count the loops inside those iterations, in
/// place of [`COUNT_BUDGET`].
pub(in crate::arch) const WALK_EACH_COUNT_BUDGET: usize = 10_000;

/// How many instructions it goes through in all the loops that it goes
/// through each iteration of, those it tries and gives up on too, from
/// which each takes up to [`WALK_EACH_BUDGET`].
pub(in crate::arch) const WALK_EACH_TOTAL: usize = 2 * WALK_EACH_BUDGET;

/// How many it goes through in all to count the loops inside them, from
/// which each takes up to [`WALK_EACH_COUNT_BUDGET`].
pub(in crate::arch) const WALK_EACH_COUNT_TOTAL: usize = 2 * WALK_EACH_COUNT_BUDGET;

/// How far before a function's start the code of a loop can begin that
/// runs on into the function (see [`Walker::runs_into`]).
const RUNS_INTO: u64 = 0x40;

/// Walks the code from `start`; see `arch::startup::Code::walk`.
pub(in crate::arch) fn walk(endianness: Endianness, program: &dyn Program, start: &Start) -> Walk {
    let mut walker = Walker {
        program,
        endianness,
        top: start.stack_pointer,
        lowest: start.stack_pointer,
        main: start.main,
        events: Vec::new(),
        budget: Budget {
            walk: BUDGET,
            count: COUNT_BUDGET,
        },
        walk_each_left: Budget {
            walk: WALK_EACH_TOTAL,
            count: WALK_EACH_COUNT_TOTAL,
        },
        open_loops: Vec::new(),
        data: HashMap::new(),
        clobbers: HashMap::new(),
    };
    let mut known = Known::default();
    known.registers[usize::from(SP)] = start.stack_pointer;
    let (finish, _) = walker.walk(start.address & !1, Frame::reset(start.follow), known);
    let end = match finish {
        Finish::Ended(end) => end,
        // Only the walk of a loop's code stops without an end.
        Finish::AtStop => End::Budget(start.address & !1),
    };
    Walk {
        events: walker.events,
        end,
    }
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
    /// When it is one iteration of a loop that the walk counts or goes
    /// through each iteration of, which stops where the loop goes back or
    /// leaves: the loop's place in [`Walker::open_loops`].
    iteration_of: Option<usize>,
    /// When its code is part of one iteration of a loop that the walk goes
    /// through each iteration of, but for what that calls, as the code of a
    /// loop inside it is: the loop's place in [`Walker::open_loops`], which
    /// keeps the words its code loads (see [`OpenLoop::loads`]).
    rows: Option<usize>,
}

impl Frame {
    /// The reset handler's code, from which the walk follows calls
    /// `follow` deep, at most [`DEPTH`].
    fn reset(follow: u32) -> Frame {
        Frame::function(0, follow.min(DEPTH), RESET_LIMIT)
    }

    /// The code of a function that this frame's code calls: one deeper,
    /// following one call fewer, if any.
    fn callee(self) -> Frame {
        Frame::function(self.depth + 1, self.follow.saturating_sub(1), CALL_LIMIT)
    }

    /// A function's code, `depth` calls deep, following calls `follow`
    /// deeper, through at most `limit` instructions.
    fn function(depth: u32, follow: u32, limit: usize) -> Frame {
        Frame {
            depth,
            follow,
            limit,
            stop: None,
            nesting: 0,
            iteration_of: None,
            rows: None,
        }
    }

    /// The code of a loop in this frame's code, walked once from its head
    /// to its branch back at `back`.
    fn loop_body(self, back: u64) -> Frame {
        Frame {
            limit: LOOP_LIMIT,
            stop: Some(back),
            nesting: self.nesting + 1,
            iteration_of: None,
            ..self
        }
    }

    /// One iteration of the loop at `index` in [`Walker::open_loops`], whose
    /// code is in this frame's, as `mode` says: with its calls, the loops
    /// inside and their events ([`Mode::WalkEach`]), or only to count the
    /// loop ([`Mode::Count`]), following no calls, through at most the
    /// `count_budget` instructions left to count with.
    fn iteration(self, index: usize, mode: Mode, count_budget: usize) -> Frame {
        let walks = mode == Mode::WalkEach;
        Frame {
            follow: if walks { self.follow } else { 0 },
            limit: if walks {
                LOOP_LIMIT
            } else {
                LOOP_LIMIT.min(count_budget)
            },
            stop: None,
            nesting: self.nesting + 1,
            iteration_of: Some(index),
            rows: walks.then_some(index),
            ..self
        }
    }
}

/// Where one walk is.
struct Run {
    known: Known,
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
    bounds: Option<FunctionBounds>,
    /// How many instructions it went through.
    count: usize,
    /// Whether the walk got here from a call that returned, through
    /// instructions that only go on to the next.
    after_call: bool,
}

/// An instruction that runs, or may, as one in an `it` block does when
/// its condition holds.
#[derive(Clone, Copy)]
struct Running {
    instruction: Decoded,
    in_it_block: bool,
    /// Whether it runs: `None` when the walk can't tell the flags that its
    /// condition reads.
    runs: Option<bool>,
}

impl Running {
    /// Whether it runs only when flags that the walk can't tell are set.
    fn conditional(&self) -> bool {
        self.runs.is_none()
    }

    /// Whether it sets the flags, where it is.
    fn sets_flags(&self) -> bool {
        self.instruction.effect.sets_flags(self.in_it_block)
    }
}

/// A branch that the walk may take or not.
#[derive(Clone, Copy)]
struct Branch {
    site: u64,
    /// Where it goes when it is taken.
    target: u64,
    /// The instruction after it, where the code goes on when it isn't.
    next: u64,
    /// Whether it is taken, when the walk can tell.
    taken: Option<bool>,
}

/// What an instruction leads to.
enum Step {
    /// On to the instruction at this address.
    Next(u64),
    /// The walk stops before it, as [`Finish::AtStop`].
    Stopped,
    /// The walk ends here, unless it goes back to a branch it didn't take.
    MayBacktrack(End),
    /// The walk ends here.
    Final(End),
}

/// How a walk finished.
#[derive(Debug, Clone, Copy)]
enum Finish {
    /// It ended, as this says.
    Ended(End),
    /// A loop's walk stopped where its frame says, without an end: the
    /// walk of its code at its branch back, and one iteration where the
    /// loop goes back or leaves (see [`Walker::stop_here`]).
    AtStop,
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
    /// How many more instructions the walk may go through, from
    /// [`BUDGET`] and [`COUNT_BUDGET`]; or while it goes through each
    /// iteration of a loop, how many more those iterations may.
    budget: Budget,
    /// What is left of [`WALK_EACH_TOTAL`] and [`WALK_EACH_COUNT_TOTAL`],
    /// from which each loop that the walk goes through each iteration of
    /// takes at most [`WALK_EACH_BUDGET`] and [`WALK_EACH_COUNT_BUDGET`]
    /// as its budget (see [`Walker::walk_each_iteration`]).
    walk_each_left: Budget,
    /// The loops that the walk goes through iteration by iteration, the
    /// innermost last: one that it counts, or those that it goes through
    /// each iteration of and loops inside them that it counts.
    open_loops: Vec<OpenLoop>,
    /// The data in each function the walk got to, by where the function
    /// starts: the constants and jump tables its code reads, as offsets
    /// from its start.
    data: HashMap<u64, BTreeMap<usize, usize>>,
    /// What [`Walker::clobbers`] found for each function, by where it
    /// starts.
    clobbers: HashMap<u64, (Registers, u8)>,
}

impl Walker<'_> {
    /// Walks from `start` with what `known` knows, until it ends, or for a
    /// loop, until it stops where `frame` says. Returns how it finished,
    /// and what it knew then.
    fn walk(&mut self, start: u64, frame: Frame, known: Known) -> (Finish, Known) {
        let (finish, run) = self.run(start, frame, known);
        (finish, run.known)
    }

    /// Walks as [`Walker::walk`] does, and returns how it finished and
    /// where it went.
    fn run(&mut self, start: u64, frame: Frame, known: Known) -> (Finish, Run) {
        let mut run = Run {
            known,
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
                Step::Stopped => return (Finish::AtStop, run),
                Step::Final(end) => end,
                Step::MayBacktrack(end) => match self.backtrack(&mut run, frame, end) {
                    Some(target) => {
                        address = target;
                        continue;
                    }
                    None => end,
                },
            };
            return (Finish::Ended(end), run);
        }
    }

    /// Runs the instruction at `address`: unless the walk stops before it
    /// ([`Walker::stop_here`] and [`Walker::enter`]), reads it
    /// ([`Walker::fetch`]), passes over an `it` and an instruction in an
    /// `it` block whose condition doesn't hold ([`Run::running`]), and does
    /// what its flow says.
    fn step(&mut self, run: &mut Run, frame: Frame, address: u64) -> Step {
        if let Some(step) = self.stop_here(run, frame, address) {
            return step;
        }
        if let Some(step) = self.enter(run, frame, address) {
            return step;
        }
        let after_call = std::mem::take(&mut run.after_call);
        let Some(instruction) = self.fetch(run, address, after_call) else {
            return no_code(address, after_call);
        };
        let Some(running) = run.running(instruction) else {
            return Step::Next(instruction.next());
        };
        match instruction.effect.flow {
            Flow::Next | Flow::It { .. } => self.straight(run, frame, running, after_call),
            Flow::Call | Flow::CallRegister(..) => self.on_call(run, frame, running),
            Flow::Branch { target, when } => self.on_branch(run, frame, running, target, when),
            _ if running.conditional() => self.may_jump(run, frame, running),
            Flow::Return => Step::MayBacktrack(End::Returns(address)),
            Flow::Pop => self.on_pop(run, frame, instruction),
            Flow::Stop => no_code(address, after_call),
            Flow::Table | Flow::JumpRegister(..) | Flow::JumpRelative(_) | Flow::JumpMemory => {
                self.on_jump(run, frame, instruction)
            }
        }
    }

    /// [`Step::Stopped`] when the walk stops before the instruction at
    /// `address`: in one iteration of a loop that the walk goes through one
    /// by one, at the branch back or where the loop leaves, which is past
    /// the branch in the first iteration and anywhere but the first
    /// iteration's code later; and in a loop's code, at its branch back. In
    /// an iteration, it also records where the iteration stopped and
    /// whether the loop goes back, in the loop's [`OpenLoop::stopped`],
    /// which [`Walker::walk_iteration`] reads.
    fn stop_here(&mut self, run: &Run, frame: Frame, address: u64) -> Option<Step> {
        let Some(index) = frame.iteration_of else {
            let at_back = frame.stop.is_some_and(|stop| address >= stop);
            return at_back.then_some(Step::Stopped);
        };
        let open = &self.open_loops[index];
        let back = open.back;
        let leaves = match &open.code {
            LoopCode::NotYet => address > back,
            LoopCode::Seen { code, .. } => !code.contains(&address),
        };
        if address != back && !leaves {
            return None;
        }
        let goes_back = match &open.code {
            _ if address == back => self.goes_back(run, back),
            LoopCode::NotYet => Some(false),
            LoopCode::Seen { ways_out, .. } => ways_out.contains(&address).then_some(false),
        };
        let stopped = StoppedAt {
            at: address,
            goes_back,
        };
        self.open_loops[index].stopped = Some(stopped);
        Some(Step::Stopped)
    }

    /// Goes to the instruction at `address`, unless the walk ends there:
    /// where code after a call that can't return runs off its function,
    /// out of instructions to go through, where it has been before, or at
    /// `main`. Other code that runs on into the next function goes on
    /// there, as hand-written startup code may.
    fn enter(&mut self, run: &mut Run, frame: Frame, address: u64) -> Option<Step> {
        if run.leaves(address) {
            if run.after_call {
                return Some(Step::Final(End::RunsOff(address)));
            }
            run.bounds = self.program.function_at(address);
        }
        if run.count >= frame.limit || self.budget.walk == 0 {
            return Some(Step::Final(End::Budget(address)));
        }
        run.count += 1;
        self.budget.walk -= 1;
        let visit = run.visit(self.events.len());
        match run.visits.entry(address) {
            Entry::Occupied(_) => return Some(Step::Final(End::Rejoins(address))),
            Entry::Vacant(entry) => entry.insert(visit),
        };
        run.order.push(address);
        if self.main == Some(address) {
            return Some(Step::Final(End::Main(address)));
        }
        None
    }

    /// The instruction at `address`, unless there is none that the walk
    /// can run there: data that the function's code reads, as a literal
    /// pool, an instruction elfq can't decode, or `bkpt` `after_call`.
    fn fetch(&mut self, run: &Run, address: u64, after_call: bool) -> Option<Decoded> {
        let instruction = decode_at(self.program, address)?;
        if self.is_data(run.bounds, address) {
            return None;
        }
        // `bkpt` straight after a call, as after the call to `exit` in the
        // Pico SDK's startup, which faults without a debugger, but for
        // `bkpt 0xab`, which asks a debugger for semihosting.
        let first = instruction.first;
        if after_call && first & 0xff00 == 0xbe00 && first & 0xff != 0xab {
            return None;
        }
        Some(instruction)
    }

    /// An instruction that goes on to the next one: what it does to what
    /// the walk knows. In an iteration of a loop whose rows of a table the
    /// walk looks for, it keeps the words that the instruction reads.
    fn straight(
        &mut self,
        run: &mut Run,
        frame: Frame,
        running: Running,
        after_call: bool,
    ) -> Step {
        let Running { instruction, .. } = running;
        let (site, effect) = (instruction.address, instruction.effect);
        let conditional = running.conditional();
        if let Some(index) = frame.rows {
            if !conditional {
                self.row_load(run, index, site, &effect);
            }
            self.carried_reads(index, effect.reads, effect.writes, conditional);
        }
        let sets_flags = running.sets_flags();
        self.execute(
            &mut run.known,
            site,
            &effect,
            sets_flags,
            conditional,
            frame.depth,
        );
        // The walk is still straight after a call (see `Run::after_call`).
        run.after_call = after_call;
        Step::Next(instruction.next())
    }

    /// An instruction in an `it` block whose condition the walk can't tell
    /// that may jump, return or trap: the walk goes on with what it writes
    /// unknown.
    fn may_jump(&mut self, run: &mut Run, frame: Frame, running: Running) -> Step {
        let Running { instruction, .. } = running;
        let sets_flags = running.sets_flags();
        let (site, effect) = (instruction.address, &instruction.effect);
        self.execute(&mut run.known, site, effect, sets_flags, true, frame.depth);
        run.known.forks = true;
        Step::Next(instruction.next())
    }

    /// A call, which the walk records, and follows as [`Walker::call`]
    /// says.
    fn on_call(&mut self, run: &mut Run, frame: Frame, running: Running) -> Step {
        let Running { instruction, .. } = running;
        let conditional = running.conditional();
        // A call reads its arguments, in r0 to r3.
        if let Some(index) = frame.rows {
            let reads = instruction.effect.reads | ARGUMENTS;
            self.carried_reads(index, reads, CALL_CLOBBERS, conditional);
        }
        let (target, how) = self.called(&run.known, instruction);
        if let Some(end) = self.call(run, frame, instruction.address, target, how, conditional) {
            return Step::Final(end);
        }
        run.after_call = true;
        Step::Next(instruction.next())
    }

    /// What the call `instruction` calls, when the walk can tell, and how.
    fn called(&self, known: &Known, instruction: Decoded) -> (Option<u64>, How) {
        let site = instruction.address;
        match instruction.effect.flow {
            Flow::CallRegister(register, low_bit) => {
                let target = self.register(known, register, site);
                let target = target.and_then(|number| low_bit.target(number));
                (target.map(u64::from), How::Register(register))
            }
            // svc
            _ if instruction.size() == 2 => (None, How::Supervisor),
            _ => match branch(site + 4, instruction.first, instruction.second) {
                Some(CodeReferenceKind::Call(target)) => (Some(target), How::Direct),
                _ => (None, How::Direct),
            },
        }
    }

    /// A `pop` with pc: a return, unless the walk can tell the word it pops
    /// into pc, as LLD's Cortex-M0 veneers push the address they go to and
    /// pop it: a called function starts with lr unknown.
    fn on_pop(&mut self, run: &mut Run, frame: Frame, instruction: Decoded) -> Step {
        let site = instruction.address;
        let to = self.popped_pc(&run.known, instruction.effect.operation);
        self.execute(
            &mut run.known,
            site,
            &instruction.effect,
            false,
            false,
            frame.depth,
        );
        match to.and_then(|number| LowBit::Thumb.target(number)) {
            None => Step::MayBacktrack(End::Returns(site)),
            Some(to) => self.jump_to(run, frame, site, Some(u64::from(to)), Through::Memory),
        }
    }

    /// The word that `operation`, a `pop` with pc, pops into pc, when the
    /// walk can tell it.
    fn popped_pc(&self, known: &Known, operation: Operation) -> Option<u32> {
        let Operation::Pop { registers, .. } = operation else {
            return None;
        };
        // pc is the highest register, so its word is the last.
        let sp = known.registers[usize::from(SP)]?;
        self.load(
            known,
            sp.wrapping_add(4 * (registers.count_ones() - 1)),
            false,
        )
    }

    /// A jump through a register, through a word in memory, or to a case
    /// of a jump table: on to where it goes, when the walk can tell (see
    /// [`Walker::jump_to`]). `ldr pc, [sp, #imm]` returns.
    fn on_jump(&mut self, run: &mut Run, frame: Frame, instruction: Decoded) -> Step {
        let Decoded {
            address: site,
            first,
            second,
            effect,
        } = instruction;
        let known = &run.known;
        let (to, through) = match effect.flow {
            Flow::JumpRegister(register, low_bit) => {
                let to = self.register(known, register, site);
                let to = to.and_then(|number| low_bit.target(number));
                (to.map(u64::from), Through::Register(register))
            }
            Flow::JumpRelative(register) => {
                let pc = (site as u32).wrapping_add(4);
                let to = self.register(known, register, site);
                let to = to.map(|offset| u64::from(pc.wrapping_add(offset) & !1));
                (to, Through::Register(register))
            }
            Flow::JumpMemory => {
                if let Operation::Load { base: SP, .. } = effect.operation {
                    return Step::MayBacktrack(End::Returns(site));
                }
                let at = match effect.operation {
                    // ldr.w pc, [base, #imm]. Not an `ldm`, whose offset is
                    // where its first word is, not pc's.
                    Operation::Load {
                        into: Transfer::Other,
                        base,
                        addressing: Addressing::Offset(offset),
                    } => (self.register(known, base, site))
                        .map(|base| base.wrapping_add_signed(offset)),
                    // ldr.w pc, [pc, #imm], as in a veneer.
                    Operation::Nothing if first & 0xff7f == 0xf85f => {
                        Some(literal(site + 4, first, u64::from(second & 0xfff)) as u32)
                    }
                    _ => None,
                };
                let to = at.and_then(|at| self.load(known, at, true));
                let to = to.and_then(|number| LowBit::Thumb.target(number));
                (to.map(u64::from), Through::Memory)
            }
            // A case of a jump table, which the walk can't tell.
            _ => (None, Through::Table),
        };
        self.jump_to(run, frame, site, to, through)
    }

    /// A branch: one that always goes, outside an `it` block, as
    /// [`Walker::jump`] says, and others as [`Walker::branch`] says.
    fn on_branch(
        &mut self,
        run: &mut Run,
        frame: Frame,
        running: Running,
        target: u64,
        when: When,
    ) -> Step {
        let site = running.instruction.address;
        // What a loop that the walk counts compares: `cbz` and `cbnz`
        // compare a register with 0.
        if let When::Zero(register) | When::NotZero(register) = when
            && !self.open_loops.is_empty()
        {
            let number = self.register(&run.known, register, site);
            run.known.compares.push(Compare {
                site,
                numbers: [number, Some(0)],
            });
        }
        if when == When::Always && !running.in_it_block {
            return self.jump(run, frame, site, target);
        }
        // One that always goes, in an `it` block, is taken when it runs.
        let taken = match when {
            When::Always => running.runs,
            _ => self.taken(&run.known, when, site),
        };
        let branch = Branch {
            site,
            target,
            next: running.instruction.next(),
            taken,
        };
        self.branch(run, frame, branch)
    }

    /// Whether a branch at `site` that goes `when` is taken, as the walk
    /// knows `known`, when it can tell: one that always goes is.
    fn taken(&self, known: &Known, when: When, site: u64) -> Option<bool> {
        match when {
            When::Always => Some(true),
            When::Flags(condition) => holds(condition, known.flags),
            When::Zero(register) => self.register(known, register, site).map(|n| n == 0),
            When::NotZero(register) => self.register(known, register, site).map(|n| n != 0),
        }
    }

    /// An unconditional branch at `site` to `target`.
    fn jump(&mut self, run: &mut Run, frame: Frame, site: u64, target: u64) -> Step {
        if target == site {
            return Step::MayBacktrack(End::Spins(site));
        }
        if let Some(visit) = run.visits.get(&target).filter(|_| target < site) {
            let visit = visit.clone();
            let found = FoundLoop {
                head: target,
                back: site,
                before: &visit,
                stopped: None,
                known_to_run: true,
            };
            let exit = self.close_loop(run, frame, found);
            return match exit.or_else(|| run.way_out(target, site)) {
                Some(exit) => Step::Next(exit),
                None => Step::MayBacktrack(End::Loops(site)),
            };
        }
        if run.leaves(target) {
            return self.tail_call(run, frame, site, target);
        }
        Step::Next(target)
    }

    /// A branch that may not be taken: to another function
    /// ([`Walker::branch_out`]), back, as a loop's
    /// ([`Walker::branch_back`]), or forward ([`Walker::branch_forward`]).
    fn branch(&mut self, run: &mut Run, frame: Frame, branch: Branch) -> Step {
        let Branch { site, target, .. } = branch;
        // A branch back to code in another function is a loop's when the
        // walk went through that code, or it runs on into this function.
        let back = target <= site;
        let loops_back = back && (self.went_through(run, target) || self.runs_into(run, target));
        if run.leaves(target) && !loops_back {
            return self.branch_out(run, frame, branch);
        }
        if back {
            return self.branch_back(run, frame, branch);
        }
        self.branch_forward(run, frame, branch)
    }

    /// A branch to another function: a tail call when it is taken, as
    /// Clang makes of `if (a) f(); else g();`. When the walk can't tell,
    /// it records the call, which it doesn't follow, and goes on.
    fn branch_out(&mut self, run: &mut Run, frame: Frame, branch: Branch) -> Step {
        let Branch {
            site,
            target,
            next,
            taken,
        } = branch;
        match taken {
            Some(true) => self.tail_call(run, frame, site, target),
            Some(false) => Step::Next(next),
            None => {
                self.events.push(Event::Call(Call {
                    site,
                    depth: frame.depth,
                    target: Some(target),
                    how: How::Jump,
                    arguments: arguments(&run.known),
                    left: run.known.left,
                    conditional: true,
                    followed: false,
                }));
                run.known.forks = true;
                Step::Next(next)
            }
        }
    }

    /// A branch forward in the function, which the walk takes when it can
    /// tell it is taken. When it can't, it goes on to the next instruction,
    /// and outside a loop's code keeps the branch to go back to (see
    /// [`Walker::backtrack`]), unless the branch goes to a loop's code that
    /// the compiler put after the function's other code (see
    /// [`Walker::out_of_line_loop`]).
    fn branch_forward(&mut self, run: &mut Run, frame: Frame, branch: Branch) -> Step {
        let Branch {
            site,
            target,
            next,
            taken,
        } = branch;
        if taken == Some(true) {
            run.pending.push((site, next));
            return Step::Next(target);
        }
        if taken.is_none() && frame.stop.is_none() {
            if let Some(step) = self.out_of_line_loop(run, frame, branch) {
                return step;
            }
            run.alternatives.push(Alternative {
                site,
                target,
                events: self.events.len(),
                known: run.known.clone(),
                order: run.order.len(),
                pending: run.pending.clone(),
                bounds: run.bounds,
            });
        }
        if taken.is_none() {
            run.known.forks = true;
        }
        run.pending.push((site, target));
        Step::Next(next)
    }

    /// Goes back to the latest branch whose condition the walk couldn't
    /// tell and that it hasn't been to, after the walk ended with `end`, in
    /// the reset handler's code: when `end` is dead, or a return or a spin
    /// before `main`. Returns where the branch goes.
    fn backtrack(&mut self, run: &mut Run, frame: Frame, end: End) -> Option<u64> {
        let stops_short = self.main.is_some() && matches!(end, End::Spins(_) | End::Returns(_));
        let back = frame.depth == 0 && frame.stop.is_none() && (end.is_dead() || stops_short);
        if !back || self.budget.walk == 0 {
            return None;
        }
        while let Some(alternative) = run.alternatives.pop() {
            if run.visits.contains_key(&alternative.target) {
                continue;
            }
            // Only the events go back to what they were at the branch, not
            // all that a `Mark` puts back: the way given up still spent
            // what it spent of the budget, which bounds all that the walk
            // does, and the stack pointer stays as low as that way took it,
            // so that stores there count as ones to the stack (see the test
            // `the_stack_stays_as_low_as_a_way_the_walk_gave_up_took_it`).
            self.events.truncate(alternative.events);
            self.events.push(Event::Backtrack {
                site: alternative.site,
                target: alternative.target,
                because: end,
            });
            for address in run.order.drain(alternative.order..) {
                run.visits.remove(&address);
            }
            run.known = alternative.known;
            run.pending = alternative.pending;
            run.bounds = alternative.bounds;
            run.it = 0;
            return Some(alternative.target);
        }
        None
    }

    /// Whether `address`, in the function at `bounds`, holds data that the
    /// function's code reads, such as a literal pool, which compilers put
    /// after its last instruction, as after a call that doesn't return.
    fn is_data(&mut self, bounds: Option<FunctionBounds>, address: u64) -> bool {
        let Some(FunctionBounds { start, end }) = bounds else {
            return false;
        };
        let (program, endianness) = (self.program, self.endianness);
        let data = self.data.entry(start).or_insert_with(|| {
            let code = program.code(start).unwrap_or_default();
            let code = &code[..code.len().min((end - start) as usize)];
            Scanner::swept(code, start, endianness).data
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
        false => Step::MayBacktrack(End::Undecoded(address)),
    }
}

impl Run {
    /// What the walk knows now, as it would be at an instruction it got to.
    fn visit(&self, events: usize) -> Visit {
        Visit {
            known: self.known.snapshot(),
            events,
            order: self.order.len(),
        }
    }

    /// The condition of the next instruction in an `it` block; `None`
    /// outside one.
    fn condition(&self) -> Option<u16> {
        (self.it & 0xf != 0).then(|| u16::from(self.it >> 4))
    }

    /// The condition of the next instruction in an `it` block, which moves
    /// the block on by one; `None` outside one.
    fn next_condition(&mut self) -> Option<u16> {
        let condition = self.condition()?;
        self.it = match self.it & 7 {
            0 => 0,
            _ => self.it & 0xe0 | (self.it << 1) & 0x1f,
        };
        Some(condition)
    }

    /// `instruction` as it runs here, with whether it does when it is in an
    /// `it` block, which moves the block on; `None` when the walk passes
    /// over it to the next: an `it`, which starts a block, and one in a
    /// block whose condition the flags say doesn't hold.
    fn running(&mut self, instruction: Decoded) -> Option<Running> {
        if let Flow::It { firstcond, mask } = instruction.effect.flow {
            self.it = (firstcond << 4 | mask) as u8;
            return None;
        }
        let condition = self.next_condition();
        let runs = condition.map_or(Some(true), |condition| holds(condition, self.known.flags));
        if runs == Some(false) {
            return None;
        }
        Some(Running {
            instruction,
            in_it_block: condition.is_some(),
            runs,
        })
    }

    /// Whether `target` is outside the function the walk is in.
    fn leaves(&self, target: u64) -> bool {
        self.bounds.is_some_and(|bounds| !bounds.contains(target))
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

/// An instruction that the walk reads: where it is, its two halves, and
/// what it does.
#[derive(Clone, Copy)]
struct Decoded {
    address: u64,
    first: u16,
    /// 0 for a 16-bit instruction.
    second: u16,
    effect: Effect,
}

impl Decoded {
    /// How many bytes it takes: 2, or 4 for a 32-bit instruction.
    fn size(&self) -> u64 {
        if is_wide(self.first) { 4 } else { 2 }
    }

    /// Where the instruction after it starts.
    fn next(&self) -> u64 {
        self.address + self.size()
    }
}

/// The instruction at `address` in `program`.
fn decode_at(program: &dyn Program, address: u64) -> Option<Decoded> {
    let code = program.code(address)?;
    let half = |at: usize| Some(u16::from_le_bytes([*code.get(at)?, *code.get(at + 1)?]));
    let first = half(0)?;
    let second = if is_wide(first) { half(2)? } else { 0 };
    Some(Decoded {
        address,
        first,
        second,
        effect: decode::decode(address, first, second)?,
    })
}

/// The instructions from `start` on, one after another, up to the first
/// that doesn't decode.
fn straight_line(program: &dyn Program, start: u64) -> impl Iterator<Item = Decoded> + '_ {
    std::iter::successors(decode_at(program, start), move |instruction| {
        decode_at(program, instruction.next())
    })
}
