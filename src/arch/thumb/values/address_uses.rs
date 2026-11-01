//! How a function uses each address that one of its instructions loads or
//! builds, so that `values` reports an address only where the code uses it
//! as one: not where it only branches there, nor where it builds another
//! address from it.

use std::collections::BTreeSet;

use super::state::{RESULT, State, Value};
use crate::arch::thumb::PC;
use crate::arch::thumb::decode::{Registers, each};

/// How the rest of a function uses an address that one instruction loaded
/// or built.
#[derive(Debug, Clone, Copy, Default)]
struct HowUsed {
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

impl HowUsed {
    /// Whether the address is one the code uses as such: one it doesn't
    /// only branch to or build another address from. An address nothing
    /// uses counts, since elfq can't tell what code it wasn't shown does.
    fn as_address(&self) -> bool {
        self.other || !(self.branch || self.part)
    }
}

/// A way code uses an address; see [`HowUsed`].
#[derive(Debug, Clone, Copy)]
pub(super) enum Use {
    Branch,
    Part,
    Other,
}

/// How the code uses the address that each instruction loads or builds, as
/// far as following the registers shows.
pub(super) struct AddressUses {
    /// By instruction.
    of: Vec<HowUsed>,
    /// The addresses that the code loads back from the stack: the offset
    /// of each word, as in [`State::stack`], with the instruction that
    /// loaded or built an address the code loads from there.
    loaded: BTreeSet<(i32, u32)>,
    /// The instructions that loaded or built an address that a register
    /// held, stale, where elfq stopped following it. They count as used
    /// as addresses if the code reads such a register.
    stale_lost: BTreeSet<u32>,
    /// Whether the code reads a register that holds stale numbers elfq
    /// stopped following.
    reads_stale_lost: bool,
}

impl AddressUses {
    /// No uses yet, in code of `instructions` instructions.
    pub(super) fn new(instructions: usize) -> AddressUses {
        AddressUses {
            of: vec![HowUsed::default(); instructions],
            loaded: BTreeSet::new(),
            stale_lost: BTreeSet::new(),
            reads_stale_lost: false,
        }
    }

    /// Whether the code uses what the instruction at `index` loads or
    /// builds as an address.
    pub(super) fn as_address(&self, index: usize) -> bool {
        self.of[index].as_address()
    }

    /// Records a use of `value`, if it is an address an instruction loaded
    /// or built.
    pub(super) fn record(&mut self, value: &Value, how: Use) {
        let Some(source) = value.source else {
            return;
        };
        let uses = &mut self.of[source as usize];
        match how {
            Use::Branch => uses.branch = true,
            Use::Part => uses.part = true,
            Use::Other => uses.other = true,
        }
    }

    /// What to do with a value elfq stops following, as where a register
    /// holds too many: count it as used, since elfq can't tell how. A stale
    /// one counts only if the code reads a register that holds such
    /// numbers; see [`AddressUses::stale_lost`].
    pub(super) fn lost(&mut self) -> impl FnMut(&Value) + '_ {
        |value| match (value.stale, value.source) {
            (true, Some(source)) => _ = self.stale_lost.insert(source),
            _ => self.record(value, Use::Other),
        }
    }

    /// Notes that an instruction reads each register of `registers` in
    /// `state`, for the stale numbers elfq stopped following there.
    pub(super) fn reads(&mut self, state: &State, registers: Registers) {
        self.reads_stale_lost |= each(registers).any(|register| {
            let contents = state.registers[usize::from(register)];
            contents.saturated && contents.stale
        });
    }

    /// Counts every stale number that elfq stopped following as used, if
    /// the code reads any register that holds stale numbers it stopped
    /// following. Called once the code has run a last time.
    pub(super) fn finish(&mut self) {
        if self.reads_stale_lost {
            for &source in &self.stale_lost {
                self.of[source as usize].other = true;
            }
        }
    }

    /// Records that the code loads back what the word on the stack at
    /// `offset` holds; see [`AddressUses::loaded`].
    pub(super) fn loads_back(&mut self, state: &State, offset: i32) {
        for value in state.word(offset).values.iter() {
            if let Some(source) = value.source {
                self.loaded.insert((offset, source));
            }
        }
    }

    // What the code does with the numbers in registers and on the stack,
    // which the last pass records.

    /// Records a use of the values in each register of `registers`, as the
    /// code reads them.
    pub(super) fn record_registers(&mut self, state: &State, registers: Registers, how: Use) {
        for register in each(registers).filter(|&register| register != PC) {
            for value in state.get(register, 0).values.iter() {
                self.record(value, how);
            }
        }
    }

    /// Records that a branch through `register` uses what r0 held before a
    /// call, when that is the register: elfq takes r0 to hold the call's
    /// result (see [`State::get`]), but code that knows the call kept r0
    /// may branch to what it held.
    pub(super) fn record_kept_result(&mut self, state: &State, register: u16) {
        if register != RESULT {
            return;
        }
        for value in state.registers[usize::from(register)].values.iter() {
            if value.stale {
                self.record(value, Use::Other);
            }
        }
    }

    /// Records that code reads the numbers in the word on the stack at
    /// `offset` where elfq can't follow them.
    pub(super) fn record_word(&mut self, state: &State, offset: i32) {
        for value in state.word(offset).values.iter() {
            self.record(value, Use::Other);
        }
    }

    /// Records that code may read the numbers in any word on the stack
    /// where elfq can't follow them.
    pub(super) fn record_stack(&mut self, state: &State) {
        for contents in state.stack.values() {
            for value in contents.values.iter() {
                self.record(value, Use::Other);
            }
        }
    }

    /// Records that a call, or a jump to another function, may take the
    /// numbers in the words on the stack from sp up as arguments: those
    /// that the code doesn't load back from there itself.
    pub(super) fn pass_stack(&mut self, state: &State) {
        let sp = state.sp();
        for (&offset, contents) in &state.stack {
            if sp.is_some_and(|sp| offset < sp) {
                continue;
            }
            for value in contents.values.iter() {
                let loaded_back = value
                    .source
                    .is_some_and(|source| self.loaded.contains(&(offset, source)));
                if !loaded_back {
                    self.record(value, Use::Other);
                }
            }
        }
    }

    /// Records that code passes the values in each register of `registers`
    /// on, without naming them: as arguments to a function, a result to
    /// the caller, or to code that elfq can't see. A stale number in one
    /// isn't passed, since code passes nothing in a register that a call
    /// may have changed.
    pub(super) fn pass_registers(&mut self, state: &State, registers: Registers) {
        for register in each(registers).filter(|&register| register != PC) {
            for value in state.registers[usize::from(register)].values.iter() {
                if !value.stale {
                    self.record(value, Use::Other);
                }
            }
        }
    }
}
