//! The code that `values` follows, laid out once before it follows the
//! registers: what each instruction does, which an `it` block makes
//! conditional, where each branch inside the function goes, and the blocks
//! of code that run from start to end.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use object::{Endian, Endianness};

use crate::arch::CodeReference;
use crate::arch::CodeReferenceKind::Jump;
use crate::arch::thumb::decode::{self, Effect, Flow, Operation, SetsFlags, Transfer, When};
use crate::arch::thumb::{Instruction, PC, SP};

/// Which instructions of an `it` block run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Condition {
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

/// One function's code, as `values` follows it. It doesn't change once
/// built.
pub(super) struct Code<'a> {
    bytes: &'a [u8],
    /// The address of `bytes[0]`.
    address: u64,
    endianness: Endianness,
    /// The instructions, up to the last that the code reaches.
    instructions: &'a [Instruction],
    /// What each instruction does: those the code doesn't reach do nothing
    /// (see [`UNREACHED`]).
    pub(super) effects: Vec<Effect>,
    pub(super) conditions: Vec<Condition>,
    /// The instructions that start a block of code that runs from start to
    /// end: the first one, those that code branches to, and those after a
    /// branch. Indices into `instructions`, sorted.
    pub(super) blocks: Vec<usize>,
    /// For each instruction that branches inside the function, the
    /// instructions it may go to.
    jumps: BTreeMap<usize, Vec<usize>>,
}

impl<'a> Code<'a> {
    /// The code to follow, from the instructions the sweep decoded and the
    /// references it found, or `None` when `values` leaves the function to
    /// the sweep, whose references then stand. That is when:
    ///
    /// - following the registers can't find more than the sweep, as in most
    ///   functions (`may_find_more`, on all the instructions and then on
    ///   those the code reaches);
    /// - the code reaches an instruction that `decode` doesn't know
    ///   (`decode_reached`), which could write any register or branch
    ///   anywhere;
    /// - code that the first instruction doesn't lead to goes on into code
    ///   it does, by running on into it or branching there
    ///   (`enters_reached`), so what the registers hold there would miss
    ///   what that code puts in them;
    /// - an `it` block breaks the rules for one, as data decoded as code may
    ///   (`find_conditions`);
    /// - a branch goes somewhere other than the start of an instruction, or
    ///   into an `it` block (`find_jumps`).
    pub(super) fn new(
        bytes: &'a [u8],
        address: u64,
        endianness: Endianness,
        instructions: &'a [Instruction],
        references: &[CodeReference],
    ) -> Option<Code<'a>> {
        // Most functions show that on their own instructions, which is
        // quicker to find than which of those the code reaches.
        let each = instructions
            .iter()
            .map(|instruction| decode_at(address, instruction));
        if !may_find_more(each) {
            return None;
        }
        let effects = decode_reached(address, instructions, references)?;
        if !may_find_more(effects.iter().copied())
            || enters_reached(address, instructions, &effects)
        {
            return None;
        }
        // The code may run on into code that it never reaches, as the code
        // between two functions that elfq finds in stripped firmware does.
        let instructions = &instructions[..effects.len()];
        let effects = effects
            .into_iter()
            .map(|effect| effect.unwrap_or(UNREACHED))
            .collect();
        let mut code = Code {
            bytes,
            address,
            endianness,
            instructions,
            effects,
            conditions: vec![Condition::Always; instructions.len()],
            blocks: Vec::new(),
            jumps: BTreeMap::new(),
        };
        code.find_conditions()?;
        code.find_jumps(references)?;
        code.find_blocks();
        Some(code)
    }

    /// Works out which instructions each `it` block makes conditional, and
    /// on what. Returns `None` for code that breaks the rules for `it`
    /// blocks.
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
                flags_changed |= effect.sets_flags(true);
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
                Flow::Table => table_targets(references, self.site(index)).collect(),
                _ => continue,
            };
            let mut inside = Vec::new();
            for target in targets.into_iter().filter(|&target| self.contains(target)) {
                let at = index_of(self.address, self.instructions, target)?;
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
    }

    /// How many instructions there are, up to the last the code reaches.
    pub(super) fn len(&self) -> usize {
        self.instructions.len()
    }

    /// The instructions of block `block`, by index.
    pub(super) fn block(&self, block: usize) -> Range<usize> {
        let end = self.blocks.get(block + 1).copied().unwrap_or(self.len());
        self.blocks[block]..end
    }

    /// The block that starts at the instruction `start`.
    pub(super) fn block_at(&self, start: usize) -> usize {
        self.blocks
            .binary_search(&start)
            .expect("code goes only to the start of a block")
    }

    /// The instructions that the branch at `index` may go to inside the
    /// function.
    pub(super) fn jumps(&self, index: usize) -> &[usize] {
        self.jumps.get(&index).map_or(&[], Vec::as_slice)
    }

    /// Whether the instruction at `index` can run on into the next one:
    /// nothing, such as a literal pool, is between them.
    pub(super) fn runs_on(&self, index: usize) -> bool {
        runs_on(self.instructions, index)
    }

    /// The address of the instruction at `index`.
    pub(super) fn site(&self, index: usize) -> u64 {
        site(self.address, &self.instructions[index])
    }

    /// What the instruction at `index` reads in pc: its address plus 4.
    pub(super) fn pc(&self, index: usize) -> u32 {
        (self.site(index) as u32).wrapping_add(4)
    }

    /// Whether `address` is in this function's code.
    pub(super) fn contains(&self, address: u64) -> bool {
        address
            .checked_sub(self.address)
            .is_some_and(|offset| offset < self.bytes.len() as u64)
    }

    /// The word at `address`, if it is in this function's code.
    pub(super) fn word(&self, address: u64) -> Option<u32> {
        let offset = usize::try_from(address.checked_sub(self.address)?).ok()?;
        let bytes: [u8; 4] = self
            .bytes
            .get(offset..offset.checked_add(4)?)?
            .try_into()
            .ok()?;
        Some(self.endianness.read_u32(bytes))
    }

    /// Whether the instruction at `index`, outside an `it` block, may change
    /// the flags.
    pub(super) fn changes_flags(&self, index: usize) -> bool {
        self.effects[index].sets_flags(false)
    }

    /// When the branch at `index`, taken `when`, tests whether a register
    /// is 0: the register, and whether it is 0 where the branch is taken.
    /// Besides `cbz` and `cbnz`, Cortex-M0 code tests with `cmp register,
    /// #0` followed by `beq` or `bne`.
    pub(super) fn zero_test(&self, index: usize, when: When) -> Option<(u16, bool)> {
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
        while let Some(instruction) = instructions.get(index) {
            if effects[index].is_some() {
                break;
            }
            let effect = decode_at(address, instruction)?;
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
                    let targets = table_targets(references, site(address, instruction));
                    to_visit.extend(
                        targets.filter_map(|target| index_of(address, instructions, target)),
                    );
                    false
                }
                _ => false,
            };
            if !runs_on(instructions, index) || !(goes_on || conditional.contains(&index)) {
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
    flags: SetsFlags::Never,
};

/// Whether code that the first instruction doesn't lead to may go on into
/// code it does, as the cases of a `switch` do when elfq can't read the
/// table of addresses that the code jumps through to them:
/// `mov pc, register`, `ldr pc, [...]`, or a call to
/// `__gnu_thumb1_case_uqi` and the like. What the registers hold where they
/// join would then miss what those cases put there. Padding before a loop
/// doesn't count, but code that runs on through its `nop`s does. A jump to
/// the first instruction doesn't count either: it is a call, as from the
/// next function in stripped firmware, which elfq may have taken to be part
/// of this one. `effects` holds `None` for the instructions the code can't
/// reach, up to the last it can.
fn enters_reached(address: u64, instructions: &[Instruction], effects: &[Option<Effect>]) -> bool {
    let reached = |index: usize| effects.get(index).is_some_and(Option::is_some);
    // Whether the code runs on from the instruction at `index` into code
    // the first one leads to, through any `nop`s. Zeros there are the high
    // half of a constant, not padding.
    let runs_into_reached = |mut index: usize| {
        while runs_on(instructions, index) {
            index += 1;
            let next = instructions[index];
            if reached(index) || !next.is_nop() {
                return reached(index);
            }
        }
        false
    };
    (0..instructions.len()).any(|index| {
        let instruction = &instructions[index];
        if reached(index) || instruction.is_padding() {
            return false;
        }
        // Data, as a table of cases may be.
        let Some(effect) = decode_at(address, instruction) else {
            return false;
        };
        let goes_on = match effect.flow {
            Flow::Branch { target, when } => {
                let target = index_of(address, instructions, target);
                if target.is_some_and(|target| target != 0 && reached(target)) {
                    return true;
                }
                when != When::Always
            }
            Flow::Next | Flow::Call | Flow::CallRegister(..) | Flow::It { .. } => true,
            _ => false,
        };
        goes_on && runs_into_reached(index)
    })
}

/// Whether following the registers may find what the sweep didn't: the
/// code branches through a register, adds pc to a register, completes
/// a number with `movt`, or stores a register on the stack and then pops
/// pc, as the stubs that linkers add for Cortex-M0 do. Most functions
/// only call with `bl`, return with `bx lr` or by popping the lr they
/// pushed, and load addresses that the sweep finds as well. `effects`
/// holds the instructions in order, with `None` for those that it leaves
/// out.
fn may_find_more(effects: impl IntoIterator<Item = Option<Effect>>) -> bool {
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
    let stores = |effect: &Effect| {
        matches!(
            effect.operation,
            Operation::Store {
                from: Transfer::Word(_),
                base: SP,
                ..
            }
        )
    };
    let mut stored = false;
    for effect in effects {
        match effect {
            Some(effect) if branches_or_builds(&effect) => return true,
            Some(effect) if stored && effect.flow == Flow::Pop => return true,
            Some(effect) => stored = stores(&effect),
            None => stored = false,
        }
    }
    false
}

/// The index in `instructions` of the one at `target`, if there is one.
fn index_of(address: u64, instructions: &[Instruction], target: u64) -> Option<usize> {
    let offset = usize::try_from(target.checked_sub(address)?).ok()?;
    instructions
        .binary_search_by_key(&offset, |instruction| instruction.offset)
        .ok()
}

/// Whether the instruction at `index` runs on into the next one: nothing,
/// such as a literal pool, is between them.
fn runs_on(instructions: &[Instruction], index: usize) -> bool {
    instructions
        .get(index + 1)
        .is_some_and(|next| next.offset == instructions[index].end())
}

/// The address of `instruction`, in code that starts at `address`.
fn site(address: u64, instruction: &Instruction) -> u64 {
    address + instruction.offset as u64
}

/// What `instruction`, in code that starts at `address`, does, or `None`
/// when `decode` doesn't know it.
fn decode_at(address: u64, instruction: &Instruction) -> Option<Effect> {
    decode::decode(
        site(address, instruction),
        instruction.first,
        instruction.second,
    )
}

/// Where the `tbb` or `tbh` at `site` may jump, as the sweep found in its
/// table.
fn table_targets(references: &[CodeReference], site: u64) -> impl Iterator<Item = u64> + '_ {
    references
        .iter()
        .filter(move |reference| reference.site == site)
        .filter_map(|reference| match reference.kind {
            Jump(target) => Some(target),
            _ => None,
        })
}
