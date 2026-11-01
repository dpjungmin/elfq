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

/// What adding a number to some [`Values`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Inserted {
    /// They didn't hold it, or held it only as stale, and now hold it.
    Changed,
    /// They held it already.
    Unchanged,
    /// They didn't hold it, and have no room for it.
    Full,
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
    /// another is one the code may use, so it is not stale.
    pub fn insert(&mut self, value: Value) -> Inserted {
        let len = usize::from(self.len);
        // Values sort by these first, and no two share them, so this finds
        // the number whether or not it is stale.
        let key = |value: &Value| (value.number, value.kind, value.source);
        match self.binary_search_by(|item| key(item).cmp(&key(&value))) {
            Ok(at) if self.items[at].stale && !value.stale => {
                self.items[at].stale = false;
                Inserted::Changed
            }
            Ok(_) => Inserted::Unchanged,
            Err(_) if len == LIMIT => Inserted::Full,
            Err(at) => {
                self.items.copy_within(at..len, at + 1);
                self.items[at] = value;
                self.len += 1;
                Inserted::Changed
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

    /// These numbers, each stale if `stale` and not stale if not.
    fn with_stale(&self, stale: bool) -> Values {
        let mut values = Values::NONE;
        for &value in self.iter() {
            // As many as there were, at most.
            values.insert(Value { stale, ..value });
        }
        values
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
pub(super) enum Contents {
    /// One of `values`, or, with `or_unknown`, something elfq can't tell.
    Known { values: Values, or_unknown: bool },
    /// It may hold more than [`LIMIT`] numbers. elfq stopped following
    /// them: it holds something elfq can't tell, and gains no numbers where
    /// paths join, until code writes it again. `stale` says whether some of
    /// the numbers elfq stopped following were stale; see [`Value::stale`].
    TooMany { stale: bool },
}

impl Contents {
    /// Something elfq can't tell, and nothing else: what a register holds
    /// after an instruction that elfq doesn't follow writes it, and a word
    /// on the stack unless the function stored a number there.
    pub const UNKNOWN: Contents = Contents::Known {
        values: Values::NONE,
        or_unknown: true,
    };

    /// `value`, and nothing else.
    pub fn holding(value: Value) -> Contents {
        let mut values = Values::NONE;
        values.insert(value);
        Contents::Known {
            values,
            or_unknown: false,
        }
    }

    /// The numbers these contents may hold, as far as elfq follows them.
    pub fn values(&self) -> &[Value] {
        match self {
            Contents::Known { values, .. } => values,
            Contents::TooMany { .. } => &[],
        }
    }

    /// Whether these contents may hold something elfq can't tell.
    pub fn may_be_unknown(&self) -> bool {
        matches!(
            self,
            Contents::Known {
                or_unknown: true,
                ..
            } | Contents::TooMany { .. }
        )
    }

    /// What a register that a call may change holds after it: something
    /// elfq can't tell, or what it held before, which is then stale. Code
    /// compiled with GCC's `-fipa-ra` counts on the second when it knows
    /// that the function it calls keeps r0 to r3 or ip. For r0, the first
    /// is the result; see [`State::get`].
    pub fn after_call(&self) -> Contents {
        match self {
            Contents::TooMany { stale: true } => *self,
            Contents::TooMany { stale: false } => Contents::UNKNOWN,
            Contents::Known { values, .. } => Contents::Known {
                values: values.with_stale(true),
                or_unknown: true,
            },
        }
    }

    /// Whether some of the numbers these contents may hold, or that elfq
    /// stopped following, are stale.
    fn has_stale(&self) -> bool {
        match self {
            Contents::Known { values, .. } => values.iter().any(|value| value.stale),
            Contents::TooMany { stale } => *stale,
        }
    }

    /// `f` of each number these contents may hold, where `f` returns
    /// `None` for one that makes something elfq can't tell.
    pub fn map(&self, f: impl Fn(Value) -> Option<Value>) -> Contents {
        let Contents::Known { values, or_unknown } = self else {
            return Contents::UNKNOWN;
        };
        let mut mapped = Values::NONE;
        let mut or_unknown = *or_unknown;
        for &value in values.iter() {
            match f(value) {
                // As many as there were, at most.
                Some(value) => _ = mapped.insert(value),
                None => or_unknown = true,
            }
        }
        Contents::Known {
            values: mapped,
            or_unknown,
        }
    }

    /// The offset from the function's starting sp of the one address on
    /// the stack these contents hold, if they hold just that.
    pub fn stack_offset(&self) -> Option<i32> {
        match self {
            Contents::Known {
                values,
                or_unknown: false,
            } => match **values {
                [value] if value.kind == Kind::Stack => Some(value.number as i32),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether these contents may be an address on the stack.
    pub fn may_be_stack(&self) -> bool {
        self.values().iter().any(|value| value.kind == Kind::Stack)
    }

    /// Leaves the numbers that are 0 if `zero`, or those that aren't. What
    /// elfq can't tell is 0 where it is 0. Contents left with nothing can't
    /// be what they are where code tests them this way, so the code there
    /// never runs. elfq doesn't follow contents that may hold too many
    /// numbers, so it leaves them.
    fn narrow(&mut self, zero: bool) {
        let Contents::Known { values, or_unknown } = self else {
            return;
        };
        values.retain(|value| (value.number == 0) == zero);
        if zero && *or_unknown {
            // There is room: it holds only 0s.
            values.insert(Value::number(0));
            *or_unknown = false;
        }
    }

    /// Adds what `other` may hold, where two paths through the code join.
    /// Returns whether these contents changed. Past [`LIMIT`] numbers they
    /// hold too many, and each number they lose goes to `lost`.
    pub fn join(&mut self, other: &Contents, lost: &mut impl FnMut(&Value)) -> bool {
        let Contents::Known { values, or_unknown } = self else {
            // They gain nothing but whether some numbers were stale.
            other.values().iter().for_each(&mut *lost);
            let changed = other.has_stale() && !self.has_stale();
            if changed {
                *self = Contents::TooMany { stale: true };
            }
            return changed;
        };
        let Contents::Known {
            values: other_values,
            or_unknown: other_unknown,
        } = other
        else {
            values.iter().for_each(&mut *lost);
            *self = Contents::TooMany {
                stale: self.has_stale() || other.has_stale(),
            };
            return true;
        };
        let before = *values;
        let mut changed = false;
        if *other_unknown && !*or_unknown {
            *or_unknown = true;
            changed = true;
        }
        for value in other_values.iter() {
            match values.insert(*value) {
                Inserted::Changed => changed = true,
                Inserted::Unchanged => {}
                Inserted::Full => {
                    before
                        .iter()
                        .chain(other_values.iter())
                        .for_each(&mut *lost);
                    *self = Contents::TooMany {
                        stale: before.iter().any(|value| value.stale) || other.has_stale(),
                    };
                    return true;
                }
            }
        }
        changed
    }

    /// These contents, as code that counts on a call having kept them
    /// reads them: what was stale isn't.
    fn kept(&self) -> Contents {
        match self {
            _ if !self.has_stale() => *self,
            Contents::Known { values, or_unknown } => Contents::Known {
                values: values.with_stale(false),
                or_unknown: *or_unknown,
            },
            Contents::TooMany { .. } => Contents::TooMany { stale: false },
        }
    }

    /// These contents as r0, as code reads it after a call: what the call
    /// returns, which elfq can't tell, in place of the stale numbers.
    fn result(&self) -> Contents {
        match self {
            _ if !self.has_stale() => *self,
            Contents::Known { values, .. } => {
                let mut values = *values;
                values.retain(|value| !value.stale);
                Contents::Known {
                    values,
                    or_unknown: true,
                }
            }
            Contents::TooMany { .. } => Contents::UNKNOWN,
        }
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
        let mut registers: [Contents; 16] = [Contents::UNKNOWN; 16];
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
        let contents = self.registers[usize::from(register)];
        match register {
            PC => Contents::holding(Value {
                number: pc,
                kind: Kind::ProgramCounter,
                source: None,
                stale: false,
            }),
            RESULT => contents.result(),
            _ => contents.kept(),
        }
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
            .unwrap_or(Contents::UNKNOWN)
    }

    /// Sets the word on the stack at `offset`.
    pub fn set_word(&mut self, offset: i32, contents: Contents) {
        if contents == Contents::UNKNOWN {
            self.stack.remove(&offset);
        } else {
            self.stack.insert(offset, contents);
        }
    }

    /// Forgets every word on the stack, giving each number it held to
    /// `lost`.
    pub fn forget_stack(&mut self, lost: &mut impl FnMut(&Value)) {
        for value in self.stack.values().flat_map(|contents| contents.values()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The plain numbers in `numbers`.
    fn values(numbers: impl IntoIterator<Item = u32>) -> Values {
        let mut values = Values::NONE;
        for number in numbers {
            values.insert(Value::number(number));
        }
        values
    }

    /// `values`, and nothing else.
    fn known(values: Values) -> Contents {
        Contents::Known {
            values,
            or_unknown: false,
        }
    }

    /// The plain numbers in `numbers`, and nothing else.
    fn holding(numbers: impl IntoIterator<Item = u32>) -> Contents {
        known(values(numbers))
    }

    /// The numbers `contents` may hold.
    fn numbers(contents: &Contents) -> Vec<u32> {
        contents.values().iter().map(|value| value.number).collect()
    }

    /// `number`, as a register held it before a call that may change it.
    fn stale(number: u32) -> Value {
        Value {
            stale: true,
            ..Value::number(number)
        }
    }

    /// Joins `other` into `contents`, and returns whether `contents`
    /// changed and the numbers it lost, sorted.
    fn join(contents: &mut Contents, other: &Contents) -> (bool, Vec<u32>) {
        let mut lost = Vec::new();
        let changed = contents.join(other, &mut |value| lost.push(value.number));
        lost.sort_unstable();
        (changed, lost)
    }

    /// Contents that may hold more than [`LIMIT`] numbers, `stale` when
    /// some of them were.
    fn too_many(stale: bool) -> Contents {
        Contents::TooMany { stale }
    }

    #[test]
    fn values_hold_at_most_limit_numbers_sorted() {
        let mut full = values((1..=LIMIT as u32).rev());
        let held: Vec<u32> = full.iter().map(|value| value.number).collect();
        assert_eq!(held, [1, 2, 3, 4, 5, 6, 7, 8]);
        let before = full;
        assert_eq!(full.insert(Value::number(9)), Inserted::Full);
        assert_eq!(full, before);
        // A number they hold is no news, even when full.
        assert_eq!(full.insert(Value::number(3)), Inserted::Unchanged);
        assert_eq!(full, before);
    }

    #[test]
    fn a_number_stale_on_one_path_and_not_on_another_is_not_stale() {
        let mut values = Values::NONE;
        assert_eq!(values.insert(stale(5)), Inserted::Changed);
        assert_eq!(values.insert(stale(5)), Inserted::Unchanged);
        assert_eq!(values.insert(Value::number(5)), Inserted::Changed);
        assert!(!values[0].stale);
        // Once it isn't stale, a stale copy doesn't make it stale again.
        assert_eq!(values.insert(stale(5)), Inserted::Unchanged);
        assert!(!values[0].stale);
        assert_eq!(values.len(), 1);
    }

    #[test]
    fn joining_adds_the_numbers_and_what_elfq_cant_tell() {
        let mut contents = holding([1, 2]);
        assert_eq!(join(&mut contents, &holding([2, 3])), (true, vec![]));
        assert_eq!(numbers(&contents), [1, 2, 3]);
        assert!(!contents.may_be_unknown());
        assert_eq!(join(&mut contents, &holding([1])), (false, vec![]));
        assert_eq!(join(&mut contents, &Contents::UNKNOWN), (true, vec![]));
        assert!(contents.may_be_unknown());
        assert_eq!(join(&mut contents, &Contents::UNKNOWN), (false, vec![]));
        assert_eq!(numbers(&contents), [1, 2, 3]);
    }

    #[test]
    fn contents_that_may_hold_more_than_limit_numbers_hold_too_many() {
        let mut contents = holding(1..=7);
        // Both sides' numbers are lost.
        assert_eq!(
            join(&mut contents, &holding([8, 9])),
            (true, (1..=9).collect())
        );
        assert_eq!(contents, too_many(false));
        assert!(contents.may_be_unknown() && contents.values().is_empty());

        // Too many with a stale number on either side: stale.
        let mut contents = holding(1..=7);
        let mut other = values([8]);
        other.insert(stale(9));
        assert!(join(&mut contents, &known(other)).0);
        assert_eq!(contents, too_many(true));
        let mut first = values(2..=7);
        first.insert(stale(1));
        let mut contents = known(first);
        assert!(join(&mut contents, &holding([8, 9])).0);
        assert_eq!(contents, too_many(true));
    }

    #[test]
    fn joining_contents_that_hold_too_many_holds_too_many_and_carries_stale() {
        let mut contents = holding([1, 2]);
        assert_eq!(join(&mut contents, &too_many(false)), (true, vec![1, 2]));
        assert_eq!(contents, too_many(false));

        let mut contents = holding([1]);
        assert_eq!(join(&mut contents, &too_many(true)), (true, vec![1]));
        assert_eq!(contents, too_many(true));

        let mut contents = Contents::holding(stale(1));
        assert_eq!(join(&mut contents, &too_many(false)), (true, vec![1]));
        assert_eq!(contents, too_many(true));
    }

    #[test]
    fn contents_that_hold_too_many_gain_nothing_but_stale() {
        let mut contents = too_many(false);
        // The other side's numbers are lost, and nothing changes.
        assert_eq!(join(&mut contents, &holding([1, 2])), (false, vec![1, 2]));
        assert_eq!(join(&mut contents, &Contents::UNKNOWN), (false, vec![]));
        assert_eq!(join(&mut contents, &too_many(false)), (false, vec![]));
        assert_eq!(contents, too_many(false));
        // A stale number, or stale ones it stopped following, make it stale.
        assert_eq!(
            join(&mut contents, &Contents::holding(stale(3))),
            (true, vec![3])
        );
        assert_eq!(contents, too_many(true));
        assert_eq!(
            join(&mut contents, &Contents::holding(stale(3))),
            (false, vec![3])
        );
        let mut contents = too_many(false);
        assert_eq!(join(&mut contents, &too_many(true)), (true, vec![]));
        assert_eq!(contents, too_many(true));
    }

    #[test]
    fn after_a_call_a_register_holds_what_it_did_stale_or_what_elfq_cant_tell() {
        let after = holding([1, 2]).after_call();
        assert!(after.may_be_unknown());
        assert_eq!(numbers(&after), [1, 2]);
        assert!(after.values().iter().all(|value| value.stale));
        assert!(matches!(after, Contents::Known { .. }));
        // Contents that hold too many keep only that some were stale.
        assert_eq!(too_many(true).after_call(), too_many(true));
        assert_eq!(too_many(false).after_call(), Contents::UNKNOWN);
    }

    #[test]
    fn r0_after_a_call_holds_the_result_and_other_registers_what_they_did() {
        let mut state = State::entry();
        state.registers[0] = holding([0x100]).after_call();
        state.registers[1] = holding([0x200]).after_call();
        // r0 is what the call returns: the stale numbers aren't there.
        assert_eq!(state.get(RESULT, 0), Contents::UNKNOWN);
        // r1 is read only when the call keeps it, so they are, and no
        // longer stale.
        let r1 = state.get(1, 0);
        assert_eq!(numbers(&r1), [0x200]);
        assert!(r1.may_be_unknown() && !r1.values()[0].stale);
        // An r0 that holds too many, some stale, is the result too.
        state.registers[0] = too_many(true);
        assert_eq!(state.get(RESULT, 0), Contents::UNKNOWN);
        state.registers[2] = too_many(true);
        assert_eq!(state.get(2, 0), too_many(false));
        // r0 without stale numbers holds what it held.
        state.registers[0] = holding([7]);
        assert_eq!(state.get(RESULT, 0), holding([7]));
    }

    #[test]
    fn reading_pc_gives_the_instruction_s_pc() {
        let pc = State::entry().get(PC, 0x1004);
        assert_eq!(pc.values().len(), 1);
        assert_eq!(pc.values()[0].number, 0x1004);
        assert_eq!(pc.values()[0].kind, Kind::ProgramCounter);
        assert!(!pc.may_be_unknown());
    }

    #[test]
    fn a_test_for_zero_leaves_the_numbers_it_can_be() {
        let mut state = State::entry();
        state.registers[3] = Contents::UNKNOWN;
        state.narrow(3, true);
        assert_eq!(state.registers[3], holding([0]));

        state.registers[3] = Contents::Known {
            values: values([0, 4]),
            or_unknown: true,
        };
        state.narrow(3, false);
        assert_eq!(numbers(&state.registers[3]), [4]);
        assert!(state.registers[3].may_be_unknown());

        // Nothing left: the code there never runs.
        state.registers[3] = holding([4]);
        state.narrow(3, true);
        assert!(numbers(&state.registers[3]).is_empty() && !state.registers[3].may_be_unknown());

        // elfq doesn't follow contents that hold too many, so a test
        // leaves them.
        state.registers[3] = too_many(false);
        state.narrow(3, true);
        assert_eq!(state.registers[3], too_many(false));
    }

    #[test]
    fn mapping_contents_keeps_what_elfq_cant_tell() {
        let contents = holding([1, 2]).map(|value| {
            (value.number == 1).then_some(Value {
                number: 10,
                ..value
            })
        });
        assert_eq!(numbers(&contents), [10]);
        assert!(contents.may_be_unknown());
        // Contents that may hold something elfq can't tell still may.
        let contents = Contents::Known {
            values: values([1]),
            or_unknown: true,
        }
        .map(Some);
        assert_eq!(numbers(&contents), [1]);
        assert!(contents.may_be_unknown());
        assert_eq!(too_many(true).map(Some), Contents::UNKNOWN);
    }

    #[test]
    fn a_word_on_the_stack_holds_what_elfq_cant_tell_until_code_stores_there() {
        let mut state = State::entry();
        assert_eq!(state.word(-4), Contents::UNKNOWN);
        state.set_word(-4, holding([1]));
        assert_eq!(state.word(-4), holding([1]));
        state.set_word(-4, Contents::UNKNOWN);
        assert!(state.stack.is_empty());
        // Contents that hold too many are kept, as they aren't just what
        // elfq can't tell.
        state.set_word(-8, too_many(false));
        assert_eq!(state.word(-8), too_many(false));
    }

    #[test]
    fn losing_an_address_on_the_stack_lets_it_escape() {
        let mut state = State::entry();
        state.set_word(-4, holding([1]));
        let mut lost = Vec::new();
        state.forget_stack(&mut |value| lost.push(value.number));
        assert_eq!(lost, [1]);
        assert!(state.stack.is_empty() && !state.escaped);

        state.set_word(-4, Contents::holding(Value::stack(-16)));
        state.forget_stack(&mut |_| {});
        assert!(state.escaped);

        // A join that leaves a register holding one with too many escapes
        // too.
        let mut state = State::entry();
        let mut other = State::entry();
        state.registers[4] = Contents::holding(Value::stack(-8));
        other.registers[4] = holding(1..=8);
        let mut lost = Vec::new();
        assert!(state.join(&other, &mut |value| lost.push(*value)));
        assert!(state.escaped);
        assert_eq!(lost.len(), 9);
        assert!(lost.contains(&Value::stack(-8)));
    }

    #[test]
    fn joining_states_joins_each_register_and_word() {
        let mut state = State::entry();
        let mut other = State::entry();
        state.registers[1] = holding([1]);
        other.registers[1] = holding([2]);
        other.set_word(-4, holding([3]));
        assert!(state.join(&other, &mut |_| {}));
        assert_eq!(numbers(&state.registers[1]), [1, 2]);
        // A word that only one side stored holds something elfq can't
        // tell as well.
        let word = state.word(-4);
        assert_eq!(numbers(&word), [3]);
        assert!(word.may_be_unknown());
        assert!(!state.join(&other, &mut |_| {}));
        // A side whose stack escaped makes the join's escape.
        other.escaped = true;
        assert!(state.join(&other, &mut |_| {}));
        assert!(state.escaped);
        assert!(!state.join(&other, &mut |_| {}));
    }
}
