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
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use object::{Endian, Endianness};

use super::decode::{self, Effect, Flow, LowBit, Operation, Registers, When, Words};
use super::{LR, PC, SP, Scanner, branch, is_wide, literal, sweep};
use crate::arch::CodeReferenceKind;
use crate::arch::startup::{
    Call, DEPTH, End, Event, How, Loop, LoopStore, Program, Source, Start, Store, Through, Time,
    Walk,
};
/// How far before a function's start the code of a loop can begin that
/// runs on into the function (see [`Walker::runs_into`]).
const RUNS_INTO: u64 = 0x40;
/// How many instructions the walk goes through, in all.
const BUDGET: usize = 20_000;
/// How many it goes through in the reset handler's own code, in each
/// function it calls, and in a loop's code.
const RESET_LIMIT: usize = 600;
const CALL_LIMIT: usize = 300;
const LOOP_LIMIT: usize = 60;
/// How many loops inside loops it walks the code of.
const NESTING: u32 = 8;
/// How many times at most a loop that the walk can tell runs: more is
/// not a loop of startup code.
const MOST_PASSES: u32 = 1 << 24;
/// How many instructions it goes through in all to count how many times
/// loops run, on top of [`BUDGET`].
const COUNT_BUDGET: usize = 50_000;
/// How many of the times a loop may leave, as its compares tell, it tries.
const CANDIDATES: usize = 12;
/// How many times at most a loop runs that the walk goes through the code
/// of each time with the calls it makes and the loops inside, as a loop
/// over a table of sections; and how many times it goes through a loop's
/// code to count it when its compares don't tell.
const MOST_TIMES: u32 = 16;
/// How many instructions the walk goes through in all in the times of the
/// loops that it goes through each time, and to count the loops inside
/// them, in place of [`BUDGET`] and [`COUNT_BUDGET`]: a loop that costs
/// more leaves the rest of the walk as it would be had the walk not gone
/// through it each time (see [`Walker::walk_times`]).
const TIMES_BUDGET: usize = 5_000;
const TIMES_COUNT_BUDGET: usize = 10_000;

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
        count_budget: COUNT_BUDGET,
        times_budget: TIMES_BUDGET,
        times_count_budget: TIMES_COUNT_BUDGET,
        counting: Vec::new(),
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
        pass: None,
        rows: None,
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
    /// Where the word in each register was loaded from, when the walk can
    /// tell and code hasn't changed the register since (see
    /// [`Store::source`]).
    sources: [Option<Source>; 16],
    /// The compares in one time through a loop's code that the walk
    /// counts (see [`Walker::count`]).
    compares: Vec<Compare>,
}

/// The two numbers that an instruction that sets the flags compares, each
/// when the walk can tell it: the flags say how the first compares with
/// the second.
#[derive(Debug, Clone, Copy)]
struct Compare {
    site: u64,
    numbers: [Option<u32>; 2],
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
    /// See `Path::sources`, `Path::left` and `Path::results`.
    sources: [Option<Source>; 16],
    left: u8,
    results: u8,
    /// How many events the walk had found.
    events: usize,
    /// How many instructions the walk had gone through (see `Run::order`).
    order: usize,
    /// See `Path::forks`.
    forks: bool,
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
    /// When it is one time through the code of a loop that the walk counts
    /// or goes through each time, which stops where the loop goes back or
    /// leaves: the loop's place in [`Walker::counting`].
    pass: Option<usize>,
    /// When its code is part of one time through a loop that the walk goes
    /// through each time, but for what that calls, as the code of a loop
    /// inside it is: the loop's place in [`Walker::counting`], which keeps
    /// the words its code loads (see [`Counting::loads`]).
    rows: Option<usize>,
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
    /// How many more it may go through to count how many times loops run.
    count_budget: usize,
    /// What is left of [`TIMES_BUDGET`] and [`TIMES_COUNT_BUDGET`], which
    /// take the place of `budget` and `count_budget` while the walk goes
    /// through a loop each time.
    times_budget: usize,
    times_count_budget: usize,
    /// The loops that the walk goes through time by time, the innermost
    /// last: one that it counts, or those that it goes through each time
    /// and loops inside them that it counts.
    counting: Vec<Counting>,
    /// The data in each function the walk got to, by where the function
    /// starts: the constants and jump tables its code reads, as offsets
    /// from its start.
    data: HashMap<u64, BTreeMap<usize, usize>>,
    /// What [`Walker::clobbers`] found for each function, by where it
    /// starts.
    clobbers: HashMap<u64, (Registers, u8)>,
}

/// A loop whose code the walk goes through time by time to count how many
/// times it runs (see [`Walker::count`]).
struct Counting {
    /// The branch back.
    back: u64,
    /// The instruction after it, where the loop leaves when it doesn't go
    /// back.
    next: u64,
    /// The instructions it went through the first time, and where the
    /// branches it didn't take then go: a later time that goes elsewhere
    /// leaves there, or goes a way the walk can't tell. Before the first
    /// time, the walk's own way through the code, when it went through it
    /// in the same run; otherwise empty, and the first time leaves past
    /// `back`.
    code: HashSet<u64>,
    ways_out: Vec<u64>,
    /// Where the time through the code stopped, and whether it goes back
    /// to the loop's head from there, when the walk can tell.
    stopped: Option<(u64, Option<bool>)>,
    /// Whether the times through the code follow calls, count the loops
    /// inside and keep their events, as for a loop that the walk goes
    /// through each time (see [`Walker::walk_times`]), rather than to
    /// count how many times it runs.
    walks: bool,
    /// The addresses that the time through the loop's own code loads words
    /// from, in the file and outside the stack, as a row of a table, when
    /// `walks`, each with the load instruction, so that a loop inside can
    /// drop its own: with those of the words that registers held at the
    /// head that it reads, as a loop does whose time loads the start of the
    /// next row at its end.
    loads: Vec<(u64, u32)>,
    /// Where the words that registers held at the head of the time were
    /// loaded from, for those that the time's code hasn't read or written
    /// yet.
    carried: [Option<Source>; 16],
    /// The function that the loop is in.
    bounds: Option<(u64, u64)>,
}

/// One time through a loop's code (see [`Walker::pass`]).
struct Pass {
    /// What the walk knows at the end.
    path: Path,
    stores: Vec<Store>,
    /// Whether it makes calls, which the walk doesn't follow, or goes
    /// into loops, which it doesn't count: then `stores` aren't all that
    /// the time stores.
    others: bool,
    compares: Vec<Compare>,
    /// Whether the loop goes back to its head after it, rather than
    /// leaving.
    again: bool,
    /// Where the loop goes when it leaves after it.
    exit: u64,
    /// The instructions it went through, and where the branches it
    /// didn't take go.
    order: Vec<u64>,
    ways_out: Vec<u64>,
    /// The addresses that it loaded words from, as [`Counting::loads`]
    /// says, the lowest first.
    loads: Vec<u32>,
}

/// What a loop leaves, as the walk counted it.
struct Counted {
    passes: u32,
    /// What the walk knows where the loop leaves, and where that is.
    path: Path,
    exit: u64,
    stores: Option<Vec<LoopStore>>,
    /// Whether the times through its code make calls or go into loops (see
    /// `Pass::others`).
    others: bool,
    /// Whether the walk went through the code every time the loop runs,
    /// so that what it knows where the loop leaves holds.
    every: bool,
}

/// What a loop that the walk went through each time leaves (see
/// [`Walker::walk_times`]).
struct Walked {
    path: Path,
    exit: u64,
    times: Vec<Time>,
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
        let (end, run) = self.run(start, frame, path);
        (end, run.path)
    }

    /// Walks as [`Walker::walk`] does, and returns how it ended and where
    /// it went.
    fn run(&mut self, start: u64, frame: Frame, path: Path) -> (Option<End>, Run) {
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
                Step::Stopped => return (None, run),
                Step::Final(end) => end,
                Step::End(end) => match self.backtrack(&mut run, frame, end) {
                    Some(target) => {
                        address = target;
                        continue;
                    }
                    None => end,
                },
            };
            return (Some(end), run);
        }
    }

    /// Runs the instruction at `address`.
    fn step(&mut self, run: &mut Run, frame: Frame, address: u64) -> Step {
        if let Some(index) = frame.pass {
            // At the branch back, or where the loop leaves: past it the
            // first time, and elsewhere than the code of the first time
            // later.
            let counting = &self.counting[index];
            let back = counting.back;
            let leaves = match counting.code.is_empty() {
                true => address > back,
                false => !counting.code.contains(&address),
            };
            if address == back || leaves {
                let again = match address == back {
                    true => self.goes_back(run, back),
                    false => (counting.code.is_empty() || counting.ways_out.contains(&address))
                        .then_some(false),
                };
                self.counting[index].stopped = Some((address, again));
                return Step::Stopped;
            }
        } else if frame.stop.is_some_and(|stop| address >= stop) {
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
        let visit = run.visit(self.events.len());
        match run.visits.entry(address) {
            Entry::Occupied(_) => return Step::Final(End::Rejoins(address)),
            Entry::Vacant(entry) => entry.insert(visit),
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
                if let Some(index) = frame.rows {
                    if !conditional {
                        self.row_load(run, index, address, &effect);
                    }
                    self.carried_reads(index, effect.reads, effect.writes, conditional);
                }
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
                // A call reads its arguments, in r0 to r3.
                if let Some(index) = frame.rows {
                    self.carried_reads(index, effect.reads | 0b1111, CLOBBERED, conditional);
                }
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
                if let When::Zero(register) | When::NotZero(register) = when
                    && !self.counting.is_empty()
                {
                    let number = self.register(&run.path, register, address);
                    run.path.compares.push(Compare {
                        site: address,
                        numbers: [number, Some(0)],
                    });
                }
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
            let exit = self.close_loop(run, frame, target, site, &visit, None, true);
            return match exit.or_else(|| run.way_out(target, site)) {
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
        let back =
            target <= site && (self.went_through(run, target) || self.runs_into(run, target));
        if run.leaves(target) && !back {
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
            let (visit, end, runs) = match run.visits.get(&target) {
                Some(visit) => (visit.clone(), None, true),
                // The code was jumped over to a test that doesn't go back:
                // the loop runs no times, as for an empty section.
                _ if taken == Some(false) => return Step::Next(next),
                _ => {
                    let before = run.visit(self.events.len());
                    match self.loop_body(run, frame, target, site) {
                        Some(End::Main(at)) => return Step::Final(End::Main(at)),
                        end => (before, end, taken == Some(true)),
                    }
                }
            };
            let exit = self.close_loop(run, frame, target, site, &visit, end, runs);
            return Step::Next(exit.unwrap_or(next));
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
                self.close_loop(run, frame, target, back, &before, end, false);
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

    /// Whether the code from `target` runs on into the function the walk
    /// is in, as a loop's code that comes just before the function does
    /// when a function finder takes the loop's test, which a call goes to,
    /// for the function's start.
    fn runs_into(&mut self, run: &Run, target: u64) -> bool {
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
    fn close_loop(
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
                first_event: before.events,
                end: None,
                passes: Some(counted.passes),
                stores: None,
                times: walked.times,
            }));
            return Some(walked.exit);
        }
        let exit = counted.as_ref().map(|counted| counted.exit);
        let (passes, stores) = match counted {
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
                (Some(passes), stores)
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
                (None, None)
            }
        };
        // The loads of a loop inside one that the walk goes through each
        // time may read what it copies, which can't be told from the rows
        // of a table when the walk can't tell what its stores copy from.
        if let Some(index) = frame.rows
            && !knows_sources(stores.as_deref())
        {
            (self.counting[index].loads).retain(|&(site, _)| !(head..=back).contains(&site));
        }
        self.events.push(Event::Loop(Loop {
            head,
            back,
            depth: frame.depth,
            first_event: before.events,
            end,
            passes,
            stores,
            times: Vec::new(),
        }));
        exit
    }

    /// How many times the loop from `head` to the branch back at `back`
    /// runs, what it leaves, and what its stores do, when the walk can
    /// tell. It goes through the loop's code from what it knew at the
    /// head, `before`, the first three times, as the processor would. Then
    /// each compare whose numbers move by the same amount each time tells
    /// when it may hold the other way, and the loop may leave; the first
    /// such time that the walk finds the loop leaves, going through the
    /// code that time from where each number would be, and goes back the
    /// time before, is when it leaves. When none does, it goes on through
    /// the code time by time, up to [`MOST_TIMES`] times, as for a loop
    /// over a table that ends at a zero word. Only a loop whose branch
    /// back goes to its head, as one with its code after the function's
    /// other code doesn't; the walk doesn't follow its calls, nor count
    /// loops inside it, and then can't tell what its stores do.
    fn count(
        &mut self,
        run: &Run,
        frame: Frame,
        head: u64,
        back: u64,
        before: &Visit,
    ) -> Option<Counted> {
        let counts = self.counting.iter().any(|counting| !counting.walks);
        if counts || self.count_budget == 0 {
            return None;
        }
        let counting = self.times_of(run, head, back, before, false)?;
        self.counting.push(counting);
        let budget = self.budget;
        let counted = self.count_passes(frame, head, before);
        self.budget = budget;
        self.counting.pop();
        counted
    }

    /// The loop from `head` to the branch back at `back`, to go through
    /// time by time from what the walk knew at its head the first time,
    /// `before`, as [`Counting`] says, with `walks`: `None` unless the
    /// branch back goes to the head.
    fn times_of(
        &self,
        run: &Run,
        head: u64,
        back: u64,
        before: &Visit,
        walks: bool,
    ) -> Option<Counting> {
        let (first, _, effect) = self.decode(back)?;
        match effect.flow {
            Flow::Branch { target, .. } if target == head => {}
            _ => return None,
        }
        // The code the walk went through from the head, which may lie
        // past the branch back, as GCC -Os puts a loop's test.
        let walked = match run.order.get(before.order) == Some(&head) {
            true => &run.order[before.order..],
            false => &[],
        };
        let code: HashSet<u64> = walked.iter().copied().collect();
        let ways_out = (run.pending.iter())
            .filter(|(from, to)| code.contains(from) && !code.contains(to))
            .map(|&(_, to)| to)
            .collect();
        Some(Counting {
            back,
            next: back + if is_wide(first) { 4 } else { 2 },
            code,
            ways_out,
            stopped: None,
            walks,
            loads: Vec::new(),
            carried: [None; 16],
            bounds: self.program.function_at(head),
        })
    }

    /// Goes through the code of the loop that the walk counts from
    /// `head`, from what the walk knew there the first time, `before`, as
    /// [`Walker::count`] says.
    fn count_passes(&mut self, frame: Frame, head: u64, before: &Visit) -> Option<Counted> {
        let mut start = first_time(before);
        let mut walked: Vec<(u32, Pass)> = Vec::new();
        for n in 1..=3 {
            let pass = self.pass(frame, head, start)?;
            if n == 1 {
                self.learn_code(&pass);
            }
            let again = pass.again;
            start = pass.path.clone();
            walked.push((n, pass));
            if !again {
                return Some(counted(walked, n));
            }
        }
        if let Some(line) = Line::new(&walked[0].1.path, &walked[1].1.path, &walked[2].1.path) {
            let (second, third) = (&walked[1].1, &walked[2].1);
            for n in leaving_times(second, third) {
                if !keeps_order(second, third, n - 1) {
                    break;
                }
                // The time before goes back, from where the line says.
                let before = match n {
                    4 => None,
                    _ => match self.pass(frame, head, line.start(n - 1)) {
                        Some(before) if before.again => Some(before),
                        _ => continue,
                    },
                };
                // Where the time before ends, which the line must go
                // through.
                let line_start = line.start(n);
                let ends = before
                    .as_ref()
                    .map_or(&walked[2].1.path, |before| &before.path);
                let on_line =
                    |r: usize| line_start.registers[r].is_none_or(|n| ends.registers[r] == Some(n));
                if !(0..16).all(on_line) {
                    continue;
                }
                match self.pass(frame, head, ends.clone()) {
                    Some(last) if !last.again => {
                        walked.extend(before.map(|before| (n - 1, before)));
                        walked.push((n, last));
                        return Some(counted(walked, n));
                    }
                    _ => continue,
                }
            }
        }
        // Time by time, as a loop whose compares tell nothing that moves
        // by the same amount, such as one over a table that ends at a row
        // of zeros.
        for n in 4..=MOST_TIMES {
            let pass = self.pass(frame, head, start)?;
            let again = pass.again;
            start = pass.path.clone();
            walked.push((n, pass));
            if !again {
                return Some(counted(walked, n));
            }
        }
        None
    }

    /// Where the innermost loop that the walk goes through time by time
    /// leaves, as the first time through its code, `first`, tells.
    fn learn_code(&mut self, first: &Pass) {
        if let Some(counting) = self.counting.last_mut() {
            counting.code = first.order.iter().copied().collect();
            counting.ways_out = first.ways_out.clone();
        }
    }

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
    /// than [`TIMES_BUDGET`] leaves; then the walk keeps the events of the
    /// first time.
    fn walk_times(
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
        if outermost {
            std::mem::swap(&mut self.budget, &mut self.times_budget);
            std::mem::swap(&mut self.count_budget, &mut self.times_count_budget);
        }
        self.counting.push(counting);
        let mut path = Path {
            sources: before.sources,
            ..first_time(before)
        };
        let mut times: Vec<Time> = Vec::new();
        let mut loads: Vec<Vec<u32>> = Vec::new();
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
            // loads, other than what the times so far copy.
            copied.extend(self.events[first_event..].iter().flat_map(copied_from));
            let row: Vec<u32> = (pass.loads.into_iter())
                .filter(|&at| {
                    !(copied.iter()).any(|(start, end)| (start..end).contains(&&u64::from(at)))
                })
                .collect();
            times.push(Time {
                first_event,
                row: row.first().copied(),
            });
            loads.push(row);
            path = pass.path;
            if n == passes {
                exit = Some(pass.exit);
            }
        }
        self.counting.pop();
        if outermost {
            std::mem::swap(&mut self.budget, &mut self.times_budget);
            std::mem::swap(&mut self.count_budget, &mut self.times_count_budget);
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
        if !steady || !same_words(&loads) {
            times.iter_mut().for_each(|time| time.row = None);
        }
        Some(Walked { path, exit, times })
    }

    /// Keeps where the load at `site` that does `effect`, in the time
    /// through the code of the loop at `index` in [`Walker::counting`]
    /// that the walk goes through, loads from (see [`Counting::loads`]).
    fn row_load(&mut self, run: &Run, index: usize, site: u64, effect: &Effect) {
        if !matches!(effect.operation, Operation::Load { .. }) {
            return;
        }
        let Some(at) = self.access(&run.path, site, effect) else {
            return;
        };
        if !self.is_stack(Some(at)) && self.program.word_at_reset(u64::from(at)).is_some() {
            self.counting[index].loads.push((site, at));
        }
    }

    /// Keeps where the words that registers held at the head of the time
    /// through the code of the loop at `index` in [`Walker::counting`]
    /// were loaded from, of those that an instruction that `reads` and
    /// `writes` these registers reads, as loads of the time, unless it is
    /// `conditional`, when the loop's function loaded them, rather than a
    /// function it calls; and forgets those of the registers it writes.
    fn carried_reads(
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
                self.counting[index].loads.push((site, address));
            }
        }
    }

    /// Goes through a loop's code once from `head`, from `path`, as one
    /// time the loop runs, where it stops at the branch back or where the
    /// loop leaves (see [`Counting`]), for the innermost loop that the
    /// walk goes through time by time: `None` when it goes past a branch
    /// that the walk can't tell, or the walk can't tell whether the loop
    /// goes back after it. To count the loop, it doesn't follow calls, and
    /// its events and instructions don't count for the walk; otherwise,
    /// they are the walk's.
    fn pass(&mut self, frame: Frame, head: u64, path: Path) -> Option<Pass> {
        let events = self.events.len();
        let lowest = self.lowest;
        let index = self.counting.len().checked_sub(1)?;
        let walks = self.counting[index].walks;
        let body = Frame {
            depth: frame.depth,
            follow: if walks { frame.follow } else { 0 },
            limit: match walks {
                true => LOOP_LIMIT,
                false => LOOP_LIMIT.min(self.count_budget),
            },
            stop: None,
            nesting: frame.nesting + 1,
            pass: Some(index),
            rows: walks.then_some(index),
        };
        if walks {
            self.counting[index].carried = path.sources;
        } else {
            self.budget = usize::MAX;
        }
        let (end, run) = self.run(head, body, path);
        let counting = &mut self.counting[index];
        let stopped = counting.stopped.take();
        let loads = std::mem::take(&mut counting.loads);
        let (back, next) = (counting.back, counting.next);
        let mut loads: Vec<u32> = loads.into_iter().map(|(_, at)| at).collect();
        loads.sort_unstable();
        loads.dedup();
        let mut stores = Vec::new();
        let mut others = false;
        if !walks {
            self.count_budget = self.count_budget.saturating_sub(run.count);
            self.lowest = lowest;
            for event in self.events.split_off(events) {
                match event {
                    Event::Store(store) if store.conditional => return None,
                    Event::Store(store) => stores.push(store),
                    _ => others = true,
                }
            }
        }
        let (Some((at, Some(again))), None, false) = (stopped, end, run.path.forks) else {
            return None;
        };
        let exit = if at == back { next } else { at };
        let mut path = run.path;
        let compares = std::mem::take(&mut path.compares);
        Some(Pass {
            path,
            stores,
            others,
            compares,
            again,
            exit,
            order: run.order,
            ways_out: run.pending.iter().map(|&(_, to)| to).collect(),
            loads,
        })
    }

    /// Whether the branch back at `back` goes back to the loop's head, as
    /// the walk is there at the end of `run`, when it can tell.
    fn goes_back(&self, run: &Run, back: u64) -> Option<bool> {
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
        let at = self.access(path, site, effect);
        if let Operation::Store { .. } = effect.operation {
            self.store(path, site, depth, effect, at, conditional);
        }
        if changes_flags {
            self.compare(path, site, effect.operation);
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
        let result = self.result(path, site, effect.operation, at);
        let source = match effect.operation {
            Operation::Copy { source, .. } if source != PC => path.sources[usize::from(source)],
            Operation::Load {
                register: Some(_), ..
            } => at.map(|address| Source { site, address }),
            _ => None,
        };
        // The words that `ldm` and `ldrd` load, with where from.
        let words: Vec<(u16, Option<u32>, Option<Source>)> = match effect.operation {
            Operation::Load { .. } => (effect.words.into_iter().flat_map(word_registers))
                .enumerate()
                .map(|(n, register)| {
                    let address = at.map(|at| at.wrapping_add(4 * n as u32));
                    let word = address.and_then(|address| self.load(path, address, false));
                    let source = address.map(|address| Source { site, address });
                    (register, word, source)
                })
                .collect(),
            _ => Vec::new(),
        };
        // The base register after a load or store that adds to it.
        let moved = match (effect.operation, effect.writeback) {
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
        if let Some((register, _)) = result {
            path.sources[usize::from(register)] = source;
        }
        for (register, word, source) in words {
            path.registers[usize::from(register)] = word;
            path.sources[usize::from(register)] = source;
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
    /// when the walk can tell it. A load loads it from `at`.
    fn result(
        &self,
        path: &Path,
        site: u64,
        operation: Operation,
        at: Option<u32>,
    ) -> Option<(u16, Option<u32>)> {
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
            Operation::AddShifted {
                register,
                a,
                b,
                shift,
                subtract,
            } => {
                let b = get(b).map(|b| b << shift);
                let result = get(a).zip(b).map(|(a, b)| match subtract {
                    true => a.wrapping_sub(b),
                    false => a.wrapping_add(b),
                });
                number(register, result)
            }
            Operation::Negate { register, source } => {
                number(register, get(source).map(u32::wrapping_neg))
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
                ..
            } => number(register, at.and_then(|at| self.load(path, at, false))),
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
            Operation::AddShifted {
                a,
                b,
                shift,
                subtract: minus,
                ..
            } => {
                let b = get(b).map(|b| b << shift);
                if minus {
                    subtract(get(a), b)
                } else {
                    add(get(a), b)
                }
            }
            Operation::Negate { source, .. } => subtract(Some(0), get(source)),
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

    /// Records the store at `site`, which does what `effect` says, at
    /// `address` when the walk can tell it, and changes the memory that
    /// `path` knows.
    fn store(
        &mut self,
        path: &mut Path,
        site: u64,
        depth: u32,
        effect: &Effect,
        address: Option<u32>,
        conditional: bool,
    ) {
        let Operation::Store {
            register,
            base,
            size,
            ..
        } = effect.operation
        else {
            return;
        };
        let from = self.register(path, base, site);
        // The registers whose words it stores, in order: none for part of
        // a word.
        let registers: Vec<u16> = match (effect.words, register) {
            (Some(words), _) => word_registers(words),
            (None, Some(register)) => vec![register],
            (None, None) => Vec::new(),
        };
        let words: Vec<Option<u32>> = (registers.iter())
            .map(|&register| self.register(path, register, site))
            .collect();
        if let Some(address) = address {
            let memory = Rc::make_mut(&mut path.memory);
            if !words.is_empty() && address.is_multiple_of(4) && !conditional {
                for (n, &word) in words.iter().enumerate() {
                    memory.insert(address.wrapping_add(4 * n as u32), word);
                }
            } else {
                // Part of a word, or what the walk can't tell: the words
                // it stores in are unknown.
                let last = address.wrapping_add(size.max(1) - 1) & !3;
                let mut word = address & !3;
                memory.insert(word, None);
                while word != last {
                    word = word.wrapping_add(4);
                    memory.insert(word, None);
                }
            }
        }
        // Stores to the stack are judged by where they go, not by the
        // register they go through: unoptimized code keeps a pointer to
        // its locals in r7.
        let stack = base == SP || self.is_stack(address.or(from));
        if !stack {
            let value = match words.split_first() {
                Some((&first, rest)) if rest.iter().all(|&word| word == first) => first,
                _ => None,
            };
            // Of several words, only when it loaded them one after another.
            let sources: Vec<Option<Source>> = (registers.iter())
                .map(|&r| path.sources[usize::from(r)])
                .collect();
            let source = sources.first().copied().flatten().filter(|first| {
                (sources.iter().enumerate()).all(|(n, source)| {
                    source.is_some_and(|source| {
                        source.address == first.address.wrapping_add(4 * n as u32)
                    })
                })
            });
            self.events.push(Event::Store(Store {
                site,
                depth,
                address,
                base: from,
                size,
                value,
                source,
                conditional,
            }));
        }
    }

    /// The address of the first word that a load or store at `site` that
    /// does what `effect` says reads or writes, when the walk can tell it.
    fn access(&self, path: &Path, site: u64, effect: &Effect) -> Option<u32> {
        let (Operation::Load { base, offset, .. } | Operation::Store { base, offset, .. }) =
            effect.operation
        else {
            return None;
        };
        // `ldrd` from pc reads from the instruction's address + 4, rounded
        // down to a word.
        let base = match base {
            PC => (site as u32).wrapping_add(4) & !3,
            _ => self.register(path, base, site)?,
        };
        if let Some(index) = effect.index {
            let added = self.register(path, index.register, site)?;
            return Some(base.wrapping_add(added << index.shift));
        }
        let offset = match (effect.words, effect.writeback) {
            (Some(Words::List { registers, down }), _) => match down {
                true => -4 * registers.count_ones() as i32,
                false => 0,
            },
            (Some(Words::Pair { offset, .. }), _) => offset,
            // `[base, #imm]!` adds before it loads or stores, `[base],
            // #imm` after.
            (None, Some(writeback)) if writeback.before => writeback.amount,
            (None, Some(_)) => 0,
            (None, None) => offset?,
        };
        Some(base.wrapping_add_signed(offset))
    }

    /// Records the numbers that an instruction that does `operation` and
    /// sets the flags compares, while the walk counts a loop (see
    /// [`Walker::count`]): `cmp a, b` compares `a` and `b`; `adds` and
    /// `subs` compare what they add to with what they add, negated, as
    /// the carry tells, and what they leave with 0; and any other
    /// instruction compares what it leaves with 0, as the negative and
    /// zero flags tell.
    fn compare(&self, path: &mut Path, site: u64, operation: Operation) {
        if self.counting.is_empty() {
            return;
        }
        let get = |register| self.register(path, register, site);
        let left = || match self.result(path, site, operation, None) {
            Some((_, number)) => [Some([number, Some(0)]), None],
            None => [None, None],
        };
        let compares = match operation {
            Operation::Compare { register, value } => [Some([get(register), Some(value)]), None],
            Operation::CompareRegisters { a, b } => [Some([get(a), get(b)]), None],
            Operation::Subtract { a, b, .. } => {
                let difference = get(a).zip(get(b)).map(|(a, b)| a.wrapping_sub(b));
                [Some([get(a), get(b)]), Some([difference, Some(0)])]
            }
            Operation::AddConstant { source, amount, .. } => {
                let sum = get(source).map(|n| n.wrapping_add(amount));
                [
                    Some([get(source), Some(amount.wrapping_neg())]),
                    Some([sum, Some(0)]),
                ]
            }
            _ => left(),
        };
        let compares = compares.into_iter().flatten();
        path.compares
            .extend(compares.map(|numbers| Compare { site, numbers }));
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
    /// `literal`, as if copied into place, unless code stored a word the
    /// walk knows there.
    fn load(&self, path: &Path, address: u32, literal: bool) -> Option<u32> {
        if !address.is_multiple_of(4) {
            return None;
        }
        let code = self.program.code(u64::from(address)).filter(|_| literal);
        match (
            path.memory.get(&address),
            code.and_then(|bytes| bytes.get(..4)),
        ) {
            (Some(&Some(word)), _) => Some(word),
            (_, Some(bytes)) => Some(self.endianness.read_u32(bytes.try_into().ok()?)),
            (Some(None), None) => None,
            (None, None) => self.program.word_at_reset(u64::from(address)),
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
            sources: self.path.sources,
            left: self.path.left,
            results: self.path.results,
            events,
            order: self.order.len(),
            forks: self.path.forks,
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

/// Where what a loop's code changes is at the start of each time it runs,
/// from the second on, as the ends of the first three times tell: each
/// register, address a register's word was loaded from, and word in
/// memory either stays, or moves by the same amount each time, or the walk
/// can't tell it, as a word that code loaded into a register.
struct Line {
    /// What the walk knows at the start of the second time.
    start: Path,
    /// How far each register moves each time, when by the same amount.
    registers: [Option<u32>; 16],
    /// How far the address each register's word was loaded from moves
    /// each time, when by the same amount (see `Path::sources`).
    sources: [Option<u32>; 16],
    /// The words in memory that change, and how far each moves each time,
    /// when by the same amount.
    memory: Vec<(u32, Option<u32>)>,
}

impl Line {
    /// The line through the ends of the first three times: `None` when
    /// the stack pointer moves.
    fn new(first: &Path, second: &Path, third: &Path) -> Option<Line> {
        let sp = usize::from(SP);
        if first.registers[sp] != second.registers[sp]
            || second.registers[sp] != third.registers[sp]
        {
            return None;
        }
        // A word that code loaded moves as the memory it came from does,
        // not by the same amount each time.
        let registers = std::array::from_fn(|r| {
            let loaded = [first, second, third]
                .iter()
                .any(|path| path.sources[r].is_some());
            let [a, b, c] = [first, second, third].map(|path| path.registers[r]);
            if loaded { None } else { moves(a, b, c) }
        });
        let sources = std::array::from_fn(|r| {
            let [a, b, c] = [first, second, third].map(|path| path.sources[r]);
            match (a, b, c) {
                (Some(a), Some(b), Some(c)) if a.site == b.site && b.site == c.site => {
                    moves(Some(a.address), Some(b.address), Some(c.address))
                }
                _ => None,
            }
        });
        let mut memory = Vec::new();
        if !(Rc::ptr_eq(&first.memory, &second.memory) && Rc::ptr_eq(&second.memory, &third.memory))
        {
            let addresses: BTreeSet<u32> = [first, second, third]
                .iter()
                .flat_map(|path| path.memory.keys().copied())
                .collect();
            for address in addresses {
                let [a, b, c] =
                    [first, second, third].map(|path| path.memory.get(&address).copied());
                if a == b && b == c {
                    continue;
                }
                let by = match (a, b, c) {
                    (Some(a), Some(b), Some(c)) => moves(a, b, c),
                    _ => None,
                };
                memory.push((address, by));
            }
        }
        let mut start = first.clone();
        if !(first.flags == second.flags && second.flags == third.flags) {
            start.flags = Flags::default();
        }
        Some(Line {
            start,
            registers,
            sources,
            memory,
        })
    }

    /// What the walk knows at the start of the `n`th time, from the
    /// second on.
    fn start(&self, n: u32) -> Path {
        let times = n - 2;
        let at =
            |from: Option<u32>, by: Option<u32>| Some(from?.wrapping_add(by?.wrapping_mul(times)));
        let mut path = self.start.clone();
        for r in 0..16 {
            path.registers[r] = at(path.registers[r], self.registers[r]);
            path.sources[r] = path.sources[r].and_then(|source| {
                let address = at(Some(source.address), self.sources[r])?;
                Some(Source { address, ..source })
            });
        }
        if !self.memory.is_empty() {
            let memory = Rc::make_mut(&mut path.memory);
            for &(address, by) in &self.memory {
                let word = memory.get(&address).copied().flatten();
                memory.insert(address, at(word, by));
            }
        }
        path
    }
}

/// How far `a`, `b` and `c` move each time, when by the same amount: 0
/// when the walk can tell none of them.
fn moves(a: Option<u32>, b: Option<u32>, c: Option<u32>) -> Option<u32> {
    match (a, b, c) {
        (None, None, None) => Some(0),
        (Some(a), Some(b), Some(c)) if b.wrapping_sub(a) == c.wrapping_sub(b) => {
            Some(b.wrapping_sub(a))
        }
        _ => None,
    }
}

/// The numbers that each compare of `second` and `third`, two times in a
/// row through a loop's code, compares, with what they are the first of
/// those times and how far they move each time, as numbers with no sign
/// or, with `signed`, with one, when the walk can tell them: those of the
/// compares that both times make in the same order.
fn compared(second: &Pass, third: &Pass, signed: bool) -> Vec<[(i64, i64); 2]> {
    let view = |n: u32| match signed {
        true => i64::from(n as i32),
        false => i64::from(n),
    };
    let same = |(a, b): &(&Compare, &Compare)| a.site == b.site;
    let pairs = second.compares.iter().zip(&third.compares);
    pairs
        .take_while(same)
        .filter_map(|(a, b)| {
            let ([Some(a1), Some(b1)], [Some(a2), Some(b2)]) = (a.numbers, b.numbers) else {
                return None;
            };
            let by = |from: u32, to: u32| i64::from(to.wrapping_sub(from) as i32);
            Some([(view(a1), by(a1, a2)), (view(b1), by(b1, b2))])
        })
        .collect()
}

/// When a loop may leave, from the fourth time on, as the compares of the
/// second and third times through its code tell: around each time when
/// one of them may hold the other way, as the gap between its two
/// numbers closes, taken with no sign or with one. At most [`CANDIDATES`]
/// of them, the earliest first.
fn leaving_times(second: &Pass, third: &Pass) -> Vec<u32> {
    let mut times = BTreeSet::new();
    for signed in [false, true] {
        for [(a, a_by), (b, b_by)] in compared(second, third, signed) {
            let (gap, closes) = (a - b, b_by - a_by);
            // The gap closes after `gap / closes` times from the second.
            if closes == 0 || gap.signum() != closes.signum() {
                continue;
            }
            let Ok(after) = u32::try_from(gap / closes) else {
                continue;
            };
            let crossing = after.saturating_add(2);
            for n in crossing.saturating_sub(1)..=crossing.saturating_add(2) {
                if (4..=MOST_PASSES).contains(&n) {
                    times.insert(n);
                }
            }
        }
    }
    times.into_iter().take(CANDIDATES).collect()
}

/// Whether the numbers that each compare of `second` and `third`, the
/// second and third times through a loop's code, compares keep moving
/// without wrapping round up to the `last`th time, with no sign and with
/// one: then each compare's flags change only where the gap between its
/// numbers closes, where [`leaving_times`] looks.
fn keeps_order(second: &Pass, third: &Pass, last: u32) -> bool {
    let times = i64::from(last - 2);
    [
        (false, 0, u32::MAX as i64),
        (true, i32::MIN as i64, i32::MAX as i64),
    ]
    .into_iter()
    .all(|(signed, low, high)| {
        compared(second, third, signed).into_iter().all(|numbers| {
            numbers.into_iter().all(|(from, by)| {
                let to = from + by * times;
                (low..=high).contains(&to)
            })
        })
    })
}

/// What a loop leaves after it runs `passes` times, with the times through
/// its code that the walk went through, `walked`, by number, the last one
/// last.
fn counted(walked: Vec<(u32, Pass)>, passes: u32) -> Counted {
    let stores = summaries(&walked, passes);
    let others = walked.iter().any(|(_, pass)| pass.others);
    let every = walked.len() == passes as usize;
    let (_, last) = walked.into_iter().next_back().expect("the last time");
    Counted {
        passes,
        path: last.path,
        exit: last.exit,
        stores,
        others,
        every,
    }
}

/// What the walk knows at the head of a loop the first time through its
/// code, as it was there, `before`, to go through the code time by time:
/// not where words in registers were loaded from, nor the flags.
fn first_time(before: &Visit) -> Path {
    Path {
        registers: before.registers,
        memory: Rc::clone(&before.memory),
        flags: Flags::default(),
        forks: false,
        left: before.left,
        results: before.results,
        sources: [None; 16],
        compares: Vec::new(),
    }
}

/// What each store of a loop that runs `passes` times does over all the
/// times, from those that the walk went through, `walked`: when each time
/// but the last stores with the same instructions as the first, and the
/// last with the first of them, and the walk can tell where each stores,
/// which moves by the same amount each time.
fn summaries(walked: &[(u32, Pass)], passes: u32) -> Option<Vec<LoopStore>> {
    if walked.iter().any(|(_, pass)| pass.others) {
        return None;
    }
    let first = &walked.first()?.1.stores;
    let sites = |pass: &Pass| {
        pass.stores
            .iter()
            .map(|store| store.site)
            .collect::<Vec<_>>()
    };
    let all = sites(&walked[0].1);
    let same = walked.iter().all(|(n, pass)| match *n == passes {
        true => all.starts_with(&sites(pass)),
        false => sites(pass) == all,
    });
    if !same {
        return None;
    }
    (first.iter().enumerate())
        .map(|(i, store)| {
            let each: Vec<(u32, &Store)> = (walked.iter())
                .filter_map(|(n, pass)| Some((*n, pass.stores.get(i)?)))
                .collect();
            let (address, step) = line_through(each.iter().map(|&(n, store)| (n, store.address)))?;
            if each.iter().any(|(_, other)| other.size != store.size) || store.size == 0 {
                return None;
            }
            let value = (each.iter())
                .all(|(_, other)| other.value == store.value)
                .then_some(store.value)
                .flatten();
            let source_at = |store: &Store| store.source.map(|source| source.address);
            let loaded = !each
                .iter()
                .all(|(_, other)| source_at(other) == source_at(store));
            let source = line_through(each.iter().map(|&(n, store)| (n, source_at(store))));
            let last = walked
                .last()
                .map(|(_, pass)| pass.stores.len())
                .unwrap_or(0);
            Some(LoopStore {
                site: store.site,
                address,
                step,
                size: store.size,
                times: passes - 1 + u32::from(i < last),
                value,
                source,
                loaded,
            })
        })
        .collect()
}

/// Where `points`, the `n`th time's number for some `n`, the first first,
/// start and how far they move each time, when the walk can tell each and
/// they move by the same amount.
fn line_through(mut points: impl Iterator<Item = (u32, Option<u32>)>) -> Option<(u32, i32)> {
    let (1, Some(first)) = points.next()? else {
        return None;
    };
    let mut step = None;
    for (n, number) in points {
        let number = number?;
        let times = (n - 1) as i32;
        let by = *step.get_or_insert_with(|| number.wrapping_sub(first) as i32 / times);
        if first.wrapping_add_signed(by.wrapping_mul(times)) != number {
            return None;
        }
    }
    Some((first, step.unwrap_or(0)))
}

/// Forgets the words in memory that a loop that runs `passes` times may
/// have stored to on the times that the walk didn't go through, from the
/// fourth to the one before the one before the last: those that its
/// `stores` cover, or when the walk can't tell them, those that changed
/// since `before`, at its head.
fn forget_stored(path: &mut Path, passes: u32, stores: Option<&[LoopStore]>, before: &Visit) {
    if passes <= 5 {
        return;
    }
    let Some(stores) = stores else {
        let registers = path.registers;
        forget_changes(path, &registers, &before.memory);
        return;
    };
    for store in stores {
        let span = i64::from(store.step) * i64::from(store.times - 1);
        let ends = [i64::from(store.address), i64::from(store.address) + span];
        let low = ends[0].min(ends[1]) & !3;
        let high = ends[0].max(ends[1]) + i64::from(store.size) - 1;
        let (Ok(low), Ok(high)) = (u32::try_from(low), u32::try_from(high)) else {
            continue;
        };
        let words: Vec<u32> = path
            .memory
            .range(low..=high)
            .map(|(&address, _)| address)
            .collect();
        if !words.is_empty() {
            let memory = Rc::make_mut(&mut path.memory);
            words
                .into_iter()
                .for_each(|address| _ = memory.insert(address, None));
        }
    }
}

/// Forgets what changed in `path`, but for the stack pointer, since it
/// held `registers` and `memory`, and where words were loaded from.
fn forget_changes(path: &mut Path, registers: &[Option<u32>; 16], memory: &Memory) {
    path.sources = [None; 16];
    for register in (0..16).filter(|&register| register != usize::from(SP)) {
        if path.registers[register] != registers[register] {
            path.registers[register] = None;
        }
    }
    let changed = stored_since(&path.memory, memory);
    forget_words(path, changed);
}

/// The words that code stored from when memory held `before` to when it
/// held `after`.
fn stored_since(after: &Memory, before: &Memory) -> Vec<u32> {
    if Rc::ptr_eq(after, before) {
        return Vec::new();
    }
    (after.iter())
        .filter(|&(address, &word)| before.get(address) != Some(&word))
        .map(|(&address, _)| address)
        .chain(
            (before.keys())
                .filter(|address| !after.contains_key(address))
                .copied(),
        )
        .collect()
}

/// Forgets the words at `addresses` in `path`.
fn forget_words(path: &mut Path, addresses: Vec<u32>) {
    if addresses.is_empty() {
        return;
    }
    let words = Rc::make_mut(&mut path.memory);
    for address in addresses {
        words.insert(address, None);
    }
}

/// Forgets what `registers` hold.
fn set_unknown(path: &mut Path, registers: Registers) {
    for register in each(registers) {
        path.registers[usize::from(register)] = None;
        path.sources[usize::from(register)] = None;
    }
}

/// The registers whose words a load or store of more than one word loads
/// or stores, in order.
fn word_registers(words: Words) -> Vec<u16> {
    match words {
        Words::List { registers, .. } => each(registers).collect(),
        Words::Pair { first, second, .. } => vec![first, second],
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
fn same_words(loads: &[Vec<u32>]) -> bool {
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
fn knows_sources(stores: Option<&[LoopStore]>) -> bool {
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

    use super::{Flags, add, holds, same_words, subtract, walk};
    use crate::arch::startup::{DEPTH, Event, How, Program, Start, Walk};

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

    /// The walk from 0x8000000 through `units`, as `walk_lines` says, that
    /// follows calls `follow` deep.
    fn walk_units(units: &[u16], functions: &[u64], main: Option<u64>, follow: u32) -> Walk {
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
        walk(Little, &flash, &start)
    }

    /// What the stores of each loop that the walk through `units` counts
    /// do, as `walk_lines` says: a line for each, as "0x8000010: 4 bytes
    /// at 0x20000000 +4n, 4 times, of ?, from 0x8000100 +4n, loaded".
    fn loop_stores(units: &[u16], functions: &[u64], main: Option<u64>) -> Vec<String> {
        let walk = walk_units(units, functions, main, DEPTH);
        let number = |number: Option<u32>| number.map_or("?".to_string(), |n| format!("{n:#x}"));
        (walk.events.iter())
            .filter_map(|event| match event {
                Event::Loop(l) => l.stores.as_ref(),
                _ => None,
            })
            .flatten()
            .map(|store| {
                let source = store.source.map_or(String::new(), |(address, step)| {
                    format!(", from {address:#x} {step:+}n")
                });
                let loaded = if store.loaded { ", loaded" } else { "" };
                format!(
                    "{:#x}: {} bytes at {:#x} {:+}n, {} times, of {}{source}{loaded}",
                    store.site,
                    store.size,
                    store.address,
                    store.step,
                    store.times,
                    number(store.value)
                )
            })
            .collect()
    }

    /// As `walk_lines`, for a walk that follows calls `follow` deep.
    fn walk_lines_following(
        units: &[u16],
        functions: &[u64],
        main: Option<u64>,
        follow: u32,
    ) -> Vec<String> {
        let walk = walk_units(units, functions, main, follow);
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
                    let source = store.source.map_or(String::new(), |source| {
                        format!(", loaded from {:#x} at {:#x}", source.address, source.site)
                    });
                    let (depth, site) = (store.depth, store.site);
                    format!(
                        "{depth} {site:#x} store {value} at {place}, {size} bytes{condition}{source}"
                    )
                }
                Event::Loop(l) => {
                    let end = l
                        .end
                        .map_or(String::new(), |end| format!(", stops {end:x?}"));
                    let passes = l.passes.map_or(String::new(), |n| format!(", runs {n:#x} times"));
                    // Where each time the walk went through starts, and the
                    // row of a table it reads.
                    let times: Vec<String> = (l.times.iter())
                        .map(|time| match time.row {
                            Some(row) => format!(" {}@{row:#x}", time.first_event),
                            None => format!(" {}", time.first_event),
                        })
                        .collect();
                    let times = match times.is_empty() {
                        true => String::new(),
                        false => format!(", times from events{}", times.concat()),
                    };
                    format!(
                        "{} {:#x} loop back at {:#x}, from event {}{passes}{times}{end}",
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
        // The loop is entered at its test, at the end of its code, and runs
        // 4 times, which leaves 0x10 in r3; this memset leaves the end in
        // r0:
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
                "0 0x800000e store ? at 0x20000000, 4 bytes, loaded from 0x8000100 at 0x800000c",
                "0 0x800000c loop back at 0x8000016, from event 0, runs 0x4 times",
                "0 0x800001e call 0x8000030 (0x20000010 0x0 0x20 0x10)",
                "1 0x8000036 store ? at 0x20000010, 1 bytes",
                "1 0x8000032 loop back at 0x800003a, from event 3, runs 0x21 times",
                "0 0x8000022 call 0x800003e (0x20000030 0x0 0x20000030 0x10), left by a call: r0 r2",
                "end Main(8000022)",
            ]
        );
        assert_eq!(
            loop_stores(&code, &functions, Some(0x0800_003e)),
            [
                "0x800000e: 4 bytes at 0x20000000 +4n, 4 times, of ?, from 0x8000100 +4n, loaded",
                "0x8000036: 1 bytes at 0x20000010 +1n, 32 times, of ?"
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
        // the second runs twice, up to r1:
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
                "0 0x8000020 loop back at 0x8000026, from event 0, runs 0x2 times",
                "0 0x8000028 call 0x8000030 (0x20000008 0x20000008 0x0 ?)",
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
        // first ends, at 0x20000010, as the walk tells from the pointer on
        // the stack, which moves by 4 up to the bound it is compared with.
        // Stores through r7 to the stack aren't listed:
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
                "0 0x800000e loop back at 0x800001e, from event 0, runs 0x4 times",
                "0 0x800002a store 0x0 at 0x20000010, 4 bytes",
                "0 0x8000022 loop back at 0x8000032, from event 2, runs 0x4 times",
                "0 0x8000034 call 0x8000040 (? ? 0x20000020 0x20000020)",
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
        // regions, calling a function for each to copy or zero it: the
        // walk goes through the loop's code the one time it runs, which
        // reads the first row, with the function, and leaves at the table's
        // end, where the registers hold the row, since the function changes
        // none of them:
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
        //   8000020: 08000027   .word
        // main:
        //   8000024: e7fe       b.n   8000024
        // copy:
        //   8000026: 4770       bx    lr
        let code = [
            0xa404, 0xa508, 0xe001, 0xcc0f, 0x4798, 0x42ac, 0xd3fb, 0xf000, 0xf809, 0xbf00, 0x0100,
            0x0800, 0x0000, 0x2000, 0x0010, 0x0000, 0x0027, 0x0800, 0xe7fe, 0x4770,
        ];
        let functions = [0x0800_0000, 0x0800_0024, 0x0800_0026];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0024)),
            [
                "0 0x8000008 call through r3 0x8000026 (0x8000100 0x20000000 0x10 0x8000027)",
                "0 0x8000006 loop back at 0x800000c, from event 0, runs 0x1 times, times from events 0@0x8000014",
                "0 0x800000e call 0x8000024 (0x8000100 0x20000000 0x10 0x8000027)",
                "end Main(800000e)",
            ]
        );
    }

    #[test]
    fn a_region_table_loop_whose_function_the_walk_cant_tell_is_walked_once() {
        // As armlib's __scatterload sets bit 0 of each row's function with
        // `orr`, which the walk doesn't follow: it can't tell that the
        // rows name the same function, so it goes through the loop once,
        // as through one over a table of functions:
        // reset:
        //   8000000: 4c0a       ldr   r4, [pc, #40]  ; 0x800002c
        //   8000002: 4d0b       ldr   r5, [pc, #44]  ; 0x8000030
        // loop:
        //   8000004: 68e0       ldr   r0, [r4, #12]
        //   8000006: f040 0301  orr.w r3, r0, #1
        //   800000a: e894 0007  ldmia.w r4, {r0, r1, r2}
        //   800000e: 4798       blx   r3
        //   8000010: 3410       adds  r4, #16
        //   8000012: 42ac       cmp   r4, r5
        //   8000014: d3f6       bcc.n 8000004 <loop>
        //   8000016: f000 f808  bl    800002a <main>
        // copy:
        //   800001a: b12a       cbz   r2, 8000028
        //   800001c: f850 3b04  ldr.w r3, [r0], #4
        //   8000020: f841 3b04  str.w r3, [r1], #4
        //   8000024: 3a04       subs  r2, #4
        //   8000026: d1f9       bne.n 800001c
        //   8000028: 4770       bx    lr
        // main:
        //   800002a: e7fe       b.n   800002a <main>
        //   800002c: 08000034   .word  ; table
        //   8000030: 08000054   .word  ; its end
        // table: two rows of (load image, start, size, copy)
        //   8000034: 08000100 20000000 00000010 0800001a
        //   8000044: 08000110 20000010 00000010 0800001a
        let code = [
            0x4c0a, 0x4d0b, 0x68e0, 0xf040, 0x0301, 0xe894, 0x0007, 0x4798, 0x3410, 0x42ac, 0xd3f6,
            0xf000, 0xf808, 0xb12a, 0xf850, 0x3b04, 0xf841, 0x3b04, 0x3a04, 0xd1f9, 0x4770, 0xe7fe,
            0x0034, 0x0800, 0x0054, 0x0800, 0x0100, 0x0800, 0x0000, 0x2000, 0x0010, 0x0000, 0x001a,
            0x0800, 0x0110, 0x0800, 0x0010, 0x2000, 0x0010, 0x0000, 0x001a, 0x0800,
        ];
        let functions = [0x0800_0000, 0x0800_001a, 0x0800_002a];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_002a)),
            [
                "0 0x800000e call through r3 ? (0x8000100 0x20000000 0x10 ?)",
                "0 0x8000004 loop back at 0x8000014, from event 0, runs 0x2 times",
                "0 0x8000016 call 0x800002a (? ? ? ?)",
                "end Main(8000016)",
            ]
        );
    }

    #[test]
    fn a_table_loop_that_loads_the_next_row_at_its_end() {
        // The code loads the first row before the loop, and the next at
        // the end of each time, which the test then reads: each time's
        // row is the one that the registers hold at its head, which the
        // call reads, not the one that it loads:
        // reset:
        //   8000000: 4c08       ldr   r4, [pc, #32]  ; 0x8000024
        //   8000002: cc07       ldmia r4!, {r0, r1, r2}
        // loop:
        //   8000004: f000 f805  bl    8000012 <copy>
        //   8000008: cc07       ldmia r4!, {r0, r1, r2}
        //   800000a: 2a00       cmp   r2, #0
        //   800000c: d1fa       bne.n 8000004 <loop>
        //   800000e: f000 f807  bl    8000020 <main>
        // copy:
        //   8000012: f850 3b04  ldr.w r3, [r0], #4
        //   8000016: f841 3b04  str.w r3, [r1], #4
        //   800001a: 3a04       subs  r2, #4
        //   800001c: d1f9       bne.n 8000012 <copy>
        //   800001e: 4770       bx    lr
        // main:
        //   8000020: e7fe       b.n   8000020 <main>
        //   8000024: 08000028   .word  ; table
        // table: two rows of (load image, start, size), then zeros
        //   8000028: 08000100 20000000 00000010
        //   8000034: 08000110 20000010 00000010
        //   8000040: 00000000 00000000 00000000
        let code = [
            0x4c08, 0xcc07, 0xf000, 0xf805, 0xcc07, 0x2a00, 0xd1fa, 0xf000, 0xf807, 0xf850, 0x3b04,
            0xf841, 0x3b04, 0x3a04, 0xd1f9, 0x4770, 0xe7fe, 0x0000, 0x0028, 0x0800, 0x0100, 0x0800,
            0x0000, 0x2000, 0x0010, 0x0000, 0x0110, 0x0800, 0x0010, 0x2000, 0x0010, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
        ];
        let functions = [0x0800_0000, 0x0800_0012, 0x0800_0020];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0020)),
            [
                "0 0x8000004 call 0x8000012 (0x8000100 0x20000000 0x10 ?)",
                "1 0x8000016 store ? at 0x20000000, 4 bytes, loaded from 0x8000100 at 0x8000012",
                "1 0x8000012 loop back at 0x800001c, from event 1, runs 0x4 times",
                "0 0x8000004 call 0x8000012 (0x8000110 0x20000010 0x10 ?)",
                "1 0x8000016 store ? at 0x20000010, 4 bytes, loaded from 0x8000110 at 0x8000012",
                "1 0x8000012 loop back at 0x800001c, from event 4, runs 0x4 times",
                "0 0x8000004 loop back at 0x800000c, from event 0, runs 0x2 times, times from events 0@0x8000028 3@0x8000034",
                "0 0x800000e call 0x8000020 (0x0 0x0 0x0 ?)",
                "end Main(800000e)",
            ]
        );
    }

    #[test]
    fn loops_that_cost_more_than_the_times_budget_leave_the_walk_its_own() {
        // Three loops of 16 times, one inside the other through calls, cost
        // more to go through each time than the times budget lets, so the
        // walk goes through the outer one once, as it would through a loop
        // that it doesn't go through each time, and still has its own
        // budget for the copy after it:
        // reset:
        //   8000000: 2600       movs  r6, #0
        //   8000002: f000 f80f  bl    8000024 <f>
        //   8000006: 3601       adds  r6, #1
        //   8000008: 2e10       cmp   r6, #16
        //   800000a: d1fa       bne.n 8000002
        //   800000c: 4811       ldr   r0, [pc, #68]  ; 0x8000054
        //   800000e: f04f 5100  mov.w r1, #0x20000000
        //   8000012: 2210       movs  r2, #16
        //   8000014: f850 3b04  ldr.w r3, [r0], #4
        //   8000018: f841 3b04  str.w r3, [r1], #4
        //   800001c: 3a04       subs  r2, #4
        //   800001e: d1f9       bne.n 8000014
        //   8000020: f000 f817  bl    8000052 <main>
        // f:
        //   8000024: b510       push  {r4, lr}
        //   8000026: 2400       movs  r4, #0
        //   8000028: f000 f804  bl    8000034 <g>
        //   800002c: 3401       adds  r4, #1
        //   800002e: 2c10       cmp   r4, #16
        //   8000030: d1fa       bne.n 8000028
        //   8000032: bd10       pop   {r4, pc}
        // g: as f, calling h
        //   8000034: b520 2500 f000 f804 3501 2d10 d1fa bd20
        // h:
        //   8000044: f04f 4180  mov.w r1, #0x40000000
        //   8000048: 6008       str   r0, [r1, #0]
        //   800004a: 6048       str   r0, [r1, #4]
        //   800004c: 6088       str   r0, [r1, #8]
        //   800004e: 60c8       str   r0, [r1, #12]
        //   8000050: 4770       bx    lr
        // main:
        //   8000052: e7fe       b.n   8000052 <main>
        //   8000054: 08000100   .word
        let code = [
            0x2600, 0xf000, 0xf80f, 0x3601, 0x2e10, 0xd1fa, 0x4811, 0xf04f, 0x5100, 0x2210, 0xf850,
            0x3b04, 0xf841, 0x3b04, 0x3a04, 0xd1f9, 0xf000, 0xf817, 0xb510, 0x2400, 0xf000, 0xf804,
            0x3401, 0x2c10, 0xd1fa, 0xbd10, 0xb520, 0x2500, 0xf000, 0xf804, 0x3501, 0x2d10, 0xd1fa,
            0xbd20, 0xf04f, 0x4180, 0x6008, 0x6048, 0x6088, 0x60c8, 0x4770, 0xe7fe, 0x0100, 0x0800,
        ];
        let functions = [
            0x0800_0000,
            0x0800_0024,
            0x0800_0034,
            0x0800_0044,
            0x0800_0052,
        ];
        let lines = walk_lines(&code, &functions, Some(0x0800_0052));
        assert_eq!(
            lines[lines.len() - 5..],
            [
                "0 0x8000002 loop back at 0x800000a, from event 0, runs 0x10 times",
                "0 0x8000018 store ? at 0x20000000, 4 bytes, loaded from 0x8000100 at 0x8000014",
                "0 0x8000014 loop back at 0x800001e, from event 1315, runs 0x4 times",
                "0 0x8000020 call 0x8000052 (0x8000110 0x20000010 0x0 ?)",
                "end Main(8000020)",
            ]
        );
    }

    #[test]
    fn each_time_reads_the_same_words_of_its_row() {
        // Three words of each row, and then only the first, as of the zero
        // word that ends the table; times that load nothing don't count.
        let rows = [
            vec![0x100, 0x104, 0x108],
            vec![0x10c, 0x110, 0x114],
            vec![0x118],
        ];
        assert!(same_words(&rows));
        assert!(same_words(&[
            vec![0x100, 0x104],
            vec![],
            vec![0x10c, 0x110]
        ]));
        // A first time that also loads the start of the next row at its
        // end, which the next time doesn't load again, or a time that
        // reads another word of its row.
        let rows = [
            vec![0x100, 0x104, 0x108, 0x10c],
            vec![0x110, 0x114, 0x118],
            vec![0x11c],
        ];
        assert!(!same_words(&rows));
        assert!(!same_words(&[
            vec![0x100, 0x104],
            vec![0x10c, 0x114],
            vec![0x118]
        ]));
        assert!(!same_words(&[vec![0x100], vec![0x10c], vec![0x118, 0x11c]]));
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
                "0 0x8000008 store 0x100 at 0x20000004, 4 bytes, loaded from 0x8000024 at 0x8000004",
                "0 0x8000012 store ? at 0x20000008, 4 bytes, loaded from 0x20000000 at 0x8000010",
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
                "0 0x8000006 store ? at 0x20000004, 4 bytes, loaded from 0x20000000 at 0x8000004",
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

    #[test]
    fn a_loop_that_copies_two_words_at_a_time_with_ldm_and_stm() {
        // The loop runs twice, and leaves r0 and r2 past what it copied:
        //   8000000: f04f 5000  mov.w r0, #0x20000000
        //   8000004: 4903       ldr   r1, [pc, #12]  ; 0x20000010
        //   8000006: 4a04       ldr   r2, [pc, #16]  ; 0x8000100
        //   8000008: ca18       ldmia r2!, {r3, r4}
        //   800000a: c018       stmia r0!, {r3, r4}
        //   800000c: 4288       cmp   r0, r1
        //   800000e: d3fb       bcc.n 8000008
        //   8000010: f000 f804  bl    800001c <main>
        //   8000014: 20000010   .word
        //   8000018: 08000100   .word
        // main:
        //   800001c: e7fe       b.n   800001c
        let code = [
            0xf04f, 0x5000, 0x4903, 0x4a04, 0xca18, 0xc018, 0x4288, 0xd3fb, 0xf000, 0xf804, 0x0010,
            0x2000, 0x0100, 0x0800, 0xe7fe, 0xbf00,
        ];
        let functions = [0x0800_0000, 0x0800_001c];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_001c)),
            [
                "0 0x800000a store ? at 0x20000000, 8 bytes, loaded from 0x8000100 at 0x8000008",
                "0 0x8000008 loop back at 0x800000e, from event 0, runs 0x2 times",
                "0 0x8000010 call 0x800001c (0x20000010 0x20000010 0x8000110 ?)",
                "end Main(8000010)",
            ]
        );
        assert_eq!(
            loop_stores(&code, &functions, Some(0x0800_001c)),
            ["0x800000a: 8 bytes at 0x20000000 +8n, 2 times, of ?, from 0x8000100 +8n, loaded"]
        );
    }

    #[test]
    fn a_loop_that_counts_down_to_zero() {
        // r3 counts the bytes left, from 16 down to 0, and the loop copies
        // the last word first:
        //   8000000: 4904       ldr   r1, [pc, #16]  ; 0x8000100
        //   8000002: f04f 5200  mov.w r2, #0x20000000
        //   8000006: 2310       movs  r3, #16
        //   8000008: 3b04       subs  r3, #4
        //   800000a: 58c8       ldr   r0, [r1, r3]
        //   800000c: 50d0       str   r0, [r2, r3]
        //   800000e: dcfb       bgt.n 8000008
        //   8000010: f000 f802  bl    8000018 <main>
        //   8000014: 08000100   .word
        // main:
        //   8000018: e7fe       b.n   8000018
        let code = [
            0x4904, 0xf04f, 0x5200, 0x2310, 0x3b04, 0x58c8, 0x50d0, 0xdcfb, 0xf000, 0xf802, 0x0100,
            0x0800, 0xe7fe, 0xbf00,
        ];
        let functions = [0x0800_0000, 0x0800_0018];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0018)),
            [
                "0 0x800000c store ? at 0x2000000c, 4 bytes, loaded from 0x800010c at 0x800000a",
                "0 0x8000008 loop back at 0x800000e, from event 0, runs 0x4 times",
                "0 0x8000010 call 0x8000018 (? 0x8000100 0x20000000 0x0)",
                "end Main(8000010)",
            ]
        );
        assert_eq!(
            loop_stores(&code, &functions, Some(0x0800_0018)),
            ["0x800000c: 4 bytes at 0x2000000c -4n, 4 times, of ?, from 0x800010c -4n, loaded"]
        );
    }

    #[test]
    fn two_loops_that_share_the_register_they_copy_from() {
        // The second loop copies from where the first left r1, as startup
        // code does when one load image follows the other:
        //   8000000: 490a       ldr   r1, [pc, #40]  ; 0x8000100
        //   8000002: f04f 5200  mov.w r2, #0x20000000
        //   8000006: 4b0a       ldr   r3, [pc, #40]  ; 0x20000008
        //   8000008: 429a       cmp   r2, r3
        //   800000a: bfbe       ittt  lt
        //   800000c: f851 0b04  ldrlt.w r0, [r1], #4
        //   8000010: f842 0b04  strlt.w r0, [r2], #4
        //   8000014: e7f8       blt.n 8000008
        //   8000016: 4a07       ldr   r2, [pc, #28]  ; 0x20000100
        //   8000018: 4b07       ldr   r3, [pc, #28]  ; 0x2000010c
        //   800001a: 429a       cmp   r2, r3
        //   800001c: bfbe       ittt  lt
        //   800001e: f851 0b04  ldrlt.w r0, [r1], #4
        //   8000022: f842 0b04  strlt.w r0, [r2], #4
        //   8000026: e7f8       blt.n 800001a
        //   8000028: f000 f808  bl    800003c <main>
        //   800002c: 08000100 20000008 20000100 2000010c   .word
        // main:
        //   800003c: e7fe       b.n   800003c
        let code = [
            0x490a, 0xf04f, 0x5200, 0x4b0a, 0x429a, 0xbfbe, 0xf851, 0x0b04, 0xf842, 0x0b04, 0xe7f8,
            0x4a07, 0x4b07, 0x429a, 0xbfbe, 0xf851, 0x0b04, 0xf842, 0x0b04, 0xe7f8, 0xf000, 0xf808,
            0x0100, 0x0800, 0x0008, 0x2000, 0x0100, 0x2000, 0x010c, 0x2000, 0xe7fe, 0xbf00,
        ];
        let functions = [0x0800_0000, 0x0800_003c];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_003c)),
            [
                "0 0x8000010 store ? at 0x20000000, 4 bytes, loaded from 0x8000100 at 0x800000c",
                "0 0x8000008 loop back at 0x8000014, from event 0, runs 0x3 times",
                "0 0x8000022 store ? at 0x20000100, 4 bytes, loaded from 0x8000108 at 0x800001e",
                "0 0x800001a loop back at 0x8000026, from event 2, runs 0x4 times",
                "0 0x8000028 call 0x800003c (? 0x8000114 0x2000010c 0x2000010c)",
                "end Main(8000028)",
            ]
        );
    }

    #[test]
    fn a_loop_that_runs_too_many_times_to_count() {
        // r0 counts up to 0xffffffff, more times than elfq counts, so the
        // walk can't tell what the loop leaves in it:
        //   8000000: f04f 5200  mov.w r2, #0x20000000
        //   8000004: 2000       movs  r0, #0
        //   8000006: f04f 31ff  mov.w r1, #0xffffffff
        //   800000a: 6010       str   r0, [r2, #0]
        //   800000c: 3001       adds  r0, #1
        //   800000e: 4288       cmp   r0, r1
        //   8000010: d1fb       bne.n 800000a
        //   8000012: f000 f800  bl    8000016 <main>
        // main:
        //   8000016: e7fe       b.n   8000016
        let code = [
            0xf04f, 0x5200, 0x2000, 0xf04f, 0x31ff, 0x6010, 0x3001, 0x4288, 0xd1fb, 0xf000, 0xf800,
            0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0016];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0016)),
            [
                "0 0x800000a store 0x0 at 0x20000000, 4 bytes",
                "0 0x800000a loop back at 0x8000010, from event 0",
                "0 0x8000012 call 0x8000016 (? 0xffffffff 0x20000000 ?)",
                "end Main(8000012)",
            ]
        );
    }

    #[test]
    fn a_table_loop_whose_test_is_past_its_branch_back() {
        // As CMSIS 5.9's __cmsis_start goes through its copy table, built
        // with GCC -Os, which puts the test of the loop over the table's
        // rows after the branch back: the table has one row, which copies
        // 4 words, and the loop leaves the second time. The walk goes
        // through the loop's code each time, and the first time reads the
        // row from its start, though the code reads the row's last word
        // first:
        // reset:
        //   8000000: 4b0a       ldr	r3, [pc, #40]	@ (800002c <body+0x6>)
        //   8000002: 490b       ldr	r1, [pc, #44]	@ (8000030 <body+0xa>)
        //   8000004: b510       push	{r4, lr}
        // outer:
        //   8000006: 428b       cmp	r3, r1
        //   8000008: d30d       bcc.n	8000026 <body>
        //   800000a: f000 f819  bl	8000040 <main>
        // inner:
        //   800000e: 6818       ldr	r0, [r3, #0]
        //   8000010: f850 4022  ldr.w	r4, [r0, r2, lsl #2]
        //   8000014: 6858       ldr	r0, [r3, #4]
        //   8000016: f840 4022  str.w	r4, [r0, r2, lsl #2]
        //   800001a: 3201       adds	r2, #1
        // test:
        //   800001c: 6898       ldr	r0, [r3, #8]
        //   800001e: 4290       cmp	r0, r2
        //   8000020: d8f5       bhi.n	800000e <inner>
        //   8000022: 330c       adds	r3, #12
        //   8000024: e7ef       b.n	8000006 <outer>
        // body:
        //   8000026: 2200       movs	r2, #0
        //   8000028: e7f8       b.n	800001c <test>
        //   800002c: 08000034   .word	0x08000034
        //   8000030: 08000040   .word	0x08000040
        // table:
        //   8000034: 08000100   .word	0x08000100
        //   8000038: 20000000   .word	0x20000000
        //   800003c: 00000004   .word	0x00000004
        // main:
        //   8000040: e7fe       b.n	8000040 <main>
        let code = [
            0x4b0a, 0x490b, 0xb510, 0x428b, 0xd30d, 0xf000, 0xf819, 0x6818, 0xf850, 0x4022, 0x6858,
            0xf840, 0x4022, 0x3201, 0x6898, 0x4290, 0xd8f5, 0x330c, 0xe7ef, 0x2200, 0xe7f8, 0xbf00,
            0x0034, 0x0800, 0x0040, 0x0800, 0x0100, 0x0800, 0x0000, 0x2000, 0x0004, 0x0000, 0xe7fe,
            0xbf00,
        ];
        let functions = [0x0800_0000, 0x0800_0040];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0040)),
            [
                "0 0x8000016 store ? at 0x20000000, 4 bytes, loaded from 0x8000100 at 0x8000010",
                "0 0x800000e loop back at 0x8000020, from event 0, runs 0x4 times",
                "0 0x8000006 loop back at 0x8000024, from event 0, runs 0x2 times, times from events 0@0x8000034 2",
                "0 0x800000a call 0x8000040 (0x4 0x8000040 0x4 0x8000040)",
                "end Main(800000a)",
            ]
        );
        assert_eq!(
            loop_stores(&code, &functions, Some(0x0800_0040)),
            ["0x8000016: 4 bytes at 0x20000000 +4n, 4 times, of ?, from 0x8000100 +4n, loaded"]
        );
    }

    #[test]
    fn a_loop_over_a_table_of_functions_is_walked_once() {
        // As __libc_init_array calls the constructors in .init_array: the
        // loop calls another function through r3 each time, so the walk
        // keeps what it found the first time, as for a loop it doesn't go
        // through each time, and doesn't follow the second function:
        // reset:
        //   8000000: 4c08       ldr	r4, [pc, #32]	@ (8000024 <second+0x8>)
        //   8000002: 4d09       ldr	r5, [pc, #36]	@ (8000028 <second+0xc>)
        // loop:
        //   8000004: 42ac       cmp	r4, r5
        //   8000006: d202       bcs.n	800000e <done>
        //   8000008: cc08       ldmia	r4!, {r3}
        //   800000a: 4798       blx	r3
        //   800000c: e7fa       b.n	8000004 <loop>
        // done:
        //   800000e: f000 f813  bl	8000038 <main>
        // first:
        //   8000012: f04f 5000  mov.w	r0, #536870912	@ 0x20000000
        //   8000016: 2101       movs	r1, #1
        //   8000018: 6001       str	r1, [r0, #0]
        //   800001a: 4770       bx	lr
        // second:
        //   800001c: 4803       ldr	r0, [pc, #12]	@ (800002c <second+0x10>)
        //   800001e: 2102       movs	r1, #2
        //   8000020: 6001       str	r1, [r0, #0]
        //   8000022: 4770       bx	lr
        //   8000024: 08000030   .word	0x08000030
        //   8000028: 08000038   .word	0x08000038
        //   800002c: 20000004   .word	0x20000004
        // table:
        //   8000030: 08000013   .word	0x08000013
        //   8000034: 0800001d   .word	0x0800001d
        // main:
        //   8000038: e7fe       b.n	8000038 <main>
        let code = [
            0x4c08, 0x4d09, 0x42ac, 0xd202, 0xcc08, 0x4798, 0xe7fa, 0xf000, 0xf813, 0xf04f, 0x5000,
            0x2101, 0x6001, 0x4770, 0x4803, 0x2102, 0x6001, 0x4770, 0x0030, 0x0800, 0x0038, 0x0800,
            0x0004, 0x2000, 0x0013, 0x0800, 0x001d, 0x0800, 0xe7fe, 0xbf00,
        ];
        let functions = [0x0800_0000, 0x0800_0012, 0x0800_001c, 0x0800_0038];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0038)),
            [
                "0 0x800000a call through r3 0x8000012 (? ? ? 0x8000013)",
                "1 0x8000018 store 0x1 at 0x20000000, 4 bytes",
                "0 0x8000004 loop back at 0x800000c, from event 0, runs 0x3 times",
                "0 0x800000e call 0x8000038 (? ? ? ?)",
                "end Main(800000e)",
            ]
        );
    }

    #[test]
    fn a_loop_that_leaves_with_cbz_at_its_head() {
        // r3 counts the bytes left down to 0, which `cbz` tests before the
        // loop copies each word:
        // reset:
        //   8000000: 4906       ldr	r1, [pc, #24]	@ (800001c <done+0x6>)
        //   8000002: f04f 5200  mov.w	r2, #536870912	@ 0x20000000
        //   8000006: 2310       movs	r3, #16
        // loop:
        //   8000008: b12b       cbz	r3, 8000016 <done>
        //   800000a: f851 0b04  ldr.w	r0, [r1], #4
        //   800000e: f842 0b04  str.w	r0, [r2], #4
        //   8000012: 3b04       subs	r3, #4
        //   8000014: e7f8       b.n	8000008 <loop>
        // done:
        //   8000016: f000 f803  bl	8000020 <main>
        //   800001c: 08000100   .word	0x08000100
        // main:
        //   8000020: e7fe       b.n	8000020 <main>
        let code = [
            0x4906, 0xf04f, 0x5200, 0x2310, 0xb12b, 0xf851, 0x0b04, 0xf842, 0x0b04, 0x3b04, 0xe7f8,
            0xf000, 0xf803, 0xbf00, 0x0100, 0x0800, 0xe7fe, 0xbf00,
        ];
        let functions = [0x0800_0000, 0x0800_0020];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0020)),
            [
                "0 0x800000e store ? at 0x20000000, 4 bytes, loaded from 0x8000100 at 0x800000a",
                "0 0x8000008 loop back at 0x8000014, from event 0, runs 0x5 times",
                "0 0x8000016 call 0x8000020 (? 0x8000110 0x20000010 0x0)",
                "end Main(8000016)",
            ]
        );
        assert_eq!(
            loop_stores(&code, &functions, Some(0x0800_0020)),
            ["0x800000e: 4 bytes at 0x20000000 +4n, 4 times, of ?, from 0x8000100 +4n, loaded"]
        );
    }

    #[test]
    fn a_loop_counts_without_the_words_it_loads() {
        // The words the loop copies run 1, 2, 3, 4, then 9: the walk
        // doesn't take the word in r0 to move on by 1 each time, as the
        // registers that count do:
        // reset:
        //   8000000: a106       add	r1, pc, #24	@ (adr r1, 800001c <data>)
        //   8000002: f04f 5200  mov.w	r2, #536870912	@ 0x20000000
        //   8000006: 4b04       ldr	r3, [pc, #16]	@ (8000018 <loop+0x10>)
        // loop:
        //   8000008: f851 0b04  ldr.w	r0, [r1], #4
        //   800000c: f842 0b04  str.w	r0, [r2], #4
        //   8000010: 429a       cmp	r2, r3
        //   8000012: d3f9       bcc.n	8000008 <loop>
        //   8000014: f000 f80e  bl	8000034 <main>
        //   8000018: 20000018   .word	0x20000018
        // data:
        //   800001c: 00000001   .word	0x00000001
        //   8000020: 00000002   .word	0x00000002
        //   8000024: 00000003   .word	0x00000003
        //   8000028: 00000004   .word	0x00000004
        //   800002c: 00000009   .word	0x00000009
        //   8000030: 00000006   .word	0x00000006
        // main:
        //   8000034: e7fe       b.n	8000034 <main>
        let code = [
            0xa106, 0xf04f, 0x5200, 0x4b04, 0xf851, 0x0b04, 0xf842, 0x0b04, 0x429a, 0xd3f9, 0xf000,
            0xf80e, 0x0018, 0x2000, 0x0001, 0x0000, 0x0002, 0x0000, 0x0003, 0x0000, 0x0004, 0x0000,
            0x0009, 0x0000, 0x0006, 0x0000, 0xe7fe, 0xbf00,
        ];
        let functions = [0x0800_0000, 0x0800_0034];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0034)),
            [
                "0 0x800000c store 0x1 at 0x20000000, 4 bytes, loaded from 0x800001c at 0x8000008",
                "0 0x8000008 loop back at 0x8000012, from event 0, runs 0x6 times",
                "0 0x8000014 call 0x8000034 (0x6 0x8000034 0x20000018 0x20000018)",
                "end Main(8000014)",
            ]
        );
        assert_eq!(
            loop_stores(&code, &functions, Some(0x0800_0034)),
            ["0x800000c: 4 bytes at 0x20000000 +4n, 6 times, of ?, from 0x800001c +4n, loaded"]
        );
    }

    #[test]
    fn numbers_from_adds_of_shifted_registers_and_negs() {
        // reset:
        //   8000000: 2103       movs	r1, #3
        //   8000002: f04f 5200  mov.w	r2, #536870912	@ 0x20000000
        //   8000006: eb02 0081  add.w	r0, r2, r1, lsl #2
        //   800000a: 424b       negs	r3, r1
        //   800000c: 6003       str	r3, [r0, #0]
        //   800000e: eba0 0081  sub.w	r0, r0, r1, lsl #2
        //   8000012: 6001       str	r1, [r0, #0]
        //   8000014: f000 f800  bl	8000018 <main>
        // main:
        //   8000018: e7fe       b.n	8000018 <main>
        let code = [
            0x2103, 0xf04f, 0x5200, 0xeb02, 0x0081, 0x424b, 0x6003, 0xeba0, 0x0081, 0x6001, 0xf000,
            0xf800, 0xe7fe,
        ];
        let functions = [0x0800_0000, 0x0800_0018];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_0018)),
            [
                "0 0x800000c store 0xfffffffd at 0x2000000c, 4 bytes",
                "0 0x8000012 store 0x3 at 0x20000000, 4 bytes",
                "0 0x8000014 call 0x8000018 (0x20000000 0x3 0x20000000 0xfffffffd)",
                "end Main(8000014)",
            ]
        );
    }

    #[test]
    fn a_store_of_several_words_copies_only_words_loaded_one_after_another() {
        //   8000000: 4905       ldr   r1, [pc, #20]  ; 0x8000100
        //   8000002: f04f 5000  mov.w r0, #0x20000000
        //   8000006: 680b       ldr   r3, [r1, #0]
        //   8000008: 688c       ldr   r4, [r1, #8]
        //   800000a: c018       stmia r0!, {r3, r4}
        //   800000c: 684b       ldr   r3, [r1, #4]
        //   800000e: 688c       ldr   r4, [r1, #8]
        //   8000010: c018       stmia r0!, {r3, r4}
        //   8000012: f000 f803  bl    800001c <main>
        //   8000018: 08000100   .word
        // main:
        //   800001c: e7fe       b.n   800001c
        let code = [
            0x4905, 0xf04f, 0x5000, 0x680b, 0x688c, 0xc018, 0x684b, 0x688c, 0xc018, 0xf000, 0xf803,
            0xbf00, 0x0100, 0x0800, 0xe7fe, 0xbf00,
        ];
        let functions = [0x0800_0000, 0x0800_001c];
        assert_eq!(
            walk_lines(&code, &functions, Some(0x0800_001c)),
            [
                "0 0x800000a store ? at 0x20000000, 8 bytes",
                "0 0x8000010 store ? at 0x20000008, 8 bytes, loaded from 0x8000104 at 0x800000c",
                "0 0x8000012 call 0x800001c (0x20000010 0x8000100 ? ?)",
                "end Main(8000012)",
            ]
        );
    }
}
