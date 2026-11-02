//! Where a branch through a register goes, when code put an address there.
//!
//! A `bl` reaches 16 MiB on either side, so a call to code further away,
//! as from RAM to flash, goes through a register:
//!
//! ```text
//! ldr   r3, [pc, #8]    @ the address of flash_leaf, from a literal pool
//! blx   r3
//! ```
//!
//! Compilers do this for `long_call` functions, and linkers put stubs called
//! veneers next to a `bl` that can't reach, which do it for the `bl`. The
//! sweep in `thumb/sweep.rs` reads each instruction on its own, so it sees
//! an address loaded and a call through a pointer. This module follows the
//! numbers that a function puts in its registers and on its stack, through
//! its branches and loops, and reports such a branch as a call or a jump to
//! each address the register can hold there. It keeps apart the paths where
//! an `it` block's condition holds and where it doesn't, until the flags it
//! tests change, since a later `it` block may test them again.
//!
//! It follows the ways compilers and linkers build an address: loading it
//! from a literal pool, `adr`, `movw` and `movt`, `movs` with shifts and
//! adds as on Cortex-M0, and adding the program counter to an offset as
//! position-independent code does. It follows numbers that code stores on
//! its stack and loads back, as unoptimized code does with every local
//! variable, and as LLD's Cortex-M0 veneers do with their target before
//! popping it into pc. A register that may hold more than [`LIMIT`](state::LIMIT)
//! numbers, or a number loaded from anywhere else in memory, holds
//! something elfq can't tell; a branch through it stays a branch through a
//! pointer.
//!
//! An address that code only branches to isn't also reported as an
//! address it uses, as it would be if the code passed it to a function or
//! read memory there.
//!
//! It assumes the code follows the procedure call standard for Arm: a call
//! keeps r4 to r11, sp and the caller's stack as they were, unless the
//! caller passes the function an address on its stack, and returns its
//! result in r0. A call may also keep r1 to r3 and ip, and code built with
//! GCC's `-fipa-ra` counts on that when it knows the function, so a branch
//! through one of them after a call may go to the address it held, or
//! anywhere. Such code may keep r0 too, but code reads r0 after a call
//! for the result far more often, so elfq takes r0 to hold the result,
//! and only counts the address it held as used where code branches
//! through r0. When such code passes the address on without naming it
//! again, to another function or back to the caller, elfq misses that
//! use: what other code leaves in those registers after a call is nothing
//! a function could take.
//!
//! Most functions only call with `bl` and return, so it follows only those
//! where it may find more than the sweep (see `may_find_more`).
//!
//! How it works:
//!
//! 1. [`Code::new`] lays the code out: what each instruction it reaches
//!    does, which instructions an `it` block makes conditional, where each
//!    branch inside the function goes, and the blocks of code that run from
//!    start to end. It gives up on most functions here, where following the
//!    registers can't find more than the sweep, and on code it can't
//!    follow.
//! 2. [`Function::run`] runs each block from what the registers and the
//!    stack may hold where it starts, passing what it ends with on to the
//!    blocks it goes to, until that changes nowhere. Contents that may hold
//!    more than [`LIMIT`](state::LIMIT) numbers stop gaining any, so this
//!    ends.
//! 3. A last pass runs each block once more from those states, to find
//!    where each branch through a register goes, the addresses that each
//!    instruction loads or builds, and how the code uses them
//!    ([`AddressUses`]); [`Found`] lists what it does that the others
//!    don't. The passes before it note in the [`AddressUses`] the
//!    addresses they stop following, as where paths join, and those that
//!    the code loads back from the stack.
//! 4. [`Function::references`] puts what it found in place of the sweep's
//!    references, at the instructions it reached.
//!
//! `state.rs` holds what the registers and the stack may hold, `code.rs`
//! the layout, `address_uses.rs` how the code uses each address, and
//! `tests.rs` the tests on hand-assembled functions.
//!
//! # Following another instruction
//!
//! To follow what another instruction does with the registers:
//!
//! 1. In `decode.rs`, decode it to an [`Operation`] of its own, with a
//!    test of its bytes.
//! 2. In [`Function::execute`], work out what it puts in the register it
//!    writes.
//! 3. In [`Function::escapes`], list the registers it reads in a way that
//!    `execute` follows, so that an address on the stack in one doesn't
//!    count as out of sight.
//! 4. In [`Function::report`], record how it uses what it reads: as an
//!    address, as part of one, or by passing it on.
//! 5. In [`Function::builds_address`], say whether it builds an address,
//!    and if it does, list it in `may_find_more`, in `code.rs`, so that
//!    elfq follows the functions that have it.
//! 6. In `startup`, say what it puts in the register (`Walker::result`),
//!    and, where the walk can tell, how it sets the flags
//!    (`Walker::flags`) and what it compares (`Walker::compare`).
//!
//! The compiler points to each of these but `may_find_more`,
//! `Walker::flags` and `Walker::compare`, which list only what they
//! follow.

mod address_uses;
mod code;
mod state;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use object::Endianness;

use self::address_uses::{AddressUses, Use};
use self::code::{Code, Condition};
use self::state::{Contents, Kind, RESULT, State, Value};
use super::decode::{Addressing, Effect, Flow, LowBit, Operation, Transfer, When, bit, each};
use super::{ARGUMENTS, Instruction, PC, SP};
use crate::arch::CodeReference;
use crate::arch::CodeReferenceKind::{self, *};

/// The references in a function's code, given the instructions the sweep
/// decoded and the references it found: those of the sweep, with each
/// branch through a register, and each address loaded or built, replaced
/// by what following the registers finds. When elfq can't follow the code,
/// or following it can't find more, as [`Code::new`] lists, the sweep's
/// references are returned as they are. Code that only a jump elfq doesn't
/// follow leads to keeps the sweep's references.
pub(super) fn follow(
    code: &[u8],
    address: u64,
    endianness: Endianness,
    instructions: &[Instruction],
    references: Vec<CodeReference>,
) -> Vec<CodeReference> {
    let Some(code) = Code::new(code, address, endianness, instructions, &references) else {
        return references;
    };
    let mut function = Function::new(&code);
    let found = function.run();
    function.references(&found, references)
}

/// What a branch through a register at one instruction may go to.
#[derive(Debug, Default)]
struct Branch {
    targets: BTreeSet<u32>,
    /// Whether it may also go somewhere elfq can't tell.
    unknown: bool,
}

/// What the last pass, from the states that the blocks start with once
/// they hold nothing new, finds. Only that pass:
///
/// - records where each branch through a register may go, in `branches`
///   (`branch`), and how each instruction uses the numbers it reads, in
///   the [`AddressUses`] (`report`);
/// - records the addresses that each instruction loads or builds
///   (`execute`);
/// - records what the code passes on where it runs on past what elfq was
///   given (`leave_block`);
/// - passes nothing on to the blocks the code goes to (`reach`), since what
///   they start with is final.
#[derive(Default)]
struct Found {
    /// What each branch through a register may go to, by instruction.
    branches: BTreeMap<usize, Branch>,
    /// The addresses each instruction that loads or builds one may give,
    /// by instruction.
    addresses: BTreeMap<usize, BTreeSet<u32>>,
}

/// Following the registers through one function's code.
struct Function<'a> {
    code: &'a Code<'a>,
    /// What the registers and the stack hold at the start of each block,
    /// where the code reaches it.
    states: Vec<Option<Box<State>>>,
    /// The blocks to run again, since what they start with changed.
    to_run: BTreeSet<usize>,
    /// How the code uses the addresses it loads or builds.
    uses: AddressUses,
    /// What the last pass finds, while it runs.
    found: Option<Found>,
}

/// Where the code is, as [`Function::run_block`] goes through a block.
/// That keeps one and changes it in place, so its size costs nothing.
#[allow(clippy::large_enum_variant)]
enum At {
    /// On one path.
    One(State),
    /// On the two paths of an `it` block.
    Split(Split),
    /// Nowhere: the code doesn't go on.
    Stopped,
}

impl At {
    /// Splits the one path the code is on in two, at an `it` on
    /// `condition`.
    fn split(&mut self, condition: u16) {
        if let At::One(state) = std::mem::replace(self, At::Stopped) {
            *self = At::Split(Split::start(condition, state));
        }
    }
}

/// The states where an `it` block's condition held and where it didn't.
/// They stay apart until the flags that the condition tests change, since
/// a later `it` block may test them again.
struct Split {
    /// The condition, as `it` encodes it: the low bit says whether it is
    /// one test, such as `eq`, or its opposite, such as `ne`.
    condition: u16,
    /// The state where it held: `None` where the code doesn't go on.
    held: Option<State>,
    /// The state where it didn't.
    not_held: Option<State>,
    /// Whether the `then` instructions of the last `it` block on these
    /// flags run where the condition held. They run where it didn't when
    /// that block tests the opposite.
    then_on_held: bool,
    /// Whether code may have changed the flags on either path.
    flags_changed: bool,
}

impl Split {
    /// The two paths from `state`, at an `it` on `condition`.
    fn start(condition: u16, state: State) -> Split {
        Split {
            condition,
            held: Some(state.clone()),
            not_held: Some(state),
            then_on_held: true,
            flags_changed: false,
        }
    }

    /// The path that an instruction that runs `Then` or `Else` in the last
    /// `it` block runs on.
    fn path_for(&mut self, condition: Condition) -> &mut Option<State> {
        if (condition == Condition::Then) == self.then_on_held {
            &mut self.held
        } else {
            &mut self.not_held
        }
    }

    /// The states of the paths where the code goes on.
    fn paths(&mut self) -> impl Iterator<Item = &mut State> {
        [&mut self.held, &mut self.not_held].into_iter().flatten()
    }
}

/// Whether two conditions, as `it` encodes them, test the same flags:
/// they are the same, or one is the opposite of the other.
fn tests_same_flags(a: u16, b: u16) -> bool {
    a >> 1 == b >> 1
}

impl<'a> Function<'a> {
    /// Ready to follow the registers through `code`, with no block reached
    /// yet.
    fn new(code: &'a Code<'a>) -> Function<'a> {
        Function {
            code,
            states: vec![None; code.blocks.len()],
            to_run: BTreeSet::new(),
            uses: AddressUses::new(code.len()),
            found: None,
        }
    }

    /// Follows the registers through the code until they hold nothing new
    /// anywhere, then runs each block a last time to collect what they
    /// hold where it matters, which it returns.
    fn run(&mut self) -> Found {
        if self.code.blocks.is_empty() {
            return Found::default();
        }
        self.reach(0, &State::entry());
        while let Some(block) = self.to_run.pop_first() {
            let state = self.states[block]
                .as_deref()
                .expect("only blocks reached run");
            self.run_block(block, state.clone());
        }
        self.found = Some(Found::default());
        for block in 0..self.code.blocks.len() {
            if let Some(state) = self.states[block].as_deref() {
                self.run_block(block, state.clone());
            }
        }
        self.uses.finish();
        self.found.take().unwrap_or_default()
    }

    /// Adds `state` to what the registers and the stack may hold at the
    /// instruction `start`, which starts a block, and runs the block again
    /// if that changed.
    fn reach(&mut self, start: usize, state: &State) {
        if self.found.is_some() {
            return;
        }
        let block = self.code.block_at(start);
        let changed = match &mut self.states[block] {
            Some(old) => old.join(state, &mut self.uses.lost()),
            empty @ None => {
                *empty = Some(Box::new(state.clone()));
                true
            }
        };
        if changed {
            self.to_run.insert(block);
        }
    }

    /// Runs one block from `state`.
    fn run_block(&mut self, block: usize, state: State) {
        let instructions = self.code.block(block);
        let end = instructions.end;
        let mut at = At::One(state);
        for index in instructions {
            let condition = self.code.conditions[index];
            if condition == Condition::Always
                && let At::Split(split) = &at
                && !self.keeps_split(index, split)
            {
                self.join_paths(&mut at);
            }
            match (condition, &mut at) {
                (_, At::Stopped) => break,
                (Condition::Always, At::One(state)) => {
                    if !self.step(index, state) {
                        at = At::Stopped;
                    } else if let Flow::It { firstcond, .. } = self.code.effects[index].flow
                        && self.code.conditions.get(index + 1) != Some(&Condition::Always)
                    {
                        at.split(firstcond);
                    }
                }
                (Condition::Always, At::Split(split)) => self.step_split(index, split),
                (condition, At::Split(split)) => self.step_guarded(index, condition, split),
                // Not reached: an `it` block splits the paths before the
                // instructions it makes conditional.
                (_, At::One(_)) => break,
            }
        }
        self.leave_block(end, at);
    }

    /// Whether the paths of `split` stay apart at the instruction at
    /// `index`, outside an `it` block: until the flags change, a later `it`
    /// block on the same flags runs where the first one's condition held or
    /// where it didn't, and so does an instruction that goes on to the next
    /// one and leaves the flags.
    fn keeps_split(&self, index: usize, split: &Split) -> bool {
        !split.flags_changed
            && match self.code.effects[index].flow {
                Flow::It { firstcond, .. } => tests_same_flags(firstcond, split.condition),
                Flow::Next => !self.code.changes_flags(index),
                _ => false,
            }
    }

    /// Runs the instruction at `index`, outside an `it` block, on both
    /// paths of `split`, which stay apart; see [`Function::keeps_split`].
    fn step_split(&mut self, index: usize, split: &mut Split) {
        if let Flow::It { firstcond, .. } = self.code.effects[index].flow {
            split.then_on_held = firstcond == split.condition;
        }
        for state in split.paths() {
            // An `it`, or an instruction that doesn't branch: it goes on.
            self.step(index, state);
        }
    }

    /// Runs the instruction at `index`, which an `it` block makes
    /// conditional on `condition`, on the path of `split` where it runs, or
    /// on both when it may run on either, which then also keep what they
    /// held before it.
    fn step_guarded(&mut self, index: usize, condition: Condition, split: &mut Split) {
        let effect = self.code.effects[index];
        if condition == Condition::Maybe {
            for state in split.paths() {
                let mut after = state.clone();
                if self.step(index, &mut after) {
                    state.join(&after, &mut self.uses.lost());
                }
            }
        } else {
            let path = split.path_for(condition);
            let goes_on = match path {
                Some(state) => self.step(index, state),
                None => true,
            };
            if !goes_on {
                *path = None;
            }
        }
        // A call may change the flags too.
        split.flags_changed |=
            effect.sets_flags(true) || matches!(effect.flow, Flow::Call | Flow::CallRegister(..));
    }

    /// Joins the two paths of an `it` block, where the code is on them;
    /// either may not go on.
    fn join_paths(&mut self, at: &mut At) {
        let At::Split(split) = at else {
            return;
        };
        *at = match (split.held.take(), split.not_held.take()) {
            (Some(mut held), Some(not_held)) => {
                held.join(&not_held, &mut self.uses.lost());
                At::One(held)
            }
            (Some(state), None) | (None, Some(state)) => At::One(state),
            (None, None) => At::Stopped,
        };
    }

    /// Passes what the registers and the stack hold where the code is at
    /// the end of a block, before the instruction `end`, on to the block
    /// that starts there, when the code runs on into it. In the last pass,
    /// where the code runs on past what elfq was given, it records what
    /// the registers and the stack hold as passed on, unless the block ends
    /// with a call.
    fn leave_block(&mut self, end: usize, mut at: At) {
        self.join_paths(&mut at);
        let At::One(state) = at else {
            return;
        };
        let last = end - 1;
        if end < self.code.len() && self.code.runs_on(last) {
            self.reach(end, &state);
        } else if self.found.is_some()
            && !matches!(
                self.code.effects[last].flow,
                Flow::Call | Flow::CallRegister(..)
            )
        {
            // The code runs on past what elfq was given, into code it
            // can't see, unless it ends with a call that never returns.
            self.uses.pass_registers(&state, !0);
            for value in state
                .stack_words()
                .flat_map(|(_, contents)| contents.values())
            {
                self.uses.record(value, Use::Other);
            }
        }
    }

    /// Runs the instruction at `index` on `state`, and passes the state on
    /// to where it goes other than on to the next instruction. Returns
    /// whether it can go on to the next one.
    fn step(&mut self, index: usize, state: &mut State) -> bool {
        let code = self.code;
        let effect = code.effects[index];
        // Before it runs, since it may store through the address it lets
        // out, as `str r1, [r0, r3]` does with sp in r3, or load from it.
        state.escaped |= self.escapes(index, state);
        if self.found.is_some() {
            self.report(index, state);
        }
        let calls = matches!(effect.flow, Flow::Call | Flow::CallRegister(..));
        // What it reads in r0 after a call is the result, unless it
        // branches through r0; see `AddressUses::record_kept_result`.
        let branches_through = match effect.flow {
            Flow::CallRegister(register, _)
            | Flow::JumpRegister(register, _)
            | Flow::JumpRelative(register) => bit(register),
            _ => 0,
        };
        self.uses
            .reads(state, effect.reads & !bit(RESULT) | branches_through);
        state.escaped |= self.execute(index, state);
        if calls {
            state.after_call();
            // The function may change the stack through an address that
            // got out of sight.
            if state.escaped {
                state.forget_stack(&mut self.uses.lost());
            }
        }
        match effect.flow {
            Flow::Next | Flow::It { .. } | Flow::Call | Flow::CallRegister(..) => true,
            Flow::Branch { when, .. } => {
                // A test of whether a register is 0, as in `if (f) f();`
                // for a weak function that the program may not have.
                let test = code.zero_test(index, when);
                for &target in code.jumps(index) {
                    match test {
                        Some((register, zero)) => {
                            let mut taken = state.clone();
                            taken.narrow(register, zero);
                            self.reach(target, &taken);
                        }
                        None => self.reach(target, state),
                    }
                }
                if when == When::Always {
                    return false;
                }
                if let Some((register, zero)) = test {
                    state.narrow(register, !zero);
                }
                true
            }
            Flow::Table => {
                for &target in code.jumps(index) {
                    self.reach(target, state);
                }
                false
            }
            Flow::JumpRegister(..)
            | Flow::JumpRelative(_)
            | Flow::Return
            | Flow::Pop
            | Flow::JumpMemory
            | Flow::Stop => false,
        }
    }

    /// Changes `state` as the instruction at `index` does: writes what it
    /// puts in registers and on the stack, and something elfq can't tell
    /// in the other registers it writes. Returns whether it turns an
    /// address on the stack into something elfq can't tell; see
    /// [`State::escaped`].
    fn execute(&mut self, index: usize, state: &mut State) -> bool {
        let effect = self.code.effects[index];
        let pc = self.code.pc(index);
        let source = Some(index as u32);
        let address = |number| Value {
            number,
            kind: Kind::Number,
            source,
            stale: false,
        };
        let builds_address = self.found.is_some() && self.builds_address(index, state);
        let sp = state.sp();
        let mut leaks = false;
        // What it puts in a register, worked out before it writes any.
        let result = match effect.operation {
            // No number, or one that isn't an address built in a way this
            // module follows (see its header).
            Operation::Nothing
            | Operation::Compare { .. }
            | Operation::CompareRegisters { .. }
            | Operation::Subtract { .. }
            | Operation::AddShifted { .. }
            | Operation::Negate { .. }
            | Operation::AndConstant { .. }
            | Operation::And { .. }
            | Operation::ShiftRight { .. }
            | Operation::Push { .. }
            | Operation::Pop { .. } => None,
            Operation::Constant { register, value } => {
                Some((register, Contents::holding(Value::number(value))))
            }
            Operation::Literal {
                register,
                address: at,
            } => Some((
                register,
                match self.code.word(at) {
                    Some(word) => Contents::holding(address(word)),
                    None => Contents::UNKNOWN,
                },
            )),
            Operation::Address { register, value } => {
                Some((register, Contents::holding(address(value))))
            }
            Operation::HighHalf { register, high } => {
                let low = state.read(register, pc);
                leaks = low.may_be_stack();
                let contents = low.map(|low| {
                    (low.kind == Kind::Number).then(|| address(low.number & 0xffff | high << 16))
                });
                Some((register, contents))
            }
            Operation::Copy { register, source } => Some((register, state.read(source, pc))),
            Operation::AddConstant {
                register,
                source,
                amount,
            } => {
                let contents = state.read(source, pc).map(|value| {
                    Some(Value {
                        number: value.number.wrapping_add(amount),
                        ..value
                    })
                });
                Some((register, contents))
            }
            Operation::ShiftLeft {
                register,
                source,
                shift,
            } => {
                let source = state.read(source, pc);
                leaks = shift != 0 && source.may_be_stack();
                let contents = source.map(|value| match (shift, value.kind) {
                    (0, _) => Some(value),
                    (_, Kind::Number) => Some(Value {
                        number: value.number << shift,
                        ..value
                    }),
                    _ => None,
                });
                Some((register, contents))
            }
            Operation::Add { register, a, b } => {
                let (a, b) = (state.read(a, pc), state.read(b, pc));
                let sum = match Contents::sum(&a, &b, source) {
                    Some(sum) => sum,
                    // More than elfq follows.
                    None => {
                        for value in a.values().iter().chain(b.values()) {
                            self.uses.record(value, Use::Other);
                        }
                        Contents::UNKNOWN
                    }
                };
                // An address on the stack plus something elfq can't tell.
                leaks = sum.may_be_unknown() && (a.may_be_stack() || b.may_be_stack());
                Some((register, sum))
            }
            Operation::Load { into, base, .. } => {
                let at = plain_offset(&effect).and_then(|offset| {
                    let base = state.read(base, pc).stack_offset()?;
                    Some(base.wrapping_add(offset))
                });
                if let Some(at) = at {
                    self.uses.loads_back(state, at.div_euclid(4) * 4);
                }
                match (into, at) {
                    (Transfer::Word(register), Some(at)) if at % 4 == 0 => {
                        Some((register, state.word(at)))
                    }
                    _ => None,
                }
            }
            Operation::Store {
                from, base, size, ..
            } => {
                let value = match from {
                    Transfer::Word(register) => Some(state.read(register, pc)),
                    _ => None,
                };
                let offset = plain_offset(&effect);
                self.store(state, value, state.read(base, pc), offset, size);
                None
            }
        };
        for register in each(effect.writes) {
            state.set(register, Contents::UNKNOWN);
        }
        match (effect.operation, sp) {
            (Operation::Push { registers, size }, Some(sp)) => {
                let bottom = sp.wrapping_sub(size as i32);
                for word in (bottom..sp).step_by(4) {
                    state.set_word(word, Contents::UNKNOWN);
                }
                for (n, register) in each(registers).enumerate() {
                    state.set_word(bottom + 4 * n as i32, state.read(register, pc));
                }
                state.set(SP, Contents::holding(Value::stack(bottom)));
            }
            // It may push anywhere on the stack.
            (Operation::Push { .. }, None) => state.forget_stack(&mut self.uses.lost()),
            (Operation::Pop { registers, size }, Some(sp)) => {
                for (n, register) in each(registers).enumerate() {
                    let at = sp + 4 * n as i32;
                    self.uses.loads_back(state, at);
                    if register != PC {
                        state.set(register, state.word(at));
                    }
                }
                let top = sp.wrapping_add(size as i32);
                state.free_below(top);
                state.set(SP, Contents::holding(Value::stack(top)));
            }
            _ => {}
        }
        if let Some((register, contents)) = result
            && register != PC
        {
            if builds_address && let Some(found) = &mut self.found {
                let built = contents
                    .values()
                    .iter()
                    .filter(|value| value.source == source);
                let addresses = found.addresses.entry(index).or_default();
                addresses.extend(built.map(|value| value.number));
            }
            state.set(register, contents);
        }
        leaks
    }

    /// Changes the words on the stack as storing `size` bytes at `base` +
    /// `offset` does, with `value` what a register held when it stores a
    /// whole register.
    fn store(
        &mut self,
        state: &mut State,
        value: Option<Contents>,
        base: Contents,
        offset: Option<i32>,
        size: u32,
    ) {
        let start = match (base.stack_offset(), offset) {
            (Some(at), Some(offset)) => at.wrapping_add(offset),
            // It may store anywhere on the stack: through an address there
            // that elfq can't tell, or through one that got out of sight.
            _ => {
                if base.may_be_stack() || state.escaped && base.may_be_unknown() {
                    state.forget_stack(&mut self.uses.lost());
                }
                return;
            }
        };
        match value {
            Some(value) if size == 4 && start % 4 == 0 => state.set_word(start, value),
            // Part of a word, or more than one.
            _ => {
                let end = start.wrapping_add(size.max(1) as i32);
                for word in (start.div_euclid(4) * 4..end).step_by(4) {
                    state.set_word(word, Contents::UNKNOWN);
                }
            }
        }
    }

    /// Whether the instruction at `index` may let an address on the stack
    /// out of sight, besides by turning it into something elfq can't tell:
    /// it does something elfq doesn't follow with a register that may hold
    /// one, as `bic r0, sp, #7` does, stores one where elfq can't tell, or
    /// passes one to a function, in a register or on the stack.
    fn escapes(&self, index: usize, state: &State) -> bool {
        let effect = self.code.effects[index];
        let writes_base = |base| effect.writes & bit(base) != 0;
        // The registers it reads in ways elfq follows.
        let mut followed = match effect.operation {
            Operation::Copy { source, .. }
            | Operation::AddConstant { source, .. }
            | Operation::ShiftLeft { source, .. } => bit(source),
            // `execute` follows the sum, though `report` counts what code
            // adds to as used as an address, unless it adds pc.
            Operation::Add { a, b, .. } => bit(a) | bit(b),
            // Comparing lets nothing out, though `report` counts what code
            // compares as used as an address.
            Operation::HighHalf { register, .. } | Operation::Compare { register, .. } => {
                bit(register)
            }
            // Its base, as an address to load from, unless it changes the
            // base too, other than by loading a word into it.
            Operation::Load { into, base, .. }
                if !writes_base(base) || into == Transfer::Word(base) =>
            {
                bit(base)
            }
            Operation::Store {
                from, base, size, ..
            } if !writes_base(base) => {
                let offset = plain_offset(&effect);
                let slot = offset.and_then(|offset| state.held(base).word_at(offset));
                match from {
                    Transfer::Word(register) if size == 4 && slot.is_some() => {
                        bit(base) | bit(register)
                    }
                    _ => bit(base),
                }
            }
            Operation::Push { registers, .. } if state.sp().is_some() => registers | bit(SP),
            // It reads sp only to pop from it, though `report` counts the
            // words it may pop as used where elfq can't tell sp.
            Operation::Pop { .. } => bit(SP),
            // A load or store that changes its base, and a push where elfq
            // can't tell sp, and what elfq doesn't follow.
            Operation::Load { .. }
            | Operation::Store { .. }
            | Operation::Push { .. }
            | Operation::Nothing
            | Operation::Constant { .. }
            | Operation::Literal { .. }
            | Operation::Address { .. }
            | Operation::Subtract { .. }
            | Operation::AddShifted { .. }
            | Operation::Negate { .. }
            | Operation::AndConstant { .. }
            | Operation::And { .. }
            | Operation::ShiftRight { .. }
            | Operation::CompareRegisters { .. } => 0,
        };
        followed |= match effect.flow {
            Flow::CallRegister(register, _)
            | Flow::JumpRegister(register, _)
            | Flow::JumpRelative(register)
            | Flow::Branch {
                when: When::Zero(register) | When::NotZero(register),
                ..
            } => bit(register),
            _ => 0,
        };
        let calls = matches!(effect.flow, Flow::Call | Flow::CallRegister(..));
        let mut passed = effect.reads & !followed & !bit(PC);
        if calls {
            passed |= ARGUMENTS;
        }
        each(passed).any(|register| state.held(register).may_be_stack())
            || calls
                && state
                    .stack_words()
                    .any(|(_, contents)| contents.may_be_stack())
    }

    /// Whether the instruction at `index` loads or builds an address: it
    /// loads a constant from a literal pool, it is `adr` or `movt`, or it
    /// adds the program counter.
    fn builds_address(&self, index: usize, state: &State) -> bool {
        match self.code.effects[index].operation {
            Operation::Literal { .. } | Operation::Address { .. } | Operation::HighHalf { .. } => {
                true
            }
            Operation::Add { a, b, .. } => state.adds_pc(a, b),
            Operation::Nothing
            | Operation::Constant { .. }
            | Operation::Copy { .. }
            | Operation::AddConstant { .. }
            | Operation::Subtract { .. }
            | Operation::AddShifted { .. }
            | Operation::Negate { .. }
            | Operation::AndConstant { .. }
            | Operation::And { .. }
            | Operation::ShiftLeft { .. }
            | Operation::ShiftRight { .. }
            | Operation::Load { .. }
            | Operation::Store { .. }
            | Operation::Push { .. }
            | Operation::Pop { .. }
            | Operation::Compare { .. }
            | Operation::CompareRegisters { .. } => false,
        }
    }

    /// Records how the instruction at `index` uses the values it reads, and
    /// where it branches when it branches through a register.
    fn report(&mut self, index: usize, state: &State) {
        let effect = self.code.effects[index];
        let pc = self.code.pc(index);
        // The registers it reads as plain numbers.
        let mut other = effect.reads;
        // Those it passes on without naming them; see
        // `AddressUses::pass_registers`.
        let mut passed = 0;
        match effect.operation {
            // Copying a value, or adding a constant to it, passes it on.
            Operation::Copy {
                register, source, ..
            }
            | Operation::AddConstant {
                register, source, ..
            }
            | Operation::ShiftLeft {
                register, source, ..
            } if register != PC => other &= !bit(source),
            Operation::HighHalf { register, .. } => {
                for value in state.read(register, pc).values() {
                    self.uses.record(value, Use::Part);
                }
                other &= !bit(register);
            }
            // Adding pc builds another address from what the other register
            // holds.
            Operation::Add { register, a, b } if register != PC && state.adds_pc(a, b) => {
                let offsets = (bit(a) | bit(b)) & !bit(PC);
                self.uses.record_registers(state, offsets, pc, Use::Part);
                other &= !offsets;
            }
            // Storing a value on the stack passes it on, to where the code
            // loads it back.
            Operation::Store {
                from: Transfer::Word(register),
                base,
                size: 4,
                ..
            } if (plain_offset(&effect))
                .is_some_and(|offset| state.held(base).word_at(offset).is_some()) =>
            {
                other &= !bit(register);
            }
            Operation::Push { registers, .. } if state.sp().is_some() => other &= !registers,
            // A load from the stack that elfq doesn't follow takes what it
            // loads out of sight: part of a word, or from a word it can't
            // tell.
            Operation::Load { into, base, .. } => {
                let base = state.read(base, pc);
                // Through a register it reads that may hold an address on
                // the stack, as its base or what it adds to the base.
                let may_be_stack =
                    each(effect.reads).any(|register| state.held(register).may_be_stack());
                match (base.stack_offset(), plain_offset(&effect)) {
                    (Some(at), Some(offset)) => {
                        let at = at.wrapping_add(offset);
                        if !matches!(into, Transfer::Word(_)) || at % 4 != 0 {
                            self.uses.record_word(state, at.div_euclid(4) * 4);
                        }
                    }
                    _ if may_be_stack || state.escaped && base.may_be_unknown() => {
                        self.uses.record_stack(state)
                    }
                    _ => {}
                }
            }
            // Where elfq can tell sp, `execute` follows what a pop loads.
            Operation::Pop { .. } if state.sp().is_none() => self.uses.record_stack(state),
            // What these read stays in `other`, which counts as using it as
            // an address: what a compare compares, as `cmp r0, #0` on the
            // address of a weak function does, and what a sum that doesn't
            // add pc adds, though `escapes` lets `execute` follow both.
            Operation::Compare { .. }
            | Operation::Nothing
            | Operation::Constant { .. }
            | Operation::Literal { .. }
            | Operation::Address { .. }
            | Operation::Copy { .. }
            | Operation::AddConstant { .. }
            | Operation::ShiftLeft { .. }
            | Operation::Add { .. }
            | Operation::Subtract { .. }
            | Operation::AddShifted { .. }
            | Operation::Negate { .. }
            | Operation::AndConstant { .. }
            | Operation::And { .. }
            | Operation::ShiftRight { .. }
            | Operation::Store { .. }
            | Operation::Push { .. }
            | Operation::Pop { .. }
            | Operation::CompareRegisters { .. } => {}
        }
        match effect.flow {
            Flow::CallRegister(register, low_bit) | Flow::JumpRegister(register, low_bit) => {
                let contents = state.read(register, pc);
                self.branch(index, &contents, |number| low_bit.target(number));
                self.uses.record_kept_result(state, register);
                // A call or a tail call passes the arguments on.
                other &= !bit(register);
                passed |= ARGUMENTS & !bit(register);
            }
            Flow::JumpRelative(register) => {
                let contents = state.read(register, pc);
                // It goes to pc + the offset, whatever its low bit.
                self.branch(index, &contents, |offset| {
                    Some(pc.wrapping_add(offset) & !1)
                });
                self.uses.record_kept_result(state, register);
                other &= !bit(register);
                passed |= ARGUMENTS & !bit(register);
            }
            Flow::Pop => {
                if let (Operation::Pop { registers, .. }, Some(sp)) = (effect.operation, state.sp())
                {
                    for (n, register) in each(registers).enumerate() {
                        let contents = state.word(sp.wrapping_add(4 * n as i32));
                        if register == PC {
                            // What elfq can't tell is the address to
                            // return to.
                            if let Contents::Known { values, .. } = contents
                                && !values.is_empty()
                            {
                                let returns_to = Contents::Known {
                                    values,
                                    or_unknown: false,
                                };
                                self.branch(index, &returns_to, |number| {
                                    LowBit::Thumb.target(number)
                                });
                            }
                        } else if bit(register) & ARGUMENTS != 0 {
                            // It passes what it pops into r0 to r3 on.
                            for value in contents.values() {
                                self.uses.record(value, Use::Other);
                            }
                        }
                    }
                }
                passed |= ARGUMENTS & !effect.writes;
            }
            Flow::Call => passed |= ARGUMENTS,
            // A return passes the result back, and a jump to another
            // function passes the arguments on.
            Flow::Return | Flow::JumpMemory => passed |= ARGUMENTS,
            Flow::Branch { target, .. } if !self.code.contains(target) => passed |= ARGUMENTS,
            _ => {}
        }
        // A call, or a jump to another function, may also pass arguments on
        // the stack.
        let leaves = match effect.flow {
            Flow::Call
            | Flow::CallRegister(..)
            | Flow::JumpRegister(..)
            | Flow::JumpRelative(_)
            | Flow::JumpMemory => true,
            Flow::Branch { target, .. } => !self.code.contains(target),
            _ => false,
        };
        if leaves {
            self.uses.pass_stack(state);
        }
        self.uses.record_registers(state, other, pc, Use::Other);
        self.uses.pass_registers(state, passed & !other);
    }

    /// Records that the branch at `index` may go to `target` of each number
    /// `contents` holds, for those it can go to, and how it uses them.
    fn branch(&mut self, index: usize, contents: &Contents, target: impl Fn(u32) -> Option<u32>) {
        let branch = self
            .found
            .as_mut()
            .expect("only the last pass records branches")
            .branches
            .entry(index)
            .or_default();
        for value in contents.values() {
            let to = match value.kind {
                Kind::Number => target(value.number),
                _ => None,
            };
            if let Some(to) = to {
                branch.targets.insert(to);
            }
            let how = if to.is_some() {
                Use::Branch
            } else {
                Use::Other
            };
            self.uses.record(value, how);
        }
        branch.unknown |= contents.may_be_unknown()
            || contents
                .values()
                .iter()
                .any(|value| value.kind != Kind::Number);
    }

    /// The references, from the sweep's and what following the registers
    /// found. Where it reached an instruction that branches through a
    /// register or loads or builds an address, its findings replace the
    /// sweep's.
    fn references(&self, found: &Found, mut references: Vec<CodeReference>) -> Vec<CodeReference> {
        let mut by_site: BTreeMap<u64, Vec<CodeReferenceKind>> = BTreeMap::new();
        for (&index, branch) in &found.branches {
            let flow = self.code.effects[index].flow;
            let calls = matches!(flow, Flow::CallRegister(..));
            let mut kinds: Vec<CodeReferenceKind> = branch
                .targets
                .iter()
                .map(|&target| match calls {
                    true => Call(u64::from(target)),
                    false => TailJump(u64::from(target)),
                })
                .collect();
            // The sweep reports nothing for `add pc, register`, as for the
            // jump tables that hand-written code builds with it, nor for a
            // `pop` into pc, which returns.
            let reported = !matches!(flow, Flow::JumpRelative(_) | Flow::Pop);
            if branch.unknown && reported {
                kinds.push(if calls { IndirectCall } else { IndirectJump });
            }
            by_site.insert(self.code.site(index), kinds);
        }
        for (&index, addresses) in &found.addresses {
            let kinds = match self.uses.as_address(index) {
                true => addresses
                    .iter()
                    .map(|&address| Address(u64::from(address)))
                    .collect(),
                false => Vec::new(),
            };
            by_site.insert(self.code.site(index), kinds);
        }
        references.retain(|reference| !by_site.contains_key(&reference.site));
        references.extend(by_site.into_iter().flat_map(|(site, kinds)| {
            kinds
                .into_iter()
                .map(move |kind| CodeReference { site, kind })
        }));
        // A stable sort keeps the order of the references at one site.
        references.sort_by_key(|reference| reference.site);
        references
    }
}

/// The offset from its base register at which the load or store that
/// `effect` says reads or writes, when it is all there is to the address:
/// `[base, #imm]`. `values` follows the words on the stack there: what a
/// load reads, and what a store writes, the two words of an `strd`
/// included (see [`Function::store`]). `ldm`, `stm` and `ldrd` have none,
/// though they may start at an offset, and nor does an `strd` that writes
/// back, even to a base in its pair, which the manual leaves
/// unpredictable and decode gives an offset: `values` takes them all to
/// read or write anywhere on the stack.
fn plain_offset(effect: &Effect) -> Option<i32> {
    match effect.operation {
        Operation::Load {
            into: Transfer::Words(_) | Transfer::Pair(..),
            ..
        }
        | Operation::Store {
            from: Transfer::Words(_),
            ..
        } => None,
        Operation::Store {
            from: Transfer::Pair(..),
            base,
            ..
        } if effect.writes & bit(base) != 0 => None,
        Operation::Load {
            addressing: Addressing::Offset(offset),
            ..
        }
        | Operation::Store {
            addressing: Addressing::Offset(offset),
            ..
        } => Some(offset),
        _ => None,
    }
}
