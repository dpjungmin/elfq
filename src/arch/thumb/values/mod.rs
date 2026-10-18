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
//! sweep in `thumb/mod.rs` reads each instruction on its own, so it sees an
//! address loaded and a call through a pointer. This module follows the
//! numbers that a function puts in its registers and on its stack, through
//! its branches and loops, and reports such a branch as a call or a jump to
//! each address the register can hold there. It keeps apart the paths
//! where an `it` block's condition holds and where it doesn't, until the
//! flags it tests change, since a later `it` block may test them again.
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
//! caller passes the function an address on its stack. A call may also
//! keep r0 to r3 and ip, and code built with GCC's `-fipa-ra` counts on
//! that when it knows the function, so a branch through one of them after
//! a call may go to the address it held, or anywhere. When such code
//! passes the address on to another function in the register, without
//! naming it again, elfq misses that use: what other code leaves in those
//! registers after a call is nothing a function could take.
//!
//! Most functions only call with `bl` and return, so it follows only those
//! where it may find more than the sweep (see `may_find_more`).

mod state;

use std::collections::{BTreeMap, BTreeSet};

use object::{Endian, Endianness};

use self::state::{Contents, Kind, State, Value, Values};
use super::decode::{self, Effect, Flow, LowBit, Operation, Registers, When};
use super::{Instruction, PC, SP, is_padding};
use crate::arch::CodeReference;
use crate::arch::CodeReferenceKind::{self, *};

/// The registers that pass arguments to a function and return its result:
/// r0 to r3.
const ARGUMENTS: Registers = 0x000f;

/// The registers that a call may change: r0 to r3, ip (r12) and lr.
const CALL_CLOBBERS: Registers = 0x500f;

/// The references in a function's code, given the instructions the sweep
/// decoded and the references it found: those of the sweep, with each
/// branch through a register, and each address loaded or built, replaced
/// by what following the registers finds. When the function has an
/// instruction that `decode` doesn't know, or code that elfq doesn't
/// understand, such as a branch into the middle of an instruction or code
/// that only a jump it doesn't follow leads to, the sweep's references are
/// returned as they are.
pub(super) fn follow(
    code: &[u8],
    address: u64,
    endianness: Endianness,
    instructions: &[Instruction],
    references: Vec<CodeReference>,
) -> Vec<CodeReference> {
    let Some(mut function) = Function::new(code, address, endianness, instructions, &references)
    else {
        return references;
    };
    function.run();
    function.references(references)
}

/// How the rest of a function uses an address that one instruction loaded
/// or built, and so whether elfq reports it as an address the code uses.
#[derive(Debug, Clone, Copy, Default)]
struct Uses {
    /// The code branches to it.
    branch: bool,
    /// The code turns it into another address: it is an offset that the
    /// code adds the program counter to, or a low half that `movt`
    /// completes.
    part: bool,
    /// The code uses it some other way, as by passing it to a function,
    /// storing it in memory or reading memory there.
    other: bool,
}

impl Uses {
    /// Whether the address is one the code uses as such: one it doesn't
    /// only branch to or build another address from. An address nothing
    /// uses counts, since elfq can't tell what code it wasn't shown does.
    fn as_address(&self) -> bool {
        self.other || !(self.branch || self.part)
    }
}

/// A way code uses an address; see [`Uses`].
#[derive(Debug, Clone, Copy)]
enum Use {
    Branch,
    Part,
    Other,
}

/// Which instructions of an `it` block run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Condition {
    /// Outside any `it` block, or in one whose condition always holds.
    Always,
    /// When the `it` block's condition holds.
    Then,
    /// When it doesn't.
    Else,
    /// Either: an instruction in the block changed the flags that the
    /// condition tests.
    Maybe,
}

/// The states where an `it` block's condition held and where it didn't.
struct Split {
    /// The condition, as `it` encodes it: the low bit says whether it is
    /// one test, such as `eq`, or its opposite, such as `ne`.
    condition: u16,
    /// Where it held, then where it didn't: `None` where the code doesn't
    /// go on.
    paths: [Option<State>; 2],
    /// Which of `paths` the `then` instructions of the last `it` block run
    /// on.
    then: usize,
    /// Whether code may have changed the flags on either path.
    flags_changed: bool,
}

/// What a branch through a register at one instruction may go to.
#[derive(Debug, Default)]
struct Branch {
    targets: BTreeSet<u32>,
    /// Whether it may also go somewhere elfq can't tell.
    unknown: bool,
}

/// One function's code, and what following its registers found.
struct Function<'a> {
    code: &'a [u8],
    /// The address of `code[0]`.
    address: u64,
    endianness: Endianness,
    instructions: &'a [Instruction],
    effects: Vec<Effect>,
    conditions: Vec<Condition>,
    /// The instructions that start a block of code that runs from start to
    /// end: the first one, those that code branches to, and those after a
    /// branch. Indices into `instructions`, sorted.
    blocks: Vec<usize>,
    /// For each instruction that branches inside the function, the
    /// instructions it may go to.
    jumps: BTreeMap<usize, Vec<usize>>,
    /// What the registers and the stack hold at the start of each block,
    /// where the code reaches it.
    states: Vec<Option<Box<State>>>,
    /// The blocks to run again, since what they start with changed.
    to_run: BTreeSet<usize>,
    /// For each instruction that loads or builds an address, how the code
    /// uses it.
    uses: Vec<Uses>,
    /// The words on the stack that the code loads into a register, by
    /// their offset as in [`State::stack`].
    loaded: BTreeSet<i32>,
    /// The instructions that loaded or built an address that a register
    /// held, stale, where elfq stopped following it. They count as used
    /// as addresses if the code reads such a register.
    stale_lost: BTreeSet<u32>,
    /// Whether the code reads a register that holds stale numbers elfq
    /// stopped following.
    reads_stale_lost: bool,
    /// Whether the code is being run a last time, from the final states, to
    /// collect what it finds.
    reporting: bool,
    /// What each branch through a register may go to, by instruction.
    branches: BTreeMap<usize, Branch>,
    /// The addresses each instruction that loads or builds one may give,
    /// by instruction.
    addresses: BTreeMap<usize, BTreeSet<u32>>,
}

impl<'a> Function<'a> {
    /// Decodes the instructions and finds the blocks of code, or returns
    /// `None` when elfq can't follow this code or following it can't find
    /// more than the sweep did.
    fn new(
        code: &'a [u8],
        address: u64,
        endianness: Endianness,
        instructions: &'a [Instruction],
        references: &[CodeReference],
    ) -> Option<Function<'a>> {
        let effects = decode_reached(address, instructions, references)?;
        if !may_find_more(&effects) || enters_reached(address, instructions, &effects) {
            return None;
        }
        // The code may run on into code that it never reaches, as the code
        // between two functions that elfq finds in stripped firmware does.
        let instructions = &instructions[..effects.len()];
        let effects = effects
            .into_iter()
            .map(|effect| effect.unwrap_or(UNREACHED))
            .collect();
        let mut function = Function {
            code,
            address,
            endianness,
            instructions,
            effects,
            conditions: vec![Condition::Always; instructions.len()],
            blocks: Vec::new(),
            jumps: BTreeMap::new(),
            states: Vec::new(),
            to_run: BTreeSet::new(),
            uses: vec![Uses::default(); instructions.len()],
            loaded: BTreeSet::new(),
            stale_lost: BTreeSet::new(),
            reads_stale_lost: false,
            reporting: false,
            branches: BTreeMap::new(),
            addresses: BTreeMap::new(),
        };
        function.find_conditions()?;
        function.find_jumps(references)?;
        function.find_blocks();
        Some(function)
    }

    /// Works out which instructions each `it` block makes conditional, and
    /// on what. Returns `None` for code that breaks the rules for `it`
    /// blocks, as data decoded as code may.
    fn find_conditions(&mut self) -> Option<()> {
        for index in 0..self.instructions.len() {
            let Flow::It { firstcond, mask } = self.effects[index].flow else {
                continue;
            };
            // An `it` inside another `it` block.
            if self.conditions[index] != Condition::Always {
                return None;
            }
            // The condition `al` always holds.
            if firstcond == 0b1110 {
                continue;
            }
            let length = 4 - mask.trailing_zeros() as usize;
            let mut flags_changed = false;
            for k in 0..length {
                let inside = index + 1 + k;
                if inside >= self.instructions.len() || !self.runs_on(inside - 1) {
                    return None;
                }
                // The condition of the k-th instruction is firstcond with
                // its low bit replaced by bit 4 - k of the mask.
                let then = k == 0 || (mask >> (4 - k)) & 1 == firstcond & 1;
                self.conditions[inside] = match () {
                    _ if flags_changed => Condition::Maybe,
                    _ if then => Condition::Then,
                    _ => Condition::Else,
                };
                let effect = self.effects[inside];
                flags_changed |= effect.sets_flags;
                // Only the last instruction of a block may branch.
                let goes_on = matches!(
                    effect.flow,
                    Flow::Next | Flow::Call | Flow::CallRegister(..)
                );
                if !goes_on && k + 1 < length {
                    return None;
                }
            }
        }
        Some(())
    }

    /// Finds where each branch inside the function goes, from the branch
    /// itself or, for `tbb` and `tbh`, from the jumps the sweep found.
    /// Returns `None` when one goes somewhere other than the start of an
    /// instruction, or into an `it` block.
    fn find_jumps(&mut self, references: &[CodeReference]) -> Option<()> {
        for index in 0..self.instructions.len() {
            let targets: Vec<u64> = match self.effects[index].flow {
                Flow::Branch { target, .. } => vec![target],
                Flow::Table => {
                    let site = self.site(index);
                    references
                        .iter()
                        .filter(|reference| reference.site == site)
                        .filter_map(|reference| match reference.kind {
                            Jump(target) => Some(target),
                            _ => None,
                        })
                        .collect()
                }
                _ => continue,
            };
            let mut inside = Vec::new();
            for target in targets.into_iter().filter(|&target| self.contains(target)) {
                let offset = (target - self.address) as usize;
                let at = self
                    .instructions
                    .binary_search_by_key(&offset, |instruction| instruction.offset)
                    .ok()?;
                if self.conditions[at] != Condition::Always {
                    return None;
                }
                inside.push(at);
            }
            self.jumps.insert(index, inside);
        }
        Some(())
    }

    /// Splits the code into blocks that run from start to end.
    fn find_blocks(&mut self) {
        if self.instructions.is_empty() {
            return;
        }
        let mut starts = BTreeSet::from([0]);
        starts.extend(self.jumps.values().flatten().copied());
        for index in 0..self.instructions.len() - 1 {
            let goes_on = matches!(
                self.effects[index].flow,
                Flow::Next | Flow::Call | Flow::CallRegister(..) | Flow::It { .. }
            );
            if !goes_on || !self.runs_on(index) {
                starts.insert(index + 1);
            }
        }
        self.blocks = starts.into_iter().collect();
        self.states = vec![None; self.blocks.len()];
    }

    /// Follows the registers through the code until they hold nothing new
    /// anywhere, then runs each block a last time to collect what they
    /// hold where it matters.
    fn run(&mut self) {
        if self.blocks.is_empty() {
            return;
        }
        self.reach(0, &State::entry());
        while let Some(block) = self.to_run.pop_first() {
            let state = self.states[block]
                .as_deref()
                .expect("only blocks reached run");
            self.run_block(block, state.clone());
        }
        self.reporting = true;
        for block in 0..self.blocks.len() {
            if let Some(state) = self.states[block].as_deref() {
                self.run_block(block, state.clone());
            }
        }
        if self.reads_stale_lost {
            for &source in &self.stale_lost {
                self.uses[source as usize].other = true;
            }
        }
    }

    /// Adds `state` to what the registers and the stack may hold at the
    /// instruction `start`, which starts a block, and runs the block again
    /// if that changed.
    fn reach(&mut self, start: usize, state: &State) {
        if self.reporting {
            return;
        }
        let block = self
            .blocks
            .binary_search(&start)
            .expect("code goes only to the start of a block");
        let changed = match &mut self.states[block] {
            Some(old) => old.join(state, &mut lost(&mut self.uses, &mut self.stale_lost)),
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
        let start = self.blocks[block];
        let end = self
            .blocks
            .get(block + 1)
            .copied()
            .unwrap_or(self.instructions.len());
        // The state while the code goes on, or from an `it` block on, the
        // two where its condition held and where it didn't.
        let mut state = Some(state);
        let mut split: Option<Split> = None;
        for index in start..end {
            let effect = self.effects[index];
            let condition = self.conditions[index];
            if condition == Condition::Always {
                if let Some(paths) = &mut split {
                    // Until the flags change, a later `it` block on them
                    // runs where the first one's condition held or where
                    // it didn't, so the two stay apart.
                    let keeps = !paths.flags_changed
                        && match effect.flow {
                            Flow::It { firstcond, .. } => firstcond >> 1 == paths.condition >> 1,
                            Flow::Next => !self.changes_flags(index),
                            _ => false,
                        };
                    if keeps {
                        if let Flow::It { firstcond, .. } = effect.flow {
                            paths.then = usize::from(firstcond != paths.condition);
                        }
                        for current in paths.paths.iter_mut().flatten() {
                            self.step(index, current);
                        }
                        continue;
                    }
                    let [held, not] = split.take().expect("the paths are apart").paths;
                    state = self.join_paths(held, not);
                }
                let Some(current) = &mut state else {
                    break;
                };
                if !self.step(index, current) {
                    state = None;
                } else if let Flow::It { firstcond, .. } = effect.flow
                    && self.conditions.get(index + 1) != Some(&Condition::Always)
                {
                    let before = state.take();
                    split = Some(Split {
                        condition: firstcond,
                        paths: [before.clone(), before],
                        then: 0,
                        flags_changed: false,
                    });
                }
                continue;
            }
            let Some(paths) = &mut split else {
                break;
            };
            for (k, path) in paths.paths.iter_mut().enumerate() {
                let Some(current) = path else {
                    continue;
                };
                match condition {
                    Condition::Maybe => {
                        let mut after = current.clone();
                        if self.step(index, &mut after) {
                            current.join(&after, &mut lost(&mut self.uses, &mut self.stale_lost));
                        }
                    }
                    _ if (condition == Condition::Then) != (k == paths.then) => {}
                    _ => {
                        if !self.step(index, current) {
                            *path = None;
                        }
                    }
                }
            }
            // A call may change the flags too.
            paths.flags_changed |=
                effect.sets_flags || matches!(effect.flow, Flow::Call | Flow::CallRegister(..));
        }
        if let Some(Split {
            paths: [held, not], ..
        }) = split
        {
            state = self.join_paths(held, not);
        }
        if let Some(state) = state {
            let last = end - 1;
            if end < self.instructions.len() && self.runs_on(last) {
                self.reach(end, &state);
            } else if self.reporting
                && !matches!(self.effects[last].flow, Flow::Call | Flow::CallRegister(..))
            {
                // The code runs on past what elfq was given, into code it
                // can't see, unless it ends with a call that never returns.
                self.pass_registers(&state, !0);
                for value in state
                    .stack
                    .values()
                    .flat_map(|contents| contents.values.iter())
                {
                    use_value(&mut self.uses, value, Use::Other);
                }
            }
        }
    }

    /// Whether the instruction at `index`, outside an `it` block, may change
    /// the flags. There, the 16-bit instructions that shift, add, subtract,
    /// move, compare or do logic, which start below 0x4400, change them too.
    fn changes_flags(&self, index: usize) -> bool {
        let instruction = self.instructions[index];
        self.effects[index].sets_flags || (instruction.size() == 2 && instruction.first < 0x4400)
    }

    /// Runs the instruction at `index` on `state`, and passes the state on
    /// to where it goes other than on to the next instruction. Returns
    /// whether it can go on to the next one.
    fn step(&mut self, index: usize, state: &mut State) -> bool {
        let effect = self.effects[index];
        if self.reporting {
            self.report(index, state);
        }
        let calls = matches!(effect.flow, Flow::Call | Flow::CallRegister(..));
        self.reads_stale_lost |= registers(effect.reads).any(|register| {
            let contents = state.registers[usize::from(register)];
            contents.saturated && contents.stale
        });
        // Before it runs, since it may store through the address it lets
        // out, as `str r1, [r0, r3]` does with sp in r3.
        state.escaped |= self.escapes(index, state);
        state.escaped |= self.execute(index, state);
        if calls {
            for register in registers(CALL_CLOBBERS) {
                let contents = &mut state.registers[usize::from(register)];
                *contents = contents.after_call();
            }
            // The function may change the stack through an address that
            // got out of sight.
            if state.escaped {
                state.forget_stack(&mut lost(&mut self.uses, &mut self.stale_lost));
            }
        }
        match effect.flow {
            Flow::Next | Flow::It { .. } | Flow::Call | Flow::CallRegister(..) => true,
            Flow::Branch { when, .. } => {
                // A test of whether a register is 0, as in `if (f) f();`
                // for a weak function that the program may not have.
                let test = self.zero_test(index, when);
                for k in 0..self.jumps[&index].len() {
                    let target = self.jumps[&index][k];
                    match test {
                        Some((register, zero)) => {
                            let mut taken = state.clone();
                            taken.registers[usize::from(register)].narrow(zero);
                            self.reach(target, &taken);
                        }
                        None => self.reach(target, state),
                    }
                }
                if when == When::Always {
                    return false;
                }
                if let Some((register, zero)) = test {
                    state.registers[usize::from(register)].narrow(!zero);
                }
                true
            }
            Flow::Table => {
                for k in 0..self.jumps[&index].len() {
                    self.reach(self.jumps[&index][k], state);
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
        let effect = self.effects[index];
        let pc = (self.site(index) as u32).wrapping_add(4);
        let source = Some(index as u32);
        let address = |number| Value {
            number,
            kind: Kind::Number,
            source,
        };
        let builds_address = self.reporting && self.builds_address(index, state);
        let sp = state.sp();
        let mut leaks = false;
        // What it puts in a register, worked out before it writes any.
        let result = match effect.operation {
            Operation::Nothing
            | Operation::Compare { .. }
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
                match self.word(at) {
                    Some(word) => Contents::holding(address(word)),
                    None => Contents::unknown(),
                },
            )),
            Operation::Address { register, value } => {
                Some((register, Contents::holding(address(value))))
            }
            Operation::HighHalf { register, high } => {
                let low = state.get(register, pc);
                leaks = low.may_be_stack();
                let contents = low.map(|low| {
                    (low.kind == Kind::Number).then(|| address(low.number & 0xffff | high << 16))
                });
                Some((register, contents))
            }
            Operation::Copy { register, source } => Some((register, state.get(source, pc))),
            Operation::AddConstant {
                register,
                source,
                amount,
            } => {
                let contents = state.get(source, pc).map(|value| {
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
                let source = state.get(source, pc);
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
                let (a, b) = (state.get(a, pc), state.get(b, pc));
                let sum = self.add(&a, &b, source);
                // An address on the stack plus something elfq can't tell.
                leaks = sum.unknown && (a.may_be_stack() || b.may_be_stack());
                Some((register, sum))
            }
            Operation::Load {
                register,
                base,
                offset: Some(offset),
            } => {
                let at = state
                    .get(base, pc)
                    .stack_offset()
                    .map(|at| at.wrapping_add(offset));
                if let Some(at) = at {
                    self.loaded.insert(at.div_euclid(4) * 4);
                }
                match (register, at) {
                    (Some(register), Some(at)) if at % 4 == 0 => Some((register, state.word(at))),
                    _ => None,
                }
            }
            Operation::Load { .. } => None,
            Operation::Store {
                register,
                base,
                offset,
                size,
            } => {
                self.store(state, register, state.get(base, pc), offset, size);
                None
            }
        };
        for register in registers(effect.writes) {
            state.registers[usize::from(register)] = Contents::unknown();
        }
        match (effect.operation, sp) {
            (Operation::Push { registers, size }, Some(sp)) => {
                let bottom = sp.wrapping_sub(size as i32);
                for word in (bottom..sp).step_by(4) {
                    state.set_word(word, Contents::unknown());
                }
                for (n, register) in self::registers(registers).enumerate() {
                    state.set_word(bottom + 4 * n as i32, state.get(register, pc));
                }
                state.registers[usize::from(SP)] = Contents::holding(Value::stack(bottom));
            }
            // It may push anywhere on the stack.
            (Operation::Push { .. }, None) => {
                state.forget_stack(&mut lost(&mut self.uses, &mut self.stale_lost))
            }
            (Operation::Pop { registers, size }, Some(sp)) => {
                for (n, register) in self::registers(registers).enumerate() {
                    let at = sp + 4 * n as i32;
                    self.loaded.insert(at);
                    if register != PC {
                        state.registers[usize::from(register)] = state.word(at);
                    }
                }
                // The words below sp are free, and hold nothing the
                // function can count on.
                let top = sp.wrapping_add(size as i32);
                state.stack.retain(|&offset, _| offset >= top);
                state.registers[usize::from(SP)] = Contents::holding(Value::stack(top));
            }
            _ => {}
        }
        if let Some((register, contents)) = result
            && register != PC
        {
            if builds_address {
                let built = contents
                    .values
                    .iter()
                    .filter(|value| value.source == source);
                let addresses = self.addresses.entry(index).or_default();
                addresses.extend(built.map(|value| value.number));
            }
            state.registers[usize::from(register)] = contents;
        }
        leaks
    }

    /// Changes the words on the stack as storing `size` bytes at `base` +
    /// `offset` does, from `register` when it stores a whole register.
    fn store(
        &mut self,
        state: &mut State,
        register: Option<u16>,
        base: Contents,
        offset: Option<i32>,
        size: u32,
    ) {
        let start = match (base.stack_offset(), offset) {
            (Some(at), Some(offset)) => at.wrapping_add(offset),
            // It may store anywhere on the stack: through an address there
            // that elfq can't tell, or through one that got out of sight.
            _ => {
                if base.may_be_stack() || state.escaped && base.unknown {
                    state.forget_stack(&mut lost(&mut self.uses, &mut self.stale_lost));
                }
                return;
            }
        };
        match register {
            Some(register) if size == 4 && start % 4 == 0 => {
                state.set_word(start, state.registers[usize::from(register)]);
            }
            // Part of a word, or more than one.
            _ => {
                let end = start.wrapping_add(size.max(1) as i32);
                for word in (start.div_euclid(4) * 4..end).step_by(4) {
                    state.set_word(word, Contents::unknown());
                }
            }
        }
    }

    /// The sum of what `a` and `b` hold, as `add` computes it. Adding the
    /// program counter to a number makes an address, which the
    /// instruction `source` builds.
    fn add(&mut self, a: &Contents, b: &Contents, source: Option<u32>) -> Contents {
        let mut sum = Contents {
            values: Values::NONE,
            unknown: a.unknown || a.saturated || b.unknown || b.saturated,
            saturated: false,
            stale: false,
        };
        for x in a.values.iter() {
            for y in b.values.iter() {
                let number = x.number.wrapping_add(y.number);
                let (kind, source) = match (x.kind, y.kind) {
                    (Kind::Number, Kind::Number) => (Kind::Number, None),
                    (Kind::ProgramCounter, Kind::Number) | (Kind::Number, Kind::ProgramCounter) => {
                        (Kind::Number, source)
                    }
                    (Kind::Stack, Kind::Number) | (Kind::Number, Kind::Stack) => {
                        (Kind::Stack, None)
                    }
                    _ => {
                        sum.unknown = true;
                        continue;
                    }
                };
                let value = Value {
                    number,
                    kind,
                    source,
                };
                if sum.values.insert(value).is_none() {
                    // More than elfq follows.
                    for value in a.values.iter().chain(b.values.iter()) {
                        use_value(&mut self.uses, value, Use::Other);
                    }
                    return Contents::unknown();
                }
            }
        }
        sum
    }

    /// Whether the instruction at `index` may let an address on the stack
    /// out of sight, besides by turning it into something elfq can't tell:
    /// it does something elfq doesn't follow with a register that may hold
    /// one, as `bic r0, sp, #7` does, stores one where elfq can't tell, or
    /// passes one to a function, in a register or on the stack.
    fn escapes(&self, index: usize, state: &State) -> bool {
        let effect = self.effects[index];
        let writes_back = |base| effect.writes & bit(base) != 0;
        // The registers it reads in ways elfq follows.
        let mut followed = match effect.operation {
            Operation::Copy { source, .. }
            | Operation::AddConstant { source, .. }
            | Operation::ShiftLeft { source, .. } => bit(source),
            Operation::Add { a, b, .. } => bit(a) | bit(b),
            Operation::HighHalf { register, .. } | Operation::Compare { register, .. } => {
                bit(register)
            }
            // Its base, as an address to load from, unless it changes the
            // base too.
            Operation::Load { register, base, .. }
                if !writes_back(base) || register == Some(base) =>
            {
                bit(base)
            }
            Operation::Store {
                register,
                base,
                offset,
                size,
            } if !writes_back(base) => {
                let slot =
                    offset.and_then(|offset| word_at(&state.registers[usize::from(base)], offset));
                match register {
                    Some(register) if size == 4 && slot.is_some() => bit(base) | bit(register),
                    _ => bit(base),
                }
            }
            Operation::Push { registers, .. } if state.sp().is_some() => registers | bit(SP),
            Operation::Pop { .. } => bit(SP),
            _ => 0,
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
        registers(passed).any(|register| state.registers[usize::from(register)].may_be_stack())
            || calls && state.stack.values().any(Contents::may_be_stack)
    }

    /// Whether the instruction at `index` loads or builds an address: it
    /// loads a constant from a literal pool, it is `adr` or `movt`, or it
    /// adds the program counter.
    fn builds_address(&self, index: usize, state: &State) -> bool {
        match self.effects[index].operation {
            Operation::Literal { .. } | Operation::Address { .. } | Operation::HighHalf { .. } => {
                true
            }
            Operation::Add { a, b, .. } => adds_pc(a, b, state),
            _ => false,
        }
    }

    /// Records how the instruction at `index` uses the values it reads, and
    /// where it branches when it branches through a register.
    fn report(&mut self, index: usize, state: &State) {
        let effect = self.effects[index];
        // The registers it reads as plain numbers.
        let mut other = effect.reads;
        // Those it passes on without naming them; see `pass_registers`.
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
                self.use_registers(state, bit(register), Use::Part);
                other &= !bit(register);
            }
            Operation::Add { register, a, b } if register != PC && adds_pc(a, b, state) => {
                let offsets = (bit(a) | bit(b)) & !bit(PC);
                self.use_registers(state, offsets, Use::Part);
                other &= !offsets;
            }
            // Storing a value on the stack passes it on, to where the code
            // loads it back.
            Operation::Store {
                register: Some(register),
                base,
                offset: Some(offset),
                size: 4,
            } if word_at(&state.registers[usize::from(base)], offset).is_some() => {
                other &= !bit(register);
            }
            Operation::Push { registers, .. } if state.sp().is_some() => other &= !registers,
            // A load from the stack that elfq doesn't follow takes what it
            // loads out of sight: part of a word, or from a word it can't
            // tell.
            Operation::Load {
                register,
                base,
                offset,
            } => {
                let base = state.get(base, 0);
                match (base.stack_offset(), offset) {
                    (Some(at), Some(offset)) => {
                        let at = at.wrapping_add(offset);
                        if register.is_none() || at % 4 != 0 {
                            self.use_word(state, at.div_euclid(4) * 4);
                        }
                    }
                    _ if base.may_be_stack() || state.escaped && base.unknown => {
                        self.use_stack(state);
                    }
                    _ => {}
                }
            }
            Operation::Pop { .. } if state.sp().is_none() => self.use_stack(state),
            _ => {}
        }
        match effect.flow {
            Flow::CallRegister(register, low_bit) | Flow::JumpRegister(register, low_bit) => {
                let contents = state.get(register, 0);
                self.branch(index, &contents, |number| target(number, low_bit));
                // A call or a tail call passes the arguments on.
                other &= !bit(register);
                passed |= ARGUMENTS & !bit(register);
            }
            Flow::JumpRelative(register) => {
                let pc = (self.site(index) as u32).wrapping_add(4);
                let contents = state.get(register, 0);
                // It goes to pc + the offset, whatever its low bit.
                self.branch(index, &contents, |offset| {
                    Some(pc.wrapping_add(offset) & !1)
                });
                other &= !bit(register);
                passed |= ARGUMENTS & !bit(register);
            }
            Flow::Pop => {
                if let (Operation::Pop { registers, .. }, Some(sp)) = (effect.operation, state.sp())
                {
                    for (n, register) in self::registers(registers).enumerate() {
                        let mut contents = state.word(sp.wrapping_add(4 * n as i32));
                        if register == PC {
                            // What elfq can't tell is the address to
                            // return to.
                            contents.unknown = false;
                            contents.saturated = false;
                            if !contents.values.is_empty() {
                                self.branch(index, &contents, |number| {
                                    target(number, LowBit::Thumb)
                                });
                            }
                        } else if bit(register) & ARGUMENTS != 0 {
                            // It passes what it pops into r0 to r3 on.
                            for value in contents.values.iter() {
                                use_value(&mut self.uses, value, Use::Other);
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
            Flow::Branch { target, .. } if !self.contains(target) => passed |= ARGUMENTS,
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
            Flow::Branch { target, .. } => !self.contains(target),
            _ => false,
        };
        if leaves {
            self.pass_stack(state);
        }
        self.use_registers(state, other, Use::Other);
        self.pass_registers(state, passed & !other);
    }

    /// Records that the branch at `index` may go to `target` of each number
    /// `contents` holds, for those it can go to, and how it uses them.
    fn branch(&mut self, index: usize, contents: &Contents, target: impl Fn(u32) -> Option<u32>) {
        let branch = self.branches.entry(index).or_default();
        for value in contents.values.iter() {
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
            use_value(&mut self.uses, value, how);
        }
        branch.unknown |= contents.unknown
            || contents.saturated
            || contents
                .values
                .iter()
                .any(|value| value.kind != Kind::Number);
    }

    /// Records that code reads the numbers in the word on the stack at
    /// `offset` where elfq can't follow them.
    fn use_word(&mut self, state: &State, offset: i32) {
        for value in state.word(offset).values.iter() {
            use_value(&mut self.uses, value, Use::Other);
        }
    }

    /// Records that code may read the numbers in any word on the stack
    /// where elfq can't follow them.
    fn use_stack(&mut self, state: &State) {
        for contents in state.stack.values() {
            for value in contents.values.iter() {
                use_value(&mut self.uses, value, Use::Other);
            }
        }
    }

    /// Records that a call, or a jump to another function, may take the
    /// numbers in the words on the stack from sp up as arguments: those
    /// that the code doesn't load back itself.
    fn pass_stack(&mut self, state: &State) {
        let sp = state.sp();
        for (offset, contents) in &state.stack {
            if sp.is_some_and(|sp| *offset < sp) || self.loaded.contains(offset) {
                continue;
            }
            for value in contents.values.iter() {
                use_value(&mut self.uses, value, Use::Other);
            }
        }
    }

    /// Records that code passes the values in each register of `registers`
    /// on, without naming them: as arguments to a function, a result to
    /// the caller, or to code that elfq can't see. A stale number in one
    /// isn't passed, since code passes nothing in a register that a call
    /// may have changed.
    fn pass_registers(&mut self, state: &State, registers: Registers) {
        for register in self::registers(registers).filter(|&register| register != PC) {
            let contents = state.registers[usize::from(register)];
            if !contents.stale {
                for value in contents.values.iter() {
                    use_value(&mut self.uses, value, Use::Other);
                }
            }
        }
    }

    /// Records a use of the values in each register of `registers`.
    fn use_registers(&mut self, state: &State, registers: Registers, how: Use) {
        for register in self::registers(registers).filter(|&register| register != PC) {
            for value in state.registers[usize::from(register)].values.iter() {
                use_value(&mut self.uses, value, how);
            }
        }
    }

    /// When the branch at `index`, taken `when`, tests whether a register
    /// is 0: the register, and whether it is 0 where the branch is taken.
    /// Besides `cbz` and `cbnz`, Cortex-M0 code tests with `cmp register,
    /// #0` followed by `beq` or `bne`.
    fn zero_test(&self, index: usize, when: When) -> Option<(u16, bool)> {
        let (register, zero) = match when {
            When::Zero(register) => (register, true),
            When::NotZero(register) => (register, false),
            // eq and ne
            When::Flags(condition @ (0 | 1)) => {
                let compare = index.checked_sub(1)?;
                let Operation::Compare { register, value: 0 } = self.effects[compare].operation
                else {
                    return None;
                };
                let follows = self.runs_on(compare)
                    && self.blocks.binary_search(&index).is_err()
                    && self.conditions[compare] == Condition::Always;
                if !follows {
                    return None;
                }
                (register, condition == 0)
            }
            _ => return None,
        };
        (register != PC).then_some((register, zero))
    }

    /// Where two paths through an `it` block join; either may not go on.
    fn join_paths(&mut self, a: Option<State>, b: Option<State>) -> Option<State> {
        match (a, b) {
            (Some(mut a), Some(b)) => {
                a.join(&b, &mut lost(&mut self.uses, &mut self.stale_lost));
                Some(a)
            }
            (a, b) => a.or(b),
        }
    }

    /// The references, from the sweep's and what following the registers
    /// found. Where it reached an instruction that branches through a
    /// register or loads or builds an address, its findings replace the
    /// sweep's.
    fn references(&self, mut references: Vec<CodeReference>) -> Vec<CodeReference> {
        let mut found: BTreeMap<u64, Vec<CodeReferenceKind>> = BTreeMap::new();
        for (&index, branch) in &self.branches {
            let flow = self.effects[index].flow;
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
            found.insert(self.site(index), kinds);
        }
        for (&index, addresses) in &self.addresses {
            let kinds = match self.uses[index].as_address() {
                true => addresses
                    .iter()
                    .map(|&address| Address(u64::from(address)))
                    .collect(),
                false => Vec::new(),
            };
            found.insert(self.site(index), kinds);
        }
        references.retain(|reference| !found.contains_key(&reference.site));
        references.extend(found.into_iter().flat_map(|(site, kinds)| {
            kinds
                .into_iter()
                .map(move |kind| CodeReference { site, kind })
        }));
        // A stable sort keeps the order of the references at one site.
        references.sort_by_key(|reference| reference.site);
        references
    }

    /// Whether the instruction at `index` can run on into the next one:
    /// nothing, such as a literal pool, is between them.
    fn runs_on(&self, index: usize) -> bool {
        let instruction = self.instructions[index];
        self.instructions
            .get(index + 1)
            .is_some_and(|next| next.offset == instruction.offset + instruction.size())
    }

    /// The address of the instruction at `index`.
    fn site(&self, index: usize) -> u64 {
        self.address + self.instructions[index].offset as u64
    }

    /// Whether `address` is in this function's code.
    fn contains(&self, address: u64) -> bool {
        address
            .checked_sub(self.address)
            .is_some_and(|offset| offset < self.code.len() as u64)
    }

    /// The word at `address`, if it is in this function's code.
    fn word(&self, address: u64) -> Option<u32> {
        let offset = usize::try_from(address.checked_sub(self.address)?).ok()?;
        let bytes: [u8; 4] = self
            .code
            .get(offset..offset.checked_add(4)?)?
            .try_into()
            .ok()?;
        Some(self.endianness.read_u32(bytes))
    }
}

/// The word on the stack at `offset` bytes past the address `base` holds,
/// when `base` holds one address on the stack and that word starts there.
fn word_at(base: &Contents, offset: i32) -> Option<i32> {
    let at = base.stack_offset()?.wrapping_add(offset);
    (at % 4 == 0).then_some(at)
}

/// What the instructions the code can reach from the first do, up to the
/// last it can reach: `None` for those it can't. `None` when it reaches one
/// that `decode` doesn't know.
fn decode_reached(
    address: u64,
    instructions: &[Instruction],
    references: &[CodeReference],
) -> Option<Vec<Option<Effect>>> {
    let mut effects: Vec<Option<Effect>> = vec![None; instructions.len()];
    // The instructions that an `it` makes conditional: they may not run, so
    // the code may go on past them even when they branch.
    let mut conditional = BTreeSet::new();
    let mut to_visit = vec![0];
    while let Some(mut index) = to_visit.pop() {
        // Each instruction from `index` on, while the code goes on to the
        // next.
        while let Some(&instruction) = instructions.get(index) {
            if effects[index].is_some() {
                break;
            }
            let site = address + instruction.offset as u64;
            let effect = decode::decode(site, instruction.first, instruction.second)?;
            effects[index] = Some(effect);
            let goes_on = match effect.flow {
                Flow::Next | Flow::Call | Flow::CallRegister(..) => true,
                Flow::It { firstcond, mask } => {
                    if firstcond != 0b1110 {
                        let length = 4 - mask.trailing_zeros() as usize;
                        conditional.extend(index + 1..=index + length);
                    }
                    true
                }
                Flow::Branch { target, when } => {
                    to_visit.extend(index_of(address, instructions, target));
                    when != When::Always
                }
                Flow::Table => {
                    let jumps = references.iter().filter(|reference| reference.site == site);
                    to_visit.extend(jumps.filter_map(|reference| match reference.kind {
                        Jump(target) => index_of(address, instructions, target),
                        _ => None,
                    }));
                    false
                }
                _ => false,
            };
            let runs_on = instructions
                .get(index + 1)
                .is_some_and(|next| next.offset == instruction.offset + instruction.size());
            if !runs_on || !(goes_on || conditional.contains(&index)) {
                break;
            }
            index += 1;
        }
    }
    let reached = effects
        .iter()
        .rposition(Option::is_some)
        .map_or(0, |last| last + 1);
    effects.truncate(reached);
    Some(effects)
}

/// What elfq takes an instruction that the code can't reach to do: it
/// never runs, so nothing.
const UNREACHED: Effect = Effect {
    reads: 0,
    writes: 0,
    operation: Operation::Nothing,
    flow: Flow::Stop,
    sets_flags: false,
};

/// The index in `instructions` of the one at `target`, if there is one.
fn index_of(address: u64, instructions: &[Instruction], target: u64) -> Option<usize> {
    let offset = usize::try_from(target.checked_sub(address)?).ok()?;
    instructions
        .binary_search_by_key(&offset, |instruction| instruction.offset)
        .ok()
}

/// Whether code that the first instruction doesn't lead to may go on into
/// code it does, as the cases of a `switch` do when elfq can't read the
/// table of addresses that the code jumps through to them: `mov pc,
/// register`, `ldr pc, [...]`, or a call to `__gnu_thumb1_case_uqi` and
/// the like. What the registers hold where they join would then miss what
/// those cases put there. Padding before a loop doesn't count. `effects`
/// holds `None` for the instructions the code can't reach.
fn enters_reached(address: u64, instructions: &[Instruction], effects: &[Option<Effect>]) -> bool {
    let reached = |index: usize| effects.get(index).is_some_and(Option::is_some);
    (0..effects.len()).any(|index| {
        let instruction = instructions[index];
        if reached(index) || is_padding(instruction.first, Some(instruction.second)) {
            return false;
        }
        let site = address + instruction.offset as u64;
        // Data, as a table of cases may be.
        let Some(effect) = decode::decode(site, instruction.first, instruction.second) else {
            return false;
        };
        let goes_on = match effect.flow {
            Flow::Branch { target, when } => {
                if index_of(address, instructions, target).is_some_and(reached) {
                    return true;
                }
                when != When::Always
            }
            Flow::Next | Flow::Call | Flow::CallRegister(..) | Flow::It { .. } => true,
            _ => false,
        };
        let runs_on = instructions
            .get(index + 1)
            .is_some_and(|next| next.offset == instruction.offset + instruction.size());
        goes_on && runs_on && reached(index + 1)
    })
}

/// Whether following the registers may find what the sweep didn't: the
/// code branches through a register, adds pc to a register, completes
/// a number with `movt`, or stores a register on the stack and then pops
/// pc, as the stubs that linkers add for Cortex-M0 do. Most functions
/// only call with `bl`, return with `bx lr` or by popping the lr they
/// pushed, and load addresses that the sweep finds as well. `effects`
/// holds `None` for the instructions the code can't reach.
fn may_find_more(effects: &[Option<Effect>]) -> bool {
    let branches_or_builds = |effect: &Effect| {
        matches!(
            effect.flow,
            Flow::CallRegister(..) | Flow::JumpRegister(..) | Flow::JumpRelative(_)
        ) || match effect.operation {
            Operation::Add { a, b, .. } => a == PC || b == PC,
            Operation::Copy { source, .. } => source == PC,
            Operation::HighHalf { .. } => true,
            _ => false,
        }
    };
    // Compilers pop pc only to return. The stubs store the address just
    // before they pop it.
    let pops_stored = |store: &Effect, pop: &Effect| {
        matches!(
            store.operation,
            Operation::Store {
                register: Some(_),
                base: SP,
                ..
            }
        ) && pop.flow == Flow::Pop
    };
    effects.iter().flatten().any(branches_or_builds)
        || effects.windows(2).any(|pair| match pair {
            [Some(store), Some(pop)] => pops_stored(store, pop),
            _ => false,
        })
}

/// Where a branch through a register goes with `number` in it: to
/// `number` without its low bit, unless that bit must be 1 and is 0.
/// Cortex-M cores only run Thumb code, so `bx` faults there, as for the
/// address 0 of a weak function that the program doesn't have.
fn target(number: u32, low_bit: LowBit) -> Option<u32> {
    match low_bit {
        LowBit::Thumb if number & 1 == 0 => None,
        _ => Some(number & !1),
    }
}

/// Whether adding what `a` and `b` hold adds the program counter.
fn adds_pc(a: u16, b: u16, state: &State) -> bool {
    let pc = |register: u16| {
        register == PC
            || state.registers[usize::from(register)]
                .values
                .iter()
                .any(|value| value.kind == Kind::ProgramCounter)
    };
    pc(a) || pc(b)
}

/// Records a use of `value`, if it is an address an instruction loaded or
/// built.
fn use_value(uses: &mut [Uses], value: &Value, how: Use) {
    let Some(source) = value.source else {
        return;
    };
    let uses = &mut uses[source as usize];
    match how {
        Use::Branch => uses.branch = true,
        Use::Part => uses.part = true,
        Use::Other => uses.other = true,
    }
}

/// What to do with a value elfq stops following, as where a register
/// holds too many: count it as used, since elfq can't tell how. A stale
/// one counts only if the code reads the register; see
/// [`Function::stale_lost`].
fn lost<'a>(
    uses: &'a mut [Uses],
    stale_lost: &'a mut BTreeSet<u32>,
) -> impl FnMut(&Value, bool) + 'a {
    |value, stale| match (stale, value.source) {
        (true, Some(source)) => _ = stale_lost.insert(source),
        _ => use_value(uses, value, Use::Other),
    }
}

/// `register` as a set.
fn bit(register: u16) -> Registers {
    1 << register
}

/// The registers in `set`, from r0 up.
fn registers(mut set: Registers) -> impl Iterator<Item = u16> {
    std::iter::from_fn(move || {
        let register = set.trailing_zeros() as u16;
        set &= set.wrapping_sub(1);
        (register < 16).then_some(register)
    })
}

#[cfg(test)]
mod tests {
    use object::Endianness::Little;

    use super::super::scan;
    use crate::arch::CodeReferenceKind::{self, *};

    /// Code made of 16-bit units, as a disassembler shows them: a 32-bit
    /// instruction as its first 16 bits, then the second, and a word as
    /// its low half, then the high.
    fn halfwords(units: &[u16]) -> Vec<u8> {
        units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
    }

    /// What elfq finds in `code` at 0x8000100.
    fn kinds(code: &[u8]) -> Vec<(u64, CodeReferenceKind)> {
        scan(code, 0x0800_0100, Little)
            .into_iter()
            .map(|reference| (reference.site, reference.kind))
            .collect()
    }

    #[test]
    fn a_call_through_a_register_set_to_a_constant_is_a_call() {
        // The address comes from a literal pool, as from GCC's long calls,
        // and isn't also reported as taken, since the code only calls it:
        //   8000100: b510       push  {r4, lr}
        //   8000102: 4b01       ldr   r3, [pc, #4]
        //   8000104: 4798       blx   r3
        //   8000106: bd10       pop   {r4, pc}
        //   8000108: 30000101   .word 0x30000101
        let code = halfwords(&[0xb510, 0x4b01, 0x4798, 0xbd10, 0x0101, 0x3000]);
        assert_eq!(kinds(&code), [(0x0800_0104, Call(0x3000_0100))]);
        // From movw and movt, as with -mpure-code:
        //   8000100: b510       push  {r4, lr}
        //   8000102: f240 1301  movw  r3, #0x101
        //   8000106: f2c3 0300  movt  r3, #0x3000
        //   800010a: 4798       blx   r3
        //   800010c: bd10       pop   {r4, pc}
        let code = halfwords(&[0xb510, 0xf240, 0x1301, 0xf2c3, 0x0300, 0x4798, 0xbd10]);
        assert_eq!(kinds(&code), [(0x0800_010a, Call(0x3000_0100))]);
        // A byte at a time, as on Cortex-M0 with -mpure-code:
        //   8000100: b510       push  {r4, lr}
        //   8000102: 2330       movs  r3, #0x30
        //   8000104: 021b       lsls  r3, r3, #8
        //   8000106: 3300       adds  r3, #0
        //   8000108: 021b       lsls  r3, r3, #8
        //   800010a: 3301       adds  r3, #1
        //   800010c: 021b       lsls  r3, r3, #8
        //   800010e: 333d       adds  r3, #0x3d
        //   8000110: 4798       blx   r3
        //   8000112: bd10       pop   {r4, pc}
        let code = halfwords(&[
            0xb510, 0x2330, 0x021b, 0x3300, 0x021b, 0x3301, 0x021b, 0x333d, 0x4798, 0xbd10,
        ]);
        assert_eq!(kinds(&code), [(0x0800_0110, Call(0x3000_013c))]);
        // As an offset from pc, as in position-independent code. Reading
        // pc at 0x8000104 gives 0x8000108:
        //   8000100: b510       push  {r4, lr}
        //   8000102: 4b02       ldr   r3, [pc, #8]
        //   8000104: 447b       add   r3, pc
        //   8000106: 4798       blx   r3
        //   8000108: bd10       pop   {r4, pc}
        //   800010a: bf00       nop
        //   800010c: 27fffff9   .word 0x27fffff9
        let code = halfwords(&[
            0xb510, 0x4b02, 0x447b, 0x4798, 0xbd10, 0xbf00, 0xfff9, 0x27ff,
        ]);
        assert_eq!(kinds(&code), [(0x0800_0106, Call(0x3000_0100))]);
    }

    #[test]
    fn a_jump_through_a_register_set_to_a_constant_is_a_tail_call() {
        //   8000100: 4b00       ldr   r3, [pc, #0]
        //   8000102: 4718       bx    r3
        //   8000104: 30000101   .word 0x30000101
        let code = halfwords(&[0x4b00, 0x4718, 0x0101, 0x3000]);
        assert_eq!(kinds(&code), [(0x0800_0102, TailJump(0x3000_0100))]);
    }

    #[test]
    fn a_register_that_may_hold_several_addresses_calls_each() {
        // Picked by an `it` block:
        //   8000100: b510       push  {r4, lr}
        //   8000102: 2805       cmp   r0, #5
        //   8000104: bfcc       ite   gt
        //   8000106: 4b02       ldrgt r3, [pc, #8]
        //   8000108: 4b02       ldrle r3, [pc, #8]
        //   800010a: 4798       blx   r3
        //   800010c: bd10       pop   {r4, pc}
        //   800010e: 0000       .short 0x0000
        //   8000110: 30000101   .word 0x30000101
        //   8000114: 30000201   .word 0x30000201
        let code = halfwords(&[
            0xb510, 0x2805, 0xbfcc, 0x4b02, 0x4b02, 0x4798, 0xbd10, 0x0000, 0x0101, 0x3000, 0x0201,
            0x3000,
        ]);
        assert_eq!(
            kinds(&code),
            [
                (0x0800_010a, Call(0x3000_0100)),
                (0x0800_010a, Call(0x3000_0200)),
            ]
        );
        // A pointer loaded from memory, or a constant when it is null: a
        // call to the constant, and one through the pointer.
        //   8000100: b510       push  {r4, lr}
        //   8000102: 6803       ldr   r3, [r0, #0]
        //   8000104: b903       cbnz  r3, 8000108
        //   8000106: 4b01       ldr   r3, [pc, #4]
        //   8000108: 4798       blx   r3
        //   800010a: bd10       pop   {r4, pc}
        //   800010c: 30000101   .word 0x30000101
        let code = halfwords(&[
            0xb510, 0x6803, 0xb903, 0x4b01, 0x4798, 0xbd10, 0x0101, 0x3000,
        ]);
        assert_eq!(
            kinds(&code),
            [
                (0x0800_0104, Jump(0x0800_0108)),
                (0x0800_0108, Call(0x3000_0100)),
                (0x0800_0108, IndirectCall),
            ]
        );
    }

    #[test]
    fn it_blocks_on_the_same_flags_run_on_the_same_path() {
        // GCC builds far_a | 1 = 0x30000179 over two `it` blocks, and loads
        // from a table where the first block's condition holds:
        //   8000100: b508       push    {r3, lr}
        //   8000102: 2803       cmp     r0, #3
        //   8000104: bf9b       ittet   ls
        //   8000106: f240 22c0  movwls  r2, #0x2c0
        //   800010a: f2c3 0200  movtls  r2, #0x3000
        //   800010e: f240 1379  movwhi  r3, #0x179
        //   8000112: f852 3020  ldrls.w r3, [r2, r0, lsl #2]
        //   8000116: 4608       mov     r0, r1
        //   8000118: bf88       it      hi
        //   800011a: f2c3 0300  movthi  r3, #0x3000
        //   800011e: 4798       blx     r3
        //   8000120: bd08       pop     {r3, pc}
        let code = halfwords(&[
            0xb508, 0x2803, 0xbf9b, 0xf240, 0x22c0, 0xf2c3, 0x0200, 0xf240, 0x1379, 0xf852, 0x3020,
            0x4608, 0xbf88, 0xf2c3, 0x0300, 0x4798, 0xbd08,
        ]);
        assert_eq!(
            kinds(&code),
            [
                (0x0800_010a, Address(0x3000_02c0)),
                (0x0800_011e, Call(0x3000_0178)),
                (0x0800_011e, IndirectCall),
            ]
        );
        // Flags that change between the blocks join the paths.
        //   8000116: 1c08       adds    r0, r1, #0
        let mut code = code;
        code[0x16..0x18].copy_from_slice(&0x1c08_u16.to_le_bytes());
        assert!(kinds(&code).contains(&(0x0800_011e, Call(0x178))));
    }

    #[test]
    fn a_branch_to_an_even_address_goes_nowhere() {
        // Unoptimized code loads a weak function's address again after
        // testing it. The program doesn't have the function, so it is 0,
        // and Cortex-M cores fault on a branch to an even address.
        //   8000100: b580       push    {r7, lr}
        //   8000102: 4b03       ldr     r3, [pc, #12]
        //   8000104: 2b00       cmp     r3, #0
        //   8000106: d001       beq     800010c
        //   8000108: 4b01       ldr     r3, [pc, #4]
        //   800010a: 4798       blx     r3
        //   800010c: bd80       pop     {r7, pc}
        //   800010e: bf00       nop
        //   8000110: 00000000   .word   0x00000000
        let code = halfwords(&[
            0xb580, 0x4b03, 0x2b00, 0xd001, 0x4b01, 0x4798, 0xbd80, 0xbf00, 0x0000, 0x0000,
        ]);
        assert_eq!(
            kinds(&code),
            [
                (0x0800_0102, Address(0)),
                (0x0800_0106, Jump(0x0800_010c)),
                (0x0800_0108, Address(0)),
            ]
        );
    }

    #[test]
    fn a_pointer_loaded_from_memory_stays_a_pointer() {
        // As when calling a routine through a table in a chip's ROM:
        //   8000100: b510       push  {r4, lr}
        //   8000102: 4b02       ldr   r3, [pc, #8]
        //   8000104: 681b       ldr   r3, [r3, #0]
        //   8000106: 4798       blx   r3
        //   8000108: bd10       pop   {r4, pc}
        //   800010a: 0000       .short 0x0000
        //   800010c: 1fff0040   .word 0x1fff0040
        let code = halfwords(&[
            0xb510, 0x4b02, 0x681b, 0x4798, 0xbd10, 0x0000, 0x0040, 0x1fff,
        ]);
        assert_eq!(
            kinds(&code),
            [
                (0x0800_0102, Address(0x1fff_0040)),
                (0x0800_0106, IndirectCall)
            ]
        );
    }

    #[test]
    fn a_call_may_change_the_registers_that_calls_need_not_keep() {
        // The bl may take the address in r3 as an argument. It may change
        // r3, or keep it, as code compiled with GCC's -fipa-ra counts on,
        // so blx r3 may call the address or anything. r5 keeps its
        // address.
        //   8000100: b570       push  {r4, r5, r6, lr}
        //   8000102: 4b03       ldr   r3, [pc, #12]
        //   8000104: 4d03       ldr   r5, [pc, #12]
        //   8000106: f7ff fffb  bl    8000100
        //   800010a: 4798       blx   r3
        //   800010c: 47a8       blx   r5
        //   800010e: bd70       pop   {r4, r5, r6, pc}
        //   8000110: 30000101   .word 0x30000101
        //   8000114: 30000201   .word 0x30000201
        let code = halfwords(&[
            0xb570, 0x4b03, 0x4d03, 0xf7ff, 0xfffb, 0x4798, 0x47a8, 0xbd70, 0x0101, 0x3000, 0x0201,
            0x3000,
        ]);
        assert_eq!(
            kinds(&code),
            [
                (0x0800_0102, Address(0x3000_0101)),
                (0x0800_0106, Call(0x0800_0100)),
                (0x0800_010a, Call(0x3000_0100)),
                (0x0800_010a, IndirectCall),
                (0x0800_010c, Call(0x3000_0200)),
            ]
        );
    }

    #[test]
    fn a_register_kept_across_a_call_is_followed() {
        // GCC -O2 with -fipa-ra, which knows that the function keeps r2:
        //   8000100: b508       push  {r3, lr}
        //   8000102: 4a03       ldr   r2, [pc, #12]
        //   8000104: 4790       blx   r2
        //   8000106: 4603       mov   r3, r0
        //   8000108: 4790       blx   r2
        //   800010a: 4418       add   r0, r3
        //   800010c: bd08       pop   {r3, pc}
        //   800010e: 0000       .short 0x0000
        //   8000110: 30000101   .word 0x30000101
        let code = halfwords(&[
            0xb508, 0x4a03, 0x4790, 0x4603, 0x4790, 0x4418, 0xbd08, 0x0000, 0x0101, 0x3000,
        ]);
        assert_eq!(
            kinds(&code),
            [
                (0x0800_0104, Call(0x3000_0100)),
                (0x0800_0108, Call(0x3000_0100)),
                (0x0800_0108, IndirectCall),
            ]
        );
    }

    /// Nine times `cmp r0, #k; bne next; movw r3, #0x101; movt r3,
    /// #0x3000; blx r3; b join`, then at join, `str r3, [r1]` if `reads`,
    /// and `pop {r3, pc}`.
    fn nine_calls(reads: bool) -> Vec<u8> {
        let mut units = vec![0xb508];
        let join: i32 = 1 + 9 * 8;
        for k in 0..9 {
            units.extend([0x2800 | k, 0xd105, 0xf240, 0x1301, 0xf2c3, 0x0300, 0x4798]);
            let offset = join - (units.len() as i32 + 2);
            units.push(0xe000 | (offset as u16 & 0x7ff));
        }
        if reads {
            units.push(0x600b);
        }
        units.push(0xbd08);
        halfwords(&units)
    }

    #[test]
    fn a_kept_address_that_elfq_stops_following_counts_only_if_read() {
        // What it finds, other than the jumps between the cases.
        let found = |reads| {
            let mut found = kinds(&nine_calls(reads));
            found.retain(|(_, kind)| !matches!(kind, Jump(_)));
            found
        };
        // r3 may hold any of the nine addresses at the join: more than elfq
        // follows. Popping r3 doesn't read them.
        let calls: Vec<_> = (0..9)
            .map(|k| (0x0800_010e + 16 * k, Call(0x3000_0100)))
            .collect();
        assert_eq!(found(false), calls);
        // Storing r3 may store any of them.
        let addresses: Vec<_> = (0..9)
            .map(|k| (0x0800_010a + 16 * k, Address(0x3000_0101)))
            .collect();
        let mut both = [calls, addresses].concat();
        both.sort_by_key(|&(site, _)| site);
        assert_eq!(found(true), both);
    }

    #[test]
    fn code_reached_only_through_a_table_leaves_the_function_to_the_sweep() {
        // GCC's switch for Cortex-M0, which jumps to each case through a
        // table of addresses elfq doesn't read; each case joins the
        // default one at the blx:
        //   8000100: b510       push  {r4, lr}
        //   8000102: 2801       cmp   r0, #1
        //   8000104: d807       bhi.n 8000116
        //   8000106: 4b05       ldr   r3, [pc, #20]
        //   8000108: 0080       lsls  r0, r0, #2
        //   800010a: 581b       ldr   r3, [r3, r0]
        //   800010c: 469f       mov   pc, r3
        //   800010e: 4c04       ldr   r4, [pc, #16]
        //   8000110: e002       b.n   8000118
        //   8000112: 4c04       ldr   r4, [pc, #16]
        //   8000114: e000       b.n   8000118
        //   8000116: 4c04       ldr   r4, [pc, #16]
        //   8000118: 47a0       blx   r4
        //   800011a: bd10       pop   {r4, pc}
        //   800011c: 08000200   .word 0x08000200
        //   8000120: 30000101   .word 0x30000101
        //   8000124: 30000201   .word 0x30000201
        //   8000128: 30000301   .word 0x30000301
        let code = halfwords(&[
            0xb510, 0x2801, 0xd807, 0x4b05, 0x0080, 0x581b, 0x469f, 0x4c04, 0xe002, 0x4c04, 0xe000,
            0x4c04, 0x47a0, 0xbd10, 0x0200, 0x0800, 0x0101, 0x3000, 0x0201, 0x3000, 0x0301, 0x3000,
        ]);
        let found = kinds(&code);
        assert!(found.contains(&(0x0800_0118, IndirectCall)), "{found:x?}");
        assert!(found.contains(&(0x0800_010e, Address(0x3000_0101))));
        assert!(!found.contains(&(0x0800_0118, Call(0x3000_0300))));
    }

    #[test]
    fn an_address_passed_on_the_stack_is_used() {
        // The fifth argument to the bl goes on the stack:
        //   8000100: b510       push  {r4, lr}
        //   8000102: b082       sub   sp, #8
        //   8000104: 4c05       ldr   r4, [pc, #20]
        //   8000106: 47a0       blx   r4
        //   8000108: 9400       str   r4, [sp, #0]
        //   800010a: 2304       movs  r3, #4
        //   800010c: 2203       movs  r2, #3
        //   800010e: 2102       movs  r1, #2
        //   8000110: 2001       movs  r0, #1
        //   8000112: f7ff fff5  bl    8000100
        //   8000116: b002       add   sp, #8
        //   8000118: bd10       pop   {r4, pc}
        //   800011a: 0000       .short 0x0000
        //   800011c: 30000101   .word 0x30000101
        let code = halfwords(&[
            0xb510, 0xb082, 0x4c05, 0x47a0, 0x9400, 0x2304, 0x2203, 0x2102, 0x2001, 0xf7ff, 0xfff5,
            0xb002, 0xbd10, 0x0000, 0x0101, 0x3000,
        ]);
        assert_eq!(
            kinds(&code),
            [
                (0x0800_0104, Address(0x3000_0101)),
                (0x0800_0106, Call(0x3000_0100)),
                (0x0800_0112, Call(0x0800_0100)),
            ]
        );
    }

    #[test]
    fn a_store_through_an_index_into_the_stack_forgets_it() {
        // GCC for Cortex-M0, for table[i] = h on a table on the stack,
        // with sp in the second register:
        //   8000100: b510       push  {r4, lr}
        //   8000102: b084       sub   sp, #16
        //   8000104: 4b04       ldr   r3, [pc, #16]
        //   8000106: 9300       str   r3, [sp, #0]
        //   8000108: 466b       mov   r3, sp
        //   800010a: 0080       lsls  r0, r0, #2
        //   800010c: 50c1       str   r1, [r0, r3]
        //   800010e: 9b00       ldr   r3, [sp, #0]
        //   8000110: 4798       blx   r3
        //   8000112: b004       add   sp, #16
        //   8000114: bd10       pop   {r4, pc}
        //   8000116: 0000       .short 0x0000
        //   8000118: 30000101   .word 0x30000101
        let code = halfwords(&[
            0xb510, 0xb084, 0x4b04, 0x9300, 0x466b, 0x0080, 0x50c1, 0x9b00, 0x4798, 0xb004, 0xbd10,
            0x0000, 0x0101, 0x3000,
        ]);
        assert_eq!(
            kinds(&code),
            [
                (0x0800_0104, Address(0x3000_0101)),
                (0x0800_0110, IndirectCall),
            ]
        );
    }

    #[test]
    fn some_branches_go_to_even_addresses() {
        // blxns calls Non-secure code at an even address:
        //   8000100: b510       push  {r4, lr}
        //   8000102: f44f 1400  mov.w r4, #0x200000
        //   8000106: 47a4       blxns r4
        //   8000108: bd10       pop   {r4, pc}
        let code = halfwords(&[0xb510, 0xf44f, 0x1400, 0x47a4, 0xbd10]);
        assert_eq!(kinds(&code), [(0x0800_0106, Call(0x0020_0000))]);
        // mov pc ignores the low bit:
        //   8000100: 4801       ldr   r0, [pc, #4]
        //   8000102: 4687       mov   pc, r0
        //   8000104: 0000       .short 0x0000
        //   8000108: 30000028   .word 0x30000028
        let code = halfwords(&[0x4801, 0x4687, 0x0000, 0x0000, 0x0028, 0x3000]);
        assert_eq!(kinds(&code), [(0x0800_0102, TailJump(0x3000_0028))]);
    }

    #[test]
    fn addresses_are_followed_around_loops_and_past_returns() {
        //   8000100: b570       push  {r4, r5, r6, lr}
        //   8000102: 4c03       ldr   r4, [pc, #12]
        //   8000104: 260a       movs  r6, #10
        //   8000106: 47a0       blx   r4
        //   8000108: 3e01       subs  r6, #1
        //   800010a: d1fc       bne.n 8000106
        //   800010c: bd70       pop   {r4, r5, r6, pc}
        //   800010e: 0000       .short 0x0000
        //   8000110: 30000101   .word 0x30000101
        let code = halfwords(&[
            0xb570, 0x4c03, 0x260a, 0x47a0, 0x3e01, 0xd1fc, 0xbd70, 0x0000, 0x0101, 0x3000,
        ]);
        assert_eq!(
            kinds(&code),
            [
                (0x0800_0106, Call(0x3000_0100)),
                (0x0800_010a, Jump(0x0800_0106)),
            ]
        );
        // The pop at 0x8000108 returns, but only on the path that skips the
        // blx, which still gets r7:
        //   8000100: b5f0       push  {r4, r5, r6, r7, lr}
        //   8000102: 4f03       ldr   r7, [pc, #12]
        //   8000104: 2800       cmp   r0, #0
        //   8000106: d100       bne.n 800010a
        //   8000108: bdf0       pop   {r4, r5, r6, r7, pc}
        //   800010a: 47b8       blx   r7
        //   800010c: bdf0       pop   {r4, r5, r6, r7, pc}
        //   800010e: 0000       .short 0x0000
        //   8000110: 30000101   .word 0x30000101
        let code = halfwords(&[
            0xb5f0, 0x4f03, 0x2800, 0xd100, 0xbdf0, 0x47b8, 0xbdf0, 0x0000, 0x0101, 0x3000,
        ]);
        assert_eq!(
            kinds(&code),
            [
                (0x0800_0106, Jump(0x0800_010a)),
                (0x0800_010a, Call(0x3000_0100)),
            ]
        );
    }

    #[test]
    fn addresses_are_followed_through_the_stack() {
        // Stored and loaded back through r7, which points into the stack,
        // as without optimization:
        //   8000100: b580       push  {r7, lr}
        //   8000102: b082       sub   sp, #8
        //   8000104: af00       add   r7, sp, #0
        //   8000106: 4b04       ldr   r3, [pc, #16]
        //   8000108: 607b       str   r3, [r7, #4]
        //   800010a: f7ff fff9  bl    8000100
        //   800010e: 687b       ldr   r3, [r7, #4]
        //   8000110: 4798       blx   r3
        //   8000112: 3708       adds  r7, #8
        //   8000114: 46bd       mov   sp, r7
        //   8000116: bd80       pop   {r7, pc}
        //   8000118: 30000101   .word 0x30000101
        let code = halfwords(&[
            0xb580, 0xb082, 0xaf00, 0x4b04, 0x607b, 0xf7ff, 0xfff9, 0x687b, 0x4798, 0x3708, 0x46bd,
            0xbd80, 0x0101, 0x3000,
        ]);
        // r3 still holds the address at the bl, which may take it as an
        // argument.
        assert_eq!(
            kinds(&code),
            [
                (0x0800_0106, Address(0x3000_0101)),
                (0x0800_010a, Call(0x0800_0100)),
                (0x0800_0110, Call(0x3000_0100)),
            ]
        );
        // The stub that LLD adds for Cortex-M0 stores the address over the
        // r1 it pushed, and pops it into pc:
        //   8000100: b403       push  {r0, r1}
        //   8000102: 4801       ldr   r0, [pc, #4]
        //   8000104: 9001       str   r0, [sp, #4]
        //   8000106: bd01       pop   {r0, pc}
        //   8000108: 30000101   .word 0x30000101
        let code = halfwords(&[0xb403, 0x4801, 0x9001, 0xbd01, 0x0101, 0x3000]);
        assert_eq!(kinds(&code), [(0x0800_0106, TailJump(0x3000_0100))]);
    }

    #[test]
    fn a_function_that_gets_the_stack_s_address_may_change_it() {
        // r0 points at the word that holds the address, so the bl may
        // write another there:
        //   8000100: b510       push  {r4, lr}
        //   8000102: b082       sub   sp, #8
        //   8000104: 4b04       ldr   r3, [pc, #16]
        //   8000106: 9301       str   r3, [sp, #4]
        //   8000108: a801       add   r0, sp, #4
        //   800010a: f7ff fff9  bl    8000100
        //   800010e: 9b01       ldr   r3, [sp, #4]
        //   8000110: 4798       blx   r3
        //   8000112: b002       add   sp, #8
        //   8000114: bd10       pop   {r4, pc}
        //   8000116: 0000       .short 0x0000
        //   8000118: 30000101   .word 0x30000101
        let code = halfwords(&[
            0xb510, 0xb082, 0x4b04, 0x9301, 0xa801, 0xf7ff, 0xfff9, 0x9b01, 0x4798, 0xb002, 0xbd10,
            0x0000, 0x0101, 0x3000,
        ]);
        assert_eq!(
            kinds(&code),
            [
                (0x0800_0104, Address(0x3000_0101)),
                (0x0800_010a, Call(0x0800_0100)),
                (0x0800_0110, IndirectCall),
            ]
        );
    }

    #[test]
    fn a_veneer_that_adds_pc_jumps_to_its_target() {
        // GNU ld's --pic-veneer. Reading pc at 0x8000104 gives 0x8000108:
        //   8000100: b401       push  {r0}
        //   8000102: 4802       ldr   r0, [pc, #8]
        //   8000104: 46fc       mov   ip, pc
        //   8000106: 4484       add   ip, r0
        //   8000108: bc01       pop   {r0}
        //   800010a: 4760       bx    ip
        //   800010c: 27fffff9   .word 0x27fffff9
        let code = halfwords(&[
            0xb401, 0x4802, 0x46fc, 0x4484, 0xbc01, 0x4760, 0xfff9, 0x27ff,
        ]);
        assert_eq!(kinds(&code), [(0x0800_010a, TailJump(0x3000_0100))]);
    }

    #[test]
    fn code_that_never_runs_makes_no_calls() {
        // A call to a weak function that isn't there, whose address is 0,
        // only when it isn't 0, as in crtstuff's register_tm_clones:
        //   8000100: f04f 0300  mov.w r3, #0
        //   8000104: b113       cbz   r3, 800010c
        //   8000106: b510       push  {r4, lr}
        //   8000108: 4798       blx   r3
        //   800010a: bd10       pop   {r4, pc}
        //   800010c: 4770       bx    lr
        let code = halfwords(&[0xf04f, 0x0300, 0xb113, 0xb510, 0x4798, 0xbd10, 0x4770]);
        assert_eq!(kinds(&code), [(0x0800_0104, Jump(0x0800_010c))]);
    }
}
