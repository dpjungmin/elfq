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

    /// The plain numbers in `numbers`, and nothing else.
    fn holding(numbers: impl IntoIterator<Item = u32>) -> Contents {
        Contents {
            values: values(numbers),
            ..Contents::holding(Value::number(0))
        }
    }

    /// The numbers `contents` may hold.
    fn numbers(contents: &Contents) -> Vec<u32> {
        contents.values.iter().map(|value| value.number).collect()
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
    fn saturated(stale: bool) -> Contents {
        Contents {
            stale,
            ..Contents::saturated()
        }
    }

    #[test]
    fn values_hold_at_most_limit_numbers_sorted() {
        let mut full = values((1..=LIMIT as u32).rev());
        let held: Vec<u32> = full.iter().map(|value| value.number).collect();
        assert_eq!(held, [1, 2, 3, 4, 5, 6, 7, 8]);
        let before = full;
        assert_eq!(full.insert(Value::number(9)), None);
        assert_eq!(full, before);
        // A number they hold is no news, even when full.
        assert_eq!(full.insert(Value::number(3)), Some(false));
        assert_eq!(full, before);
    }

    #[test]
    fn a_number_stale_on_one_path_and_not_on_another_is_not_stale() {
        let mut values = Values::NONE;
        assert_eq!(values.insert(stale(5)), Some(true));
        assert_eq!(values.insert(stale(5)), Some(false));
        assert_eq!(values.insert(Value::number(5)), Some(true));
        assert!(!values[0].stale);
        // Once it isn't stale, a stale copy doesn't make it stale again.
        assert_eq!(values.insert(stale(5)), Some(false));
        assert!(!values[0].stale);
        assert_eq!(values.len(), 1);
    }

    #[test]
    fn joining_adds_the_numbers_and_what_elfq_cant_tell() {
        let mut contents = holding([1, 2]);
        assert_eq!(join(&mut contents, &holding([2, 3])), (true, vec![]));
        assert_eq!(numbers(&contents), [1, 2, 3]);
        assert!(!contents.unknown);
        assert_eq!(join(&mut contents, &holding([1])), (false, vec![]));
        assert_eq!(join(&mut contents, &Contents::unknown()), (true, vec![]));
        assert!(contents.unknown);
        assert_eq!(join(&mut contents, &Contents::unknown()), (false, vec![]));
        assert_eq!(numbers(&contents), [1, 2, 3]);
    }

    #[test]
    fn contents_that_may_hold_more_than_limit_numbers_are_saturated() {
        let mut contents = holding(1..=7);
        // Both sides' numbers are lost.
        assert_eq!(
            join(&mut contents, &holding([8, 9])),
            (true, (1..=9).collect())
        );
        assert_eq!(contents, saturated(false));
        assert!(contents.unknown && contents.values.is_empty());

        // Saturated with a stale number on either side: stale.
        let mut contents = holding(1..=7);
        let mut other = holding([8]);
        other.values.insert(stale(9));
        assert!(join(&mut contents, &other).0);
        assert_eq!(contents, saturated(true));
        let mut contents = Contents::holding(stale(1));
        for number in 2..=7 {
            contents.values.insert(Value::number(number));
        }
        assert!(join(&mut contents, &holding([8, 9])).0);
        assert_eq!(contents, saturated(true));
    }

    #[test]
    fn joining_saturated_contents_saturates_and_carries_stale() {
        let mut contents = holding([1, 2]);
        assert_eq!(join(&mut contents, &saturated(false)), (true, vec![1, 2]));
        assert_eq!(contents, saturated(false));

        let mut contents = holding([1]);
        assert_eq!(join(&mut contents, &saturated(true)), (true, vec![1]));
        assert_eq!(contents, saturated(true));

        let mut contents = Contents::holding(stale(1));
        assert_eq!(join(&mut contents, &saturated(false)), (true, vec![1]));
        assert_eq!(contents, saturated(true));
    }

    #[test]
    fn saturated_contents_gain_nothing_but_stale() {
        let mut contents = saturated(false);
        // The other side's numbers are lost, and nothing changes.
        assert_eq!(join(&mut contents, &holding([1, 2])), (false, vec![1, 2]));
        assert_eq!(join(&mut contents, &Contents::unknown()), (false, vec![]));
        assert_eq!(join(&mut contents, &saturated(false)), (false, vec![]));
        assert_eq!(contents, saturated(false));
        // A stale number, or stale ones it stopped following, make it stale.
        assert_eq!(
            join(&mut contents, &Contents::holding(stale(3))),
            (true, vec![3])
        );
        assert_eq!(contents, saturated(true));
        assert_eq!(
            join(&mut contents, &Contents::holding(stale(3))),
            (false, vec![3])
        );
        let mut contents = saturated(false);
        assert_eq!(join(&mut contents, &saturated(true)), (true, vec![]));
        assert_eq!(contents, saturated(true));
    }

    #[test]
    fn after_a_call_a_register_holds_what_it_did_stale_or_what_elfq_cant_tell() {
        let after = holding([1, 2]).after_call();
        assert!(after.unknown);
        assert_eq!(numbers(&after), [1, 2]);
        assert!(after.values.iter().all(|value| value.stale));
        assert!(!after.saturated && !after.stale);
        // Contents that are saturated keep only that some were stale.
        assert_eq!(saturated(true).after_call(), saturated(true));
        assert_eq!(saturated(false).after_call(), Contents::unknown());
    }

    #[test]
    fn r0_after_a_call_holds_the_result_and_other_registers_what_they_did() {
        let mut state = State::entry();
        state.registers[0] = holding([0x100]).after_call();
        state.registers[1] = holding([0x200]).after_call();
        // r0 is what the call returns: the stale numbers aren't there.
        assert_eq!(state.get(RESULT, 0), Contents::unknown());
        // r1 is read only when the call keeps it, so they are, and no
        // longer stale.
        let r1 = state.get(1, 0);
        assert_eq!(numbers(&r1), [0x200]);
        assert!(r1.unknown && !r1.values[0].stale);
        // A saturated r0 with stale numbers is the result too.
        state.registers[0] = saturated(true);
        assert_eq!(state.get(RESULT, 0), Contents::unknown());
        state.registers[2] = saturated(true);
        assert_eq!(state.get(2, 0), saturated(false));
        // r0 without stale numbers holds what it held.
        state.registers[0] = holding([7]);
        assert_eq!(state.get(RESULT, 0), holding([7]));
    }

    #[test]
    fn reading_pc_gives_the_instruction_s_pc() {
        let pc = State::entry().get(PC, 0x1004);
        assert_eq!(pc.values.len(), 1);
        assert_eq!(pc.values[0].number, 0x1004);
        assert_eq!(pc.values[0].kind, Kind::ProgramCounter);
        assert!(!pc.unknown);
    }

    #[test]
    fn a_test_for_zero_leaves_the_numbers_it_can_be() {
        let mut state = State::entry();
        state.registers[3] = Contents::unknown();
        state.narrow(3, true);
        assert_eq!(state.registers[3], holding([0]));

        state.registers[3] = Contents {
            unknown: true,
            ..holding([0, 4])
        };
        state.narrow(3, false);
        assert_eq!(numbers(&state.registers[3]), [4]);
        assert!(state.registers[3].unknown);

        // Nothing left: the code there never runs.
        state.registers[3] = holding([4]);
        state.narrow(3, true);
        assert!(numbers(&state.registers[3]).is_empty() && !state.registers[3].unknown);

        // elfq doesn't follow saturated contents, so a test leaves them.
        state.registers[3] = saturated(false);
        state.narrow(3, true);
        assert_eq!(state.registers[3], saturated(false));
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
        assert!(contents.unknown);
        // Contents that may hold something elfq can't tell still may.
        let contents = Contents {
            unknown: true,
            ..holding([1])
        }
        .map(Some);
        assert_eq!(numbers(&contents), [1]);
        assert!(contents.unknown);
        assert_eq!(saturated(true).map(Some), Contents::unknown());
    }

    #[test]
    fn a_word_on_the_stack_holds_what_elfq_cant_tell_until_code_stores_there() {
        let mut state = State::entry();
        assert_eq!(state.word(-4), Contents::unknown());
        state.set_word(-4, holding([1]));
        assert_eq!(state.word(-4), holding([1]));
        state.set_word(-4, Contents::unknown());
        assert!(state.stack.is_empty());
        // Saturated contents are kept, as they aren't just unknown.
        state.set_word(-8, saturated(false));
        assert_eq!(state.word(-8), saturated(false));
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

        // A join that saturates a register holding one escapes too.
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
        assert!(word.unknown);
        assert!(!state.join(&other, &mut |_| {}));
        // A side whose stack escaped makes the join's escape.
        other.escaped = true;
        assert!(state.join(&other, &mut |_| {}));
        assert!(state.escaped);
        assert!(!state.join(&other, &mut |_| {}));
    }
}
