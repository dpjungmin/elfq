//! What an instruction that goes on to the next does to what the walk
//! knows ([`Walker::execute`]): the numbers it writes in registers, the
//! flags it sets, and the words it stores and loads, with the arithmetic
//! on numbers that the walk may not know, such as [`add`] and [`holds`].

use std::rc::Rc;

use object::Endian;

use crate::arch::startup::{Event, Source, Store};
use crate::arch::thumb::decode::{Effect, Operation, SetsFlags, Words, each};
use crate::arch::thumb::{ARGUMENTS, PC, SP};

use super::Walker;
use super::calls::results;
use super::walk_state::{Compare, Flags, Path, set_unknown};

impl Walker<'_> {
    /// Changes `path` as the instruction at `site` does, other than where
    /// it goes next. When it is `conditional`, in an `it` block whose
    /// condition the walk can't tell, what it writes becomes unknown.
    pub(super) fn execute(
        &mut self,
        path: &mut Path,
        site: u64,
        effect: &Effect,
        changes_flags: bool,
        conditional: bool,
        depth: u32,
    ) {
        path.results |= results(effect);
        path.left &= !(effect.writes & ARGUMENTS) as u8;
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
            // movs keeps the carry flag; movs.w and mvns with an immediate
            // may set it.
            Operation::Constant { value, .. } => {
                let narrow = effect.flags == SetsFlags::OutsideIt;
                logic(Some(value), if narrow { old.carry } else { None })
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
    pub(super) fn access(&self, path: &Path, site: u64, effect: &Effect) -> Option<u32> {
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
    pub(super) fn is_stack(&self, address: Option<u32>) -> bool {
        match (address, self.lowest, self.top) {
            (Some(address), Some(lowest), Some(top)) => (lowest..top).contains(&address),
            _ => false,
        }
    }

    /// The word at `address`: what code stored there on the way, or what
    /// is there at reset. A literal pool is read as the code is, when
    /// `literal`, as if copied into place, unless code stored a word the
    /// walk knows there.
    pub(super) fn load(&self, path: &Path, address: u32, literal: bool) -> Option<u32> {
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
    pub(super) fn register(&self, path: &Path, register: u16, site: u64) -> Option<u32> {
        match register {
            PC => Some((site as u32).wrapping_add(4)),
            _ => path.registers[usize::from(register)],
        }
    }
}

/// Whether condition `condition`, the field of `b<cond>` and `it`, holds
/// with `flags`, when the walk can tell.
pub(super) fn holds(condition: u16, flags: Flags) -> Option<bool> {
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
pub(super) fn subtract(a: Option<u32>, b: Option<u32>) -> Flags {
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
pub(super) fn add(a: Option<u32>, b: Option<u32>) -> Flags {
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

/// The registers whose words a load or store of more than one word loads
/// or stores, in order.
fn word_registers(words: Words) -> Vec<u16> {
    match words {
        Words::List { registers, .. } => each(registers).collect(),
        Words::Pair { first, second, .. } => vec![first, second],
    }
}
