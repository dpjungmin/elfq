//! What elfq knows a function's registers and stack hold at one point in
//! its code: the numbers each may hold, as far as it can tell.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;

use crate::arch::thumb::{PC, SP};

/// How many numbers elfq follows in one register or word on the stack.
/// Past that, it stops following it until code writes it again.
pub(super) const LIMIT: usize = 8;

/// The register that a function returns its result in: r0.
pub(super) const RESULT: u16 = 0;

/// What a number is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Kind {
    /// A number, which may be an address.
    Number,
    /// The program counter, as `mov ip, pc` reads it: adding an offset to
    /// it makes an address.
    ProgramCounter,
    /// An address on the stack, as an offset from where sp pointed when
    /// the function started.
    Stack,
}

/// A number that a register or a word on the stack may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Value {
    pub number: u32,
    pub kind: Kind,
    /// The instruction that loaded or built this number as an address, as
    /// an index into the function's instructions. A number made from it,
    /// as by adding to it, keeps it.
    pub source: Option<u32>,
    /// Whether it is what a register held before a call that may have
    /// changed it; see [`Contents::after_call`].
    pub stale: bool,
}

impl Value {
    /// A number that no instruction loaded or built as an address.
    pub const fn number(number: u32) -> Value {
        Value {
            number,
            kind: Kind::Number,
            source: None,
            stale: false,
        }
    }

    /// The address `offset` bytes from where sp pointed when the function
    /// started.
    pub fn stack(offset: i32) -> Value {
        Value {
            number: offset as u32,
            kind: Kind::Stack,
            source: None,
            stale: false,
        }
    }
}

/// Up to [`LIMIT`] numbers, sorted. They are kept in place rather than on
/// the heap, since copying states is much of what elfq does here.
#[derive(Debug, Clone, Copy)]
pub(super) struct Values {
    len: u8,
    items: [Value; LIMIT],
}

impl Values {
    /// No numbers.
    pub const NONE: Values = Values {
        len: 0,
        items: [Value::number(0); LIMIT],
    };

    /// Adds `value`. A number that is stale on one path and not on
    /// another is one the code may use, so it is not stale. Returns whether
    /// it is new or no longer stale, or `None` when it is new and there's
    /// no room for it.
    pub fn insert(&mut self, value: Value) -> Option<bool> {
        let len = usize::from(self.len);
        // Values sort by these first, and no two share them, so this finds
        // the number whether or not it is stale.
        let key = |value: &Value| (value.number, value.kind, value.source);
        match self.binary_search_by(|item| key(item).cmp(&key(&value))) {
            Ok(at) if self.items[at].stale && !value.stale => {
                self.items[at].stale = false;
                Some(true)
            }
            Ok(_) => Some(false),
            Err(_) if len == LIMIT => None,
            Err(at) => {
                self.items.copy_within(at..len, at + 1);
                self.items[at] = value;
                self.len += 1;
                Some(true)
            }
        }
    }

    /// Keeps the numbers for which `keep` is true.
    pub fn retain(&mut self, keep: impl Fn(&Value) -> bool) {
        let mut kept = 0;
        for index in 0..usize::from(self.len) {
            if keep(&self.items[index]) {
                self.items[kept] = self.items[index];
                kept += 1;
            }
        }
        self.len = kept as u8;
    }
}

impl Deref for Values {
    type Target = [Value];

    fn deref(&self) -> &[Value] {
        &self.items[..usize::from(self.len)]
    }
}

impl PartialEq for Values {
    fn eq(&self, other: &Values) -> bool {
        **self == **other
    }
}

impl Eq for Values {}

/// What elfq knows a register, or a word on the stack, holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Contents {
    /// The numbers it may hold.
    pub values: Values,
    /// Whether it may also hold something elfq can't tell.
    pub unknown: bool,
    /// Whether it may hold more than [`LIMIT`] numbers. It then holds no
    /// values, and gains none where paths join, until code writes it.
    pub saturated: bool,
    /// When `saturated`, whether some of the numbers elfq stopped following
    /// were stale; see [`Value::stale`].
    pub stale: bool,
}

impl Contents {
    /// Something elfq can't tell.
    pub fn unknown() -> Contents {
        Contents {
            values: Values::NONE,
            unknown: true,
            saturated: false,
            stale: false,
        }
    }

    /// `value`, and nothing else.
    pub fn holding(value: Value) -> Contents {
        let mut values = Values::NONE;
        values.insert(value);
        Contents {
            values,
            unknown: false,
            saturated: false,
            stale: false,
        }
    }

    /// More than [`LIMIT`] numbers.
    fn saturated() -> Contents {
        Contents {
            values: Values::NONE,
            unknown: true,
            saturated: true,
            stale: false,
        }
    }

    /// What a register that a call may change holds after it: something
    /// elfq can't tell, or what it held before, which is then stale. Code
    /// compiled with GCC's `-fipa-ra` counts on the second when it knows
    /// that the function it calls keeps r0 to r3 or ip. For r0, the first
    /// is the result; see [`State::get`].
    pub fn after_call(&self) -> Contents {
        match (self.saturated, self.stale) {
            (true, true) => *self,
            (true, false) => Contents::unknown(),
            (false, _) => {
                let mut contents = Contents::unknown();
                for &value in self.values.iter() {
                    contents.values.insert(Value {
                        stale: true,
                        ..value
                    });
                }
                contents
            }
        }
    }

    /// Whether some of the numbers these contents may hold, or that elfq
    /// stopped following, are stale.
    fn has_stale(&self) -> bool {
        self.stale || self.values.iter().any(|value| value.stale)
    }

    /// `f` of each number these contents may hold, where `f` returns
    /// `None` for one that makes something elfq can't tell.
    pub fn map(&self, f: impl Fn(Value) -> Option<Value>) -> Contents {
        if self.saturated {
            return Contents::unknown();
        }
        let mut contents = Contents {
            values: Values::NONE,
            unknown: self.unknown,
            saturated: false,
            stale: false,
        };
        for &value in self.values.iter() {
            match f(value) {
                // As many as there were, at most.
                Some(value) => _ = contents.values.insert(value),
                None => contents.unknown = true,
            }
        }
        contents
    }

    /// The offset from the function's starting sp of the one address on
    /// the stack these contents hold, if they hold just that.
    pub fn stack_offset(&self) -> Option<i32> {
        match (&*self.values, self.unknown) {
            ([value], false) if value.kind == Kind::Stack => Some(value.number as i32),
            _ => None,
        }
    }

    /// Whether these contents may be an address on the stack.
    pub fn may_be_stack(&self) -> bool {
        self.values.iter().any(|value| value.kind == Kind::Stack)
    }

    /// Leaves the numbers that are 0 if `zero`, or those that aren't. What
    /// elfq can't tell is 0 where it is 0. Contents left with nothing can't
    /// be what they are where code tests them this way, so the code there
    /// never runs.
    fn narrow(&mut self, zero: bool) {
        if self.saturated {
            return;
        }
        self.values.retain(|value| (value.number == 0) == zero);
        if zero && self.unknown {
            // There is room: it holds only 0s.
            self.values.insert(Value::number(0));
            self.unknown = false;
        }
    }

    /// Adds what `other` may hold, where two paths through the code join.
    /// Returns whether these contents changed. Past [`LIMIT`] numbers they
    /// are saturated, and each number they lose goes to `lost`.
    pub fn join(&mut self, other: &Contents, lost: &mut impl FnMut(&Value)) -> bool {
        if self.saturated {
            other.values.iter().for_each(&mut *lost);
            let changed = other.has_stale() && !self.stale;
            self.stale |= other.has_stale();
            return changed;
        }
        if other.saturated {
            self.values.iter().for_each(&mut *lost);
            *self = Contents {
                stale: self.has_stale() || other.stale,
                ..Contents::saturated()
            };
            return true;
        }
        let before = *self;
        let mut changed = false;
        if other.unknown && !self.unknown {
            self.unknown = true;
            changed = true;
        }
        for value in other.values.iter() {
            match self.values.insert(*value) {
                Some(new) => changed |= new,
                None => {
                    before
                        .values
                        .iter()
                        .chain(other.values.iter())
                        .for_each(&mut *lost);
                    *self = Contents {
                        stale: before.has_stale() || other.has_stale(),
                        ..Contents::saturated()
                    };
                    return true;
                }
            }
        }
        changed
    }

    /// Whether these contents are just something elfq can't tell, as a
    /// word on the stack is unless the function stored a number there.
    fn is_unknown(&self) -> bool {
        self.unknown && self.values.is_empty() && !self.saturated
    }
}

/// What elfq knows the registers and the stack hold at one point in a
/// function's code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct State {
    /// r0 to pc. pc always holds something elfq can't tell: it reads pc
    /// from the instruction's address instead.
    pub registers: [Contents; 16],
    /// The words on the stack that the function stored numbers in, by
    /// their offset from where sp pointed when the function started. Any
    /// other word holds something elfq can't tell.
    pub stack: BTreeMap<i32, Contents>,
    /// Whether an address on the stack may be where elfq can't follow it,
    /// as in a register that holds something elfq can't tell, or with a
    /// function the code called. Code could then change the stack where
    /// elfq can't see it: through such a register, or in a function it
    /// calls.
    pub escaped: bool,
}

impl State {
    /// The state where the function starts: sp points at its stack, and
    /// everything else holds something elfq can't tell.
    pub fn entry() -> State {
        let mut registers: [Contents; 16] = std::array::from_fn(|_| Contents::unknown());
        registers[usize::from(SP)] = Contents::holding(Value::stack(0));
        State {
            registers,
            stack: BTreeMap::new(),
            escaped: false,
        }
    }

    /// What `register` holds for an instruction that reads it, where
    /// reading pc gives `pc`. After a call, r0 holds what the function
    /// returns, which elfq can't tell. Code reads r1 to r3 or ip after a
    /// call only when it knows the call kept them, so what they held is no
    /// longer stale.
    pub fn get(&self, register: u16, pc: u32) -> Contents {
        let mut contents = self.registers[usize::from(register)];
        if register != RESULT || !contents.has_stale() {
            return self.kept(register, pc);
        }
        if contents.saturated {
            return Contents::unknown();
        }
        contents.values.retain(|value| !value.stale);
        contents.unknown = true;
        contents
    }

    /// What `register` holds, where reading pc gives `pc`, taking what it
    /// held before a call to be there still.
    fn kept(&self, register: u16, pc: u32) -> Contents {
        let contents = self.registers[usize::from(register)];
        if register == PC {
            return Contents::holding(Value {
                number: pc,
                kind: Kind::ProgramCounter,
                source: None,
                stale: false,
            });
        }
        if !contents.has_stale() {
            return contents;
        }
        let mut kept = Contents {
            values: Values::NONE,
            stale: false,
            ..contents
        };
        for &value in contents.values.iter() {
            // As many as there were, at most.
            kept.values.insert(Value {
                stale: false,
                ..value
            });
        }
        kept
    }

    /// Leaves in `register` the numbers that are 0 if `zero`, or those that
    /// aren't, where code tests whether it is 0; see [`Contents::narrow`].
    pub fn narrow(&mut self, register: u16, zero: bool) {
        let mut contents = self.get(register, 0);
        contents.narrow(zero);
        self.registers[usize::from(register)] = contents;
    }

    /// Where sp points, as an offset from where it pointed when the
    /// function started, if elfq can tell.
    pub fn sp(&self) -> Option<i32> {
        self.registers[usize::from(SP)].stack_offset()
    }

    /// What the word on the stack at `offset` holds.
    pub fn word(&self, offset: i32) -> Contents {
        self.stack
            .get(&offset)
            .copied()
            .unwrap_or_else(Contents::unknown)
    }

    /// Sets the word on the stack at `offset`.
    pub fn set_word(&mut self, offset: i32, contents: Contents) {
        if contents.is_unknown() {
            self.stack.remove(&offset);
        } else {
            self.stack.insert(offset, contents);
        }
    }

    /// Forgets every word on the stack, giving each number it held to
    /// `lost`.
    pub fn forget_stack(&mut self, lost: &mut impl FnMut(&Value)) {
        for value in self
            .stack
            .values()
            .flat_map(|contents| contents.values.iter())
        {
            self.escaped |= value.kind == Kind::Stack;
            lost(value);
        }
        self.stack.clear();
    }

    /// Adds what `other` may hold, where two paths through the code join.
    /// Returns whether this state changed.
    pub fn join(&mut self, other: &State, lost: &mut impl FnMut(&Value)) -> bool {
        let mut changed = false;
        let mut escaped = other.escaped;
        let mut lose = |value: &Value| {
            escaped |= value.kind == Kind::Stack;
            lost(value);
        };
        for (contents, other) in self.registers.iter_mut().zip(&other.registers) {
            changed |= contents.join(other, &mut lose);
        }
        // A word that one side doesn't hold holds something elfq can't
        // tell there.
        let offsets: BTreeSet<i32> = self
            .stack
            .keys()
            .chain(other.stack.keys())
            .copied()
            .collect();
        for offset in offsets {
            let mut contents = self.word(offset);
            if contents.join(&other.word(offset), &mut lose) {
                changed = true;
                self.set_word(offset, contents);
            }
        }
        if escaped && !self.escaped {
            self.escaped = true;
            changed = true;
        }
        changed
    }
}
