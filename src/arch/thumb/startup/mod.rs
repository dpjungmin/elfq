//! The startup walk (see `arch::startup`) through Thumb code.
//!
//! It is a second reader of what `decode` says each instruction does,
//! beside `values`. `values` follows every path through one function at
//! once, with every number a register may hold on any of them, to find the
//! addresses that code branches through. The walk goes one way, as the
//! processor would from reset, through the functions that the startup
//! code calls, so a register holds one number or one it can't tell, and
//! memory is at the addresses the code uses, the stack among them.

mod calls;
mod count;
mod each_time;
mod execute;
mod loops;
mod queries;
#[cfg(test)]
mod tests;
mod walk_state;

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use object::Endianness;

use super::decode::{self, Effect, Flow, LowBit, Operation, Registers, When};
use super::{LR, SP, Scanner, branch, is_wide, literal, sweep};
use crate::arch::CodeReferenceKind;
use crate::arch::startup::{Call, DEPTH, End, Event, How, Program, Start, Through, Walk};

use calls::{arguments, target};
use count::Counting;
use execute::holds;
pub(in crate::arch) use queries::{branches_past, passes_result};
use walk_state::{Alternative, Compare, Path, Visit};

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

/// How many instructions the walk goes through in the times of a loop
/// that it goes through each time, and to count the loops inside them, in
/// place of [`BUDGET`] and [`COUNT_BUDGET`]: a loop that costs more is
/// walked once, as others, and leaves the rest of the walk as it would be
/// had the walk not tried (see [`Walker::walk_times`]); and how many in
/// all for such loops, those it tries and gives up on too.
const TIMES_BUDGET: usize = 5_000;

const TIMES_COUNT_BUDGET: usize = 10_000;

const TIMES_TOTAL: usize = 2 * TIMES_BUDGET;

const TIMES_COUNT_TOTAL: usize = 2 * TIMES_COUNT_BUDGET;

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
        times_budget: TIMES_TOTAL,
        times_count_budget: TIMES_COUNT_TOTAL,
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
    /// What is left of [`TIMES_TOTAL`] and [`TIMES_COUNT_TOTAL`], from
    /// which the walk takes [`TIMES_BUDGET`] and [`TIMES_COUNT_BUDGET`] at
    /// most in place of `budget` and `count_budget` to go through a loop
    /// each time.
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

/// The instruction at `address` in `program`: its two halves and what it
/// does.
fn decode_at(program: &dyn Program, address: u64) -> Option<(u16, u16, Effect)> {
    let code = program.code(address)?;
    let half = |at: usize| Some(u16::from_le_bytes([*code.get(at)?, *code.get(at + 1)?]));
    let first = half(0)?;
    let second = if is_wide(first) { half(2)? } else { 0 };
    Some((first, second, decode::decode(address, first, second)?))
}
