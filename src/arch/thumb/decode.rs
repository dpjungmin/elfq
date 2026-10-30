//! What each Thumb instruction does with the registers: which it reads,
//! which it writes, where the processor goes next, and, for the few that put
//! a number elfq can follow into a register, what number. The `values`
//! module uses this to follow the addresses that code puts in a register
//! and then branches through, and `startup` to follow the code that runs
//! from reset, with the numbers it passes to the functions it calls.
//!
//! The encodings are those of the Armv7-M and Armv8-M architecture reference
//! manuals: sections A5.2 (16-bit) and A5.3 (32-bit) in Armv7-M. An
//! instruction this module doesn't know decodes to `None`, and `values`
//! then follows nothing in its function, since such an instruction could
//! write any register or branch anywhere.

use super::{LR, PC, SP, align, branch, is_wide, relative};
use crate::arch::CodeReferenceKind;

/// A set of registers, one bit each: r0 is bit 0 and pc bit 15.
pub(super) type Registers = u16;

/// One instruction's effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Effect {
    /// The registers it reads.
    pub reads: Registers,
    /// The registers it writes. Unless `operation` says what number goes
    /// into one, it holds something elfq can't follow.
    pub writes: Registers,
    /// What it does with the numbers in the registers, when elfq can follow
    /// it.
    pub operation: Operation,
    /// Where the processor goes after it.
    pub flow: Flow,
    /// Whether it changes the condition flags inside an `it` block. The
    /// 16-bit instructions that set the flags elsewhere, such as `adds`,
    /// don't inside one.
    pub sets_flags: bool,
    /// For a load or store that adds an immediate to its base register, as
    /// `ldr.w r3, [r5], #4`, `str.w r3, [r2, #4]!` and `ldmia r1!, {r3,
    /// r4}` do, what it adds and when. Its `Load` or `Store` gives no
    /// `offset`.
    pub writeback: Option<Writeback>,
    /// For a load or store whose address adds a register to its base
    /// register, as `ldr r3, [r2, r1]` and `str.w r3, [r0, r1, lsl #2]`
    /// do, that register. Its `Load` or `Store` gives no `offset`.
    pub index: Option<Index>,
    /// For a load or store of more than one word, `ldm`, `stm`, `ldrd` and
    /// `strd`, the registers it loads or stores, and where. Its `Load` or
    /// `Store` names no `register`.
    pub words: Option<Words>,
}

/// What a load or store adds to its base register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Writeback {
    pub amount: i32,
    /// Whether it adds it before it loads or stores, as `[base, #imm]!`
    /// does, or after, as `[base], #imm` does.
    pub before: bool,
}

/// A register that a load or store adds to its base register, shifted
/// left by `shift` bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Index {
    pub register: u16,
    pub shift: u32,
}

/// The registers of a load or store of more than one word, a word each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Words {
    /// `ldm` and `stm`: the registers in the list, the lowest at the lowest
    /// address. The words start at the base, or with `down`, as for
    /// `ldmdb` and `stmdb`, end there.
    List { registers: Registers, down: bool },
    /// `ldrd` and `strd`: the word of `first` at `offset` from the base, as
    /// the base is before any writeback, and then the word of `second`.
    Pair {
        first: u16,
        second: u16,
        offset: i32,
    },
}

/// What an instruction does with the numbers in the registers, as far as
/// elfq follows: mostly, the number it puts into one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Operation {
    /// Nothing elfq follows.
    Nothing,
    /// `register` = `value`: `movs`, `mov.w` or `movw` with an immediate.
    Constant { register: u16, value: u32 },
    /// `register` = the word at `address` in a literal pool: `ldr register,
    /// [pc, #imm]`.
    Literal { register: u16, address: u64 },
    /// `register` = an address in the code: `adr register, label`.
    Address { register: u16, value: u32 },
    /// `register` = its low 16 bits and `high` above them: `movt`.
    HighHalf { register: u16, high: u32 },
    /// `register` = `source`: `mov`. A `source` of pc is the instruction's
    /// address + 4.
    Copy { register: u16, source: u16 },
    /// `register` = `source` + `amount`, wrapping: `adds`, `subs`, `addw`,
    /// `subw`, `add.w` and `sub.w` with an immediate.
    AddConstant {
        register: u16,
        source: u16,
        amount: u32,
    },
    /// `register` = `a` + `b`: `add` with registers. Either can be pc.
    Add { register: u16, a: u16, b: u16 },
    /// `register` = `a` - `b`, wrapping: `subs` and `sub.w` with registers.
    Subtract { register: u16, a: u16, b: u16 },
    /// `register` = `a` + (`b` << `shift`), or with `subtract`, `a` - (`b`
    /// << `shift`), wrapping: `add.w` and `sub.w` with a register shifted
    /// left, as code that works out where a run of words ends does.
    AddShifted {
        register: u16,
        a: u16,
        b: u16,
        shift: u32,
        subtract: bool,
    },
    /// `register` = 0 - `source`, wrapping: `negs`.
    Negate { register: u16, source: u16 },
    /// `register` = `source` & `mask`: `and` with an immediate, and `bic`,
    /// whose mask is the immediate's complement.
    AndConstant {
        register: u16,
        source: u16,
        mask: u32,
    },
    /// `register` = `a` & `b`, or with `complement`, `a` & !`b`: `ands`,
    /// `and.w`, `bics` and `bic.w` with registers, as Cortex-M0 code, which
    /// has no `bic` with an immediate, rounds a length to words.
    And {
        register: u16,
        a: u16,
        b: u16,
        complement: bool,
    },
    /// `register` = `source` << `shift`: `lsls` with an immediate.
    ShiftLeft {
        register: u16,
        source: u16,
        shift: u32,
    },
    /// `register` = `source` >> `shift`, from 1 to 32: `lsrs`, `lsr.w`,
    /// `asrs` and `asr.w` with an immediate. `signed` for `asr`, which
    /// shifts in copies of the top bit, where `lsr` shifts in zeros.
    ShiftRight {
        register: u16,
        source: u16,
        shift: u32,
        signed: bool,
    },
    /// Loads from `base` + `offset`, or when `offset` is `None`, from where
    /// the effect's `writeback`, `index` or `words` say, when elfq can
    /// tell: `ldr`, `ldrb`, `ldrh`, `ldrd`, `ldm`, the floating-point loads
    /// and others. A load of a word into a register names the `register`.
    Load {
        register: Option<u16>,
        base: u16,
        offset: Option<i32>,
    },
    /// Stores `size` bytes at `base` + `offset`, or when `offset` is
    /// `None`, where the effect's `writeback`, `index` or `words` say, when
    /// elfq can tell: `str`, `strb`, `strh`, `strd`, `stm`, the
    /// floating-point stores and others. A store of a word from a register
    /// names the `register`.
    Store {
        register: Option<u16>,
        base: u16,
        offset: Option<i32>,
        size: u32,
    },
    /// Lowers sp by `size` and stores `registers` there, the lowest
    /// register at the lowest address: `push`, and `vpush`, whose
    /// floating-point registers aren't in `registers`.
    Push { registers: Registers, size: u32 },
    /// Loads `registers` from sp, the lowest from the lowest address, and
    /// raises sp by `size`: `pop` and `vpop`.
    Pop { registers: Registers, size: u32 },
    /// Compares `register` with `value`, setting the flags: `cmp` with an
    /// immediate. Nothing changes in the registers.
    Compare { register: u16, value: u32 },
    /// Compares `a` with `b`, setting the flags as `a` - `b` does: `cmp`
    /// and `cmp.w` with registers. Nothing changes in the registers.
    CompareRegisters { a: u16, b: u16 },
}

/// When a branch goes to its target, rather than on to the next
/// instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum When {
    Always,
    /// When the flags pass the test `condition`, the field of `b<cond>`:
    /// 0 for `beq`, 1 for `bne` and so on.
    Flags(u16),
    /// When the register is 0: `cbz`.
    Zero(u16),
    /// When it isn't: `cbnz`.
    NotZero(u16),
}

/// Where the processor goes after an instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Flow {
    /// On to the next instruction.
    Next,
    /// A call that returns to the next instruction: `bl`, or `svc`.
    /// What a call does to the registers is up to the function it calls;
    /// `values` assumes it follows the procedure call standard.
    Call,
    /// A call through a register: `blx register`.
    CallRegister(u16, LowBit),
    /// A branch to `target`, or on to the next instruction when it isn't
    /// taken: `b`, `b<cond>`, `cbz`, `cbnz`, `b.w` and `b<cond>.w`.
    Branch { target: u64, when: When },
    /// A jump through a register: `bx register` and `mov pc, register`.
    JumpRegister(u16, LowBit),
    /// A jump `register` bytes past the program counter: `add pc,
    /// register`.
    JumpRelative(u16),
    /// A return: `bx lr` and `mov pc, lr`.
    Return,
    /// A load into pc from the stack: `pop {..., pc}`, `ldm sp!, {...,
    /// pc}` or `ldr pc, [sp], #4`. It returns, unless the word it loads is
    /// an address that code stored there.
    Pop,
    /// A jump to an address loaded from memory other than the stack:
    /// `ldr pc, [...]`, or `ldr pc, [pc, #imm]` as in a veneer.
    JumpMemory,
    /// A jump to a case of the jump table after it: `tbb` and `tbh`.
    Table,
    /// Makes the next instructions conditional: `it`. `firstcond` and
    /// `mask` are its fields.
    It { firstcond: u16, mask: u16 },
    /// Never goes on: `udf`.
    Stop,
}

/// What the low bit of the number in the register does, for a branch
/// through one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LowBit {
    /// It says which instruction set the code is in: 1 for Thumb, the only
    /// one that Cortex-M cores run, so they fault on 0. `bx` and `blx`.
    Thumb,
    /// 1 for Secure Thumb code, and 0 for Non-secure code: Armv8-M's `bxns`
    /// and `blxns`.
    Security,
    /// The branch ignores it: `mov pc, register`.
    Ignored,
}

impl Effect {
    fn new(reads: Registers, writes: Registers) -> Effect {
        Effect {
            reads,
            writes,
            operation: Operation::Nothing,
            flow: Flow::Next,
            sets_flags: false,
            writeback: None,
            index: None,
            words: None,
        }
    }

    fn operation(mut self, operation: Operation) -> Effect {
        self.operation = operation;
        self
    }

    fn flow(mut self, flow: Flow) -> Effect {
        self.flow = flow;
        self
    }

    fn sets_flags(mut self, sets_flags: bool) -> Effect {
        self.sets_flags = sets_flags;
        self
    }

    fn writeback(mut self, writeback: Option<Writeback>) -> Effect {
        self.writeback = writeback;
        self
    }

    fn index(mut self, index: Option<Index>) -> Effect {
        self.index = index;
        self
    }

    fn words(mut self, words: Words) -> Effect {
        self.words = Some(words);
        self
    }
}

/// `register` as a set.
fn bit(register: u16) -> Registers {
    1 << register
}

/// `register` = `source` >> `shift`, as `lsr` does, or `asr` when `signed`.
/// A `shift` of 0, as the encodings give it, means 32.
fn shift_right(register: u16, source: u16, shift: u32, signed: bool) -> Operation {
    Operation::ShiftRight {
        register,
        source,
        shift: if shift == 0 { 32 } else { shift },
        signed,
    }
}

/// The instruction made of `first` and, when it is 32 bits long, `second`,
/// at `address`. `None` when this module doesn't know it.
pub(super) fn decode(address: u64, first: u16, second: u16) -> Option<Effect> {
    if is_wide(first) {
        wide(address, first, second)
    } else {
        narrow(address, first)
    }
}

/// A 16-bit instruction.
fn narrow(address: u64, op: u16) -> Option<Effect> {
    let pc = address + 4;
    // The register fields most 16-bit instructions use.
    let low = op & 7;
    let middle = (op >> 3) & 7;
    let top = (op >> 6) & 7;
    let high = (op >> 8) & 7;
    let effect = match op >> 11 {
        // lsls, lsrs and asrs with an immediate; lsls #0 is movs register.
        0b00000..=0b00010 => {
            let effect = Effect::new(bit(middle), bit(low));
            let shift = u32::from((op >> 6) & 0x1f);
            if op >> 11 == 0 {
                effect.operation(Operation::ShiftLeft {
                    register: low,
                    source: middle,
                    shift,
                })
            } else {
                effect.operation(shift_right(low, middle, shift, op >> 11 == 0b00010))
            }
        }
        // adds and subs, with a register or a 3-bit immediate.
        0b00011 => {
            let effect = Effect::new(bit(middle), bit(low));
            match (op >> 9) & 3 {
                0b00 => Effect::new(bit(middle) | bit(top), bit(low)).operation(Operation::Add {
                    register: low,
                    a: middle,
                    b: top,
                }),
                0b01 => {
                    Effect::new(bit(middle) | bit(top), bit(low)).operation(Operation::Subtract {
                        register: low,
                        a: middle,
                        b: top,
                    })
                }
                0b10 => effect.operation(Operation::AddConstant {
                    register: low,
                    source: middle,
                    amount: u32::from(top),
                }),
                _ => effect.operation(Operation::AddConstant {
                    register: low,
                    source: middle,
                    amount: u32::from(top).wrapping_neg(),
                }),
            }
        }
        // movs register, #imm8
        0b00100 => Effect::new(0, bit(high)).operation(Operation::Constant {
            register: high,
            value: u32::from(op & 0xff),
        }),
        // cmp register, #imm8
        0b00101 => Effect::new(bit(high), 0)
            .operation(Operation::Compare {
                register: high,
                value: u32::from(op & 0xff),
            })
            .sets_flags(true),
        // adds and subs register, #imm8
        0b00110 | 0b00111 => {
            let amount = u32::from(op & 0xff);
            let amount = if op >> 11 == 0b00110 {
                amount
            } else {
                amount.wrapping_neg()
            };
            Effect::new(bit(high), bit(high)).operation(Operation::AddConstant {
                register: high,
                source: high,
                amount,
            })
        }
        0b01000 => match (op >> 10) & 1 {
            0 => data_processing(op),
            _ => special(op),
        },
        // ldr register, [pc, #imm]
        0b01001 => Effect::new(0, bit(high)).operation(Operation::Literal {
            register: high,
            address: align(pc) + u64::from(op & 0xff) * 4,
        }),
        // Loads and stores with a register offset: str, strh, strb, then
        // ldrsb, ldr, ldrh, ldrb and ldrsh.
        0b01010 | 0b01011 => {
            let address = bit(middle) | bit(top);
            let index = Some(Index {
                register: top,
                shift: 0,
            });
            let effect = match (op >> 9) & 7 {
                kind @ 0..=2 => Effect::new(address | bit(low), 0).operation(Operation::Store {
                    register: (kind == 0).then_some(low),
                    base: middle,
                    offset: None,
                    size: [4, 2, 1][usize::from(kind)],
                }),
                kind => Effect::new(address, bit(low)).operation(Operation::Load {
                    register: (kind == 4).then_some(low),
                    base: middle,
                    offset: None,
                }),
            };
            effect.index(index)
        }
        // Loads and stores with an immediate offset: str, ldr, strb, ldrb,
        // strh and ldrh. The immediate counts units of their size.
        0b01100..=0b10001 => {
            let size = match op >> 12 {
                0b0110 => 4,
                0b0111 => 1,
                _ => 2,
            };
            let offset = i32::from((op >> 6) & 0x1f) * size;
            if (op >> 11) & 1 == 0 {
                Effect::new(bit(middle) | bit(low), 0).operation(Operation::Store {
                    register: (size == 4).then_some(low),
                    base: middle,
                    offset: Some(offset),
                    size: size as u32,
                })
            } else {
                Effect::new(bit(middle), bit(low)).operation(Operation::Load {
                    register: (size == 4).then_some(low),
                    base: middle,
                    offset: Some(offset),
                })
            }
        }
        // str register, [sp, #imm]
        0b10010 => Effect::new(bit(high) | bit(SP), 0).operation(Operation::Store {
            register: Some(high),
            base: SP,
            offset: Some(i32::from(op & 0xff) * 4),
            size: 4,
        }),
        // ldr register, [sp, #imm]
        0b10011 => Effect::new(bit(SP), bit(high)).operation(Operation::Load {
            register: Some(high),
            base: SP,
            offset: Some(i32::from(op & 0xff) * 4),
        }),
        // adr register, label
        0b10100 => Effect::new(0, bit(high)).operation(Operation::Address {
            register: high,
            value: (align(pc) + u64::from(op & 0xff) * 4) as u32,
        }),
        // add register, sp, #imm
        0b10101 => Effect::new(bit(SP), bit(high)).operation(Operation::AddConstant {
            register: high,
            source: SP,
            amount: u32::from(op & 0xff) * 4,
        }),
        0b10110 | 0b10111 => return miscellaneous(address, op),
        // stmia register!, {registers}
        0b11000 => {
            let size = u32::from(op & 0xff).count_ones() * 4;
            Effect::new(bit(high) | op & 0xff, bit(high))
                .operation(Operation::Store {
                    register: None,
                    base: high,
                    offset: None,
                    size,
                })
                .words(Words::List {
                    registers: op & 0xff,
                    down: false,
                })
                .writeback(Some(Writeback {
                    amount: size as i32,
                    before: false,
                }))
        }
        // ldmia register!, {registers}; the register is written back
        // unless it is in the list.
        0b11001 => {
            let size = u32::from(op & 0xff).count_ones() * 4;
            let writeback = (op & bit(high) == 0).then_some(Writeback {
                amount: size as i32,
                before: false,
            });
            Effect::new(bit(high), op & 0xff | bit(high))
                .operation(Operation::Load {
                    register: None,
                    base: high,
                    offset: None,
                })
                .words(Words::List {
                    registers: op & 0xff,
                    down: false,
                })
                .writeback(writeback)
        }
        0b11010 | 0b11011 => match (op >> 8) & 0xf {
            // udf
            0b1110 => Effect::new(0, 0).flow(Flow::Stop),
            // svc, which returns like a call.
            0b1111 => Effect::new(0, 0).flow(Flow::Call),
            // b<cond> label
            condition => Effect::new(0, 0).flow(Flow::Branch {
                target: relative(pc, u32::from(op & 0xff) << 1, 9),
                when: When::Flags(condition),
            }),
        },
        // b label
        0b11100 => Effect::new(0, 0).flow(Flow::Branch {
            target: relative(pc, u32::from(op & 0x7ff) << 1, 12),
            when: When::Always,
        }),
        _ => return None,
    };
    Some(effect)
}

/// The 16-bit data-processing instructions, which work on the low
/// registers: `ands`, `eors`, `lsls`, `lsrs`, `asrs`, `adcs`, `sbcs`,
/// `rors`, `tst`, `rsbs`, `cmp`, `cmn`, `orrs`, `muls`, `bics` and `mvns`.
fn data_processing(op: u16) -> Effect {
    let register = op & 7;
    let source = (op >> 3) & 7;
    let reads = bit(register) | bit(source);
    match (op >> 6) & 0xf {
        // ands and bics
        kind @ (0b0000 | 0b1110) => Effect::new(reads, bit(register)).operation(Operation::And {
            register,
            a: register,
            b: source,
            complement: kind == 0b1110,
        }),
        // cmp
        0b1010 => Effect::new(reads, 0)
            .operation(Operation::CompareRegisters {
                a: register,
                b: source,
            })
            .sets_flags(true),
        // tst and cmn only set the flags.
        0b1000 | 0b1011 => Effect::new(reads, 0).sets_flags(true),
        0b1001 => {
            Effect::new(reads, bit(register)).operation(Operation::Negate { register, source })
        }
        _ => Effect::new(reads, bit(register)),
    }
}

/// The 16-bit instructions that work on any register, and branch through
/// one: `add`, `cmp`, `mov`, `bx` and `blx`.
fn special(op: u16) -> Effect {
    let register = (op >> 4) & 8 | op & 7;
    let source = (op >> 3) & 0xf;
    match (op >> 8) & 3 {
        // add register, source: into pc it jumps.
        0b00 if register == PC => Effect::new(bit(source), 0).flow(Flow::JumpRelative(source)),
        0b00 => Effect::new(bit(register) | bit(source), bit(register)).operation(Operation::Add {
            register,
            a: register,
            b: source,
        }),
        0b01 => Effect::new(bit(register) | bit(source), 0)
            .operation(Operation::CompareRegisters {
                a: register,
                b: source,
            })
            .sets_flags(true),
        // mov register, source: into pc it jumps.
        0b10 if register == PC => match source {
            LR => Effect::new(bit(LR), 0).flow(Flow::Return),
            _ => Effect::new(bit(source), 0).flow(Flow::JumpRegister(source, LowBit::Ignored)),
        },
        0b10 => {
            Effect::new(bit(source), bit(register)).operation(Operation::Copy { register, source })
        }
        // bx and blx, and Armv8-M's bxns and blxns, which set bit 2.
        _ => {
            let low_bit = match op & 4 {
                0 => LowBit::Thumb,
                _ => LowBit::Security,
            };
            match op & 0x80 {
                0 if source == LR => Effect::new(bit(LR), 0).flow(Flow::Return),
                0 => Effect::new(bit(source), 0).flow(Flow::JumpRegister(source, low_bit)),
                _ => Effect::new(bit(source), bit(LR)).flow(Flow::CallRegister(source, low_bit)),
            }
        }
    }
}

/// The 16-bit instructions that start with 0b1011.
fn miscellaneous(address: u64, op: u16) -> Option<Effect> {
    let list = op & 0xff;
    let effect = match (op >> 8) & 0xf {
        // add sp, #imm and sub sp, #imm
        0b0000 => {
            let amount = u32::from(op & 0x7f) * 4;
            Effect::new(bit(SP), bit(SP)).operation(Operation::AddConstant {
                register: SP,
                source: SP,
                amount: match op & 0x80 {
                    0 => amount,
                    _ => amount.wrapping_neg(),
                },
            })
        }
        // cbz and cbnz
        0b0001 | 0b0011 | 0b1001 | 0b1011 => {
            let distance = u64::from((op >> 9) & 1) << 6 | u64::from((op >> 3) & 0x1f) << 1;
            let register = op & 7;
            Effect::new(bit(register), 0).flow(Flow::Branch {
                target: address + 4 + distance,
                when: match op & 0x800 {
                    0 => When::Zero(register),
                    _ => When::NotZero(register),
                },
            })
        }
        // sxth, sxtb, uxth and uxtb
        0b0010 => Effect::new(bit((op >> 3) & 7), bit(op & 7)),
        // push {registers}, with lr when bit 8 is set
        0b0100 | 0b0101 => {
            let registers = list | (op & 0x100) << 6;
            Effect::new(registers | bit(SP), bit(SP)).operation(Operation::Push {
                registers,
                size: registers.count_ones() * 4,
            })
        }
        // cpsie and cpsid
        0b0110 if op & 0xffe8 == 0xb660 => Effect::new(0, 0),
        // rev, rev16 and revsh
        0b1010 if (op >> 6) & 3 != 0b10 => Effect::new(bit((op >> 3) & 7), bit(op & 7)),
        // pop {registers}, with pc when bit 8 is set
        0b1100 | 0b1101 => {
            let registers = list | (op & 0x100) << 7;
            let effect = Effect::new(bit(SP), list | bit(SP)).operation(Operation::Pop {
                registers,
                size: registers.count_ones() * 4,
            });
            match registers & bit(PC) {
                0 => effect,
                _ => effect.flow(Flow::Pop),
            }
        }
        // bkpt
        0b1110 => Effect::new(0, 0),
        // it, or a hint such as nop when the mask is 0.
        0b1111 if op & 0xf != 0 => Effect::new(0, 0).flow(Flow::It {
            firstcond: (op >> 4) & 0xf,
            mask: op & 0xf,
        }),
        0b1111 => Effect::new(0, 0),
        _ => return None,
    };
    Some(effect)
}

/// A 32-bit instruction, made of `first` and `second`.
fn wide(address: u64, first: u16, second: u16) -> Option<Effect> {
    let pc = address + 4;
    let rn = first & 0xf;
    let rt = second >> 12;
    let rd = (second >> 8) & 0xf;
    let rm = second & 0xf;
    match first >> 11 {
        0b11101 => match (first >> 9) & 3 {
            0b00 if first & 0x40 == 0 => load_store_multiple(first, second),
            0b00 => load_store_dual(first, second),
            0b01 => shifted_register(first, second),
            _ => coprocessor(first, second),
        },
        0b11110 if second & 0x8000 != 0 => branch_or_control(pc, first, second),
        0b11110 if first & 0x200 == 0 => modified_immediate(first, second),
        0b11110 => plain_immediate(pc, first, second),
        _ => match (first >> 8) & 7 {
            // Stores of a byte, halfword or word.
            0b000 if first & 0x10 == 0 => store(first, second),
            // Loads of a byte, halfword or word.
            0b000 | 0b001 => load(pc, first, second),
            // lsl.w, lsr.w, asr.w and ror.w with registers, sign and zero
            // extension, parallel additions, rev.w, rbit, clz and others.
            0b010 => {
                if rd == PC || rd == SP {
                    return None;
                }
                let shifts = first & 0x80 == 0 && second & 0xf0 == 0;
                Some(Effect::new(reads(&[rn, rm]), bit(rd)).sets_flags(shifts && first & 0x10 != 0))
            }
            // mul, mla, mls and the multiplies of the DSP extension.
            0b011 if first & 0x80 == 0 => {
                if rd == PC || rd == SP {
                    return None;
                }
                Some(Effect::new(reads(&[rn, rm, rt]), bit(rd)))
            }
            // smull, umull, smlal, umlal and the like, and sdiv and udiv.
            0b011 => {
                let divides = second & 0xf0 == 0xf0;
                let writes = if divides { bit(rd) } else { bit(rd) | bit(rt) };
                if writes & (bit(PC) | bit(SP)) != 0 {
                    return None;
                }
                Some(Effect::new(reads(&[rn, rm, rt, rd]), writes))
            }
            _ => coprocessor(first, second),
        },
    }
}

/// The registers in `registers` other than pc (which these encodings use
/// for "none"), as a set.
fn reads(registers: &[u16]) -> Registers {
    registers
        .iter()
        .filter(|&&register| register != PC)
        .fold(0, |set, &register| set | bit(register))
}

/// `ldm`, `ldmdb`, `stm` and `stmdb` (`push.w` and `pop.w`), and
/// Armv8.1-M's `clrm`.
fn load_store_multiple(first: u16, second: u16) -> Option<Effect> {
    // clrm {registers}, which zeroes them, and the flags with bit 15. It
    // takes the place of an `ldm` from pc, which Armv7-M doesn't allow.
    if first == 0xe89f {
        return Some(Effect::new(0, second & !bit(PC)).sets_flags(second & bit(PC) != 0));
    }
    let rn = first & 0xf;
    let writeback = if first & 0x20 != 0 { bit(rn) } else { 0 };
    let increments = match (first >> 7) & 3 {
        0b01 => true,
        0b10 => false,
        // srs and rfe, which M-profile processors don't have.
        _ => return None,
    };
    let size = second.count_ones() * 4;
    let stack = rn == SP && writeback != 0;
    let words = Words::List {
        registers: second,
        down: !increments,
    };
    // The base moves up past the words, or down to the first of them,
    // unless it is one of them.
    let amount = if increments {
        size
    } else {
        size.wrapping_neg()
    };
    let moves = (writeback != 0 && second & bit(rn) == 0).then_some(Writeback {
        amount: amount as i32,
        before: !increments,
    });
    if first & 0x10 == 0 {
        let effect = Effect::new(bit(rn) | second, writeback);
        // stmdb sp!, {registers} is push.w.
        return Some(match stack && !increments {
            true => effect.operation(Operation::Push {
                registers: second,
                size,
            }),
            false => effect
                .operation(Operation::Store {
                    register: None,
                    base: rn,
                    offset: None,
                    size,
                })
                .words(words)
                .writeback(moves),
        });
    }
    let effect = Effect::new(bit(rn), (second & !bit(PC)) | writeback);
    // ldmia sp!, {registers} is pop.w.
    let effect = match stack && increments {
        true => effect.operation(Operation::Pop {
            registers: second,
            size,
        }),
        false => effect
            .operation(Operation::Load {
                register: None,
                base: rn,
                offset: None,
            })
            .words(words)
            .writeback(moves),
    };
    if second & bit(PC) == 0 {
        return Some(effect);
    }
    // Into pc it jumps.
    Some(match effect.operation {
        Operation::Pop { .. } => effect.flow(Flow::Pop),
        _ => effect.flow(Flow::JumpMemory),
    })
}

/// `ldrd`, `strd`, the exclusive loads and stores, `tbb` and `tbh`, and
/// Armv8-M's `sg`, `tt` and loads and stores with acquire and release.
fn load_store_dual(first: u16, second: u16) -> Option<Effect> {
    if first == 0xe97f && second == 0xe97f {
        // sg
        return Some(Effect::new(0, 0));
    }
    let rn = first & 0xf;
    let rt = second >> 12;
    let rt2 = (second >> 8) & 0xf;
    let low = second & 0xf;
    let writeback = if first & 0x20 != 0 { bit(rn) } else { 0 };
    let store = |offset, size| Operation::Store {
        register: None,
        base: rn,
        offset,
        size,
    };
    let load = |register| Operation::Load {
        register,
        base: rn,
        offset: None,
    };
    let effect = match ((first >> 7) & 3, (first >> 4) & 3) {
        // tt, which has rt = pc.
        (0b00, 0b00) if rt == PC => Effect::new(bit(rn), bit(rt2)),
        // strex
        (0b00, 0b00) => Effect::new(reads(&[rn, rt]), bit(rt2)).operation(store(None, 4)),
        // ldrex
        (0b00, 0b01) => Effect::new(bit(rn), bit(rt)).operation(load(Some(rt))),
        // tbb and tbh
        (0b01, 0b01) if (second >> 4) & 0xf <= 1 => {
            Effect::new(reads(&[rn, low]), 0).flow(Flow::Table)
        }
        // ldrexb, ldrexh, lda, ldab, ldah, ldaex, ldaexb, ldaexh and, with
        // two registers, ldaexd.
        (0b01, 0b01) => {
            let pair = if (second >> 4) & 0xf == 0xf {
                bit(rt2)
            } else {
                0
            };
            Effect::new(bit(rn), bit(rt) | pair).operation(load(None))
        }
        // strexb, strexh, stlb, stlh, stl, stlexb, stlexh and stlex. The
        // exclusive ones write their status to the low register.
        (0b01, 0b00) => {
            let op = (second >> 4) & 0xf;
            let status = match op {
                0b0100 | 0b0101 | 0b1100 | 0b1101 | 0b1110 => bit(low),
                _ => 0,
            };
            let size = match op & 3 {
                0b00 => 1,
                0b01 => 2,
                _ => 4,
            };
            Effect::new(reads(&[rn, rt]), status).operation(store(None, size))
        }
        // strd and ldrd, which add the immediate to the base first, and
        // keep the sum with writeback, or add it after.
        _ => {
            let before = first & 0x100 != 0;
            let imm = i32::from(second & 0xff) * 4;
            let amount = if first & 0x80 != 0 { imm } else { -imm };
            let moves =
                (writeback != 0 && rn != rt && rn != rt2).then_some(Writeback { amount, before });
            let words = Words::Pair {
                first: rt,
                second: rt2,
                offset: if before { amount } else { 0 },
            };
            if first & 0x10 == 0 {
                // strd, at an offset when it doesn't write back.
                let offset = (before && writeback == 0).then_some(amount);
                Effect::new(reads(&[rn, rt, rt2]), writeback)
                    .operation(store(offset, 8))
                    .words(words)
                    .writeback(moves)
            } else {
                Effect::new(reads(&[rn]), bit(rt) | bit(rt2) | writeback)
                    .operation(load(None))
                    .words(words)
                    .writeback(moves)
            }
        }
    };
    if effect.writes & bit(PC) != 0 {
        return None;
    }
    Some(effect)
}

/// The data-processing instructions with a shifted register: `and.w`,
/// `bic.w`, `orr.w`, `mov.w`, `orn`, `mvn.w`, `eor.w`, `pkhbt`, `add.w`,
/// `adc.w`, `sbc.w`, `sub.w`, `rsb` and the shifts by an immediate, and
/// `tst.w`, `teq`, `cmn.w` and `cmp.w`, which only set the flags.
fn shifted_register(first: u16, second: u16) -> Option<Effect> {
    let op = (first >> 5) & 0xf;
    let s = first & 0x10 != 0;
    let rn = first & 0xf;
    let rd = (second >> 8) & 0xf;
    let rm = second & 0xf;
    let shift = u32::from((second >> 12) & 7) << 2 | u32::from((second >> 6) & 3);
    let kind = (second >> 4) & 3;
    if !matches!(
        op,
        0b0000..=0b0100 | 0b0110 | 0b1000 | 0b1010 | 0b1011 | 0b1101 | 0b1110
    ) {
        return None;
    }
    // Armv8.1-M puts its conditional selects, such as `cset`, and its
    // shifts of a register pair here, where Armv7-M leaves the top bit 0
    // and doesn't allow these registers.
    if second & 0x8000 != 0 || rm == PC || (s && rm == SP) {
        return None;
    }
    // The second register as it is, not shifted.
    let plain = kind == 0 && shift == 0;
    let tests = rd == PC && s && matches!(op, 0b0000 | 0b0100 | 0b1000 | 0b1101);
    if tests {
        let effect = Effect::new(reads(&[rn, rm]), 0).sets_flags(true);
        // cmp.w
        return Some(match op {
            0b1101 if plain => effect.operation(Operation::CompareRegisters { a: rn, b: rm }),
            _ => effect,
        });
    }
    if rd == PC {
        return None;
    }
    let effect = Effect::new(reads(&[rn, rm]), bit(rd)).sets_flags(s);
    Some(match op {
        // mov.w and lsl.w with an immediate.
        0b0010 if rn == PC && kind == 0 => effect.operation(Operation::ShiftLeft {
            register: rd,
            source: rm,
            shift,
        }),
        // lsr.w and asr.w with an immediate, where 0 means 32.
        0b0010 if rn == PC && (kind == 1 || kind == 2) => {
            effect.operation(shift_right(rd, rm, shift, kind == 2))
        }
        // and.w and bic.w
        0b0000 | 0b0001 if plain => effect.operation(Operation::And {
            register: rd,
            a: rn,
            b: rm,
            complement: op == 0b0001,
        }),
        0b1000 if plain => effect.operation(Operation::Add {
            register: rd,
            a: rn,
            b: rm,
        }),
        0b1101 if plain => effect.operation(Operation::Subtract {
            register: rd,
            a: rn,
            b: rm,
        }),
        0b1000 | 0b1101 if kind == 0 => effect.operation(Operation::AddShifted {
            register: rd,
            a: rn,
            b: rm,
            shift,
            subtract: op == 0b1101,
        }),
        _ => effect,
    })
}

/// The data-processing instructions with a modified immediate, a byte
/// that can be shifted or repeated: `and`, `bic`, `orr`, `mov.w`, `orn`,
/// `mvn`, `eor`, `add.w`, `adc`, `sbc`, `sub.w` and `rsb`, and `tst`,
/// `teq`, `cmn` and `cmp`, which only set the flags.
fn modified_immediate(first: u16, second: u16) -> Option<Effect> {
    let op = (first >> 5) & 0xf;
    let s = first & 0x10 != 0;
    let rn = first & 0xf;
    let rd = (second >> 8) & 0xf;
    let imm12 = u32::from((first >> 10) & 1) << 11
        | u32::from((second >> 12) & 7) << 8
        | u32::from(second & 0xff);
    let value = expand_immediate(imm12);
    if !matches!(
        op,
        0b0000..=0b0100 | 0b1000 | 0b1010 | 0b1011 | 0b1101 | 0b1110
    ) {
        return None;
    }
    let tests = rd == PC && s && matches!(op, 0b0000 | 0b0100 | 0b1000 | 0b1101);
    if tests {
        let effect = Effect::new(reads(&[rn]), 0).sets_flags(true);
        // cmp.w
        return Some(match op {
            0b1101 => effect.operation(Operation::Compare {
                register: rn,
                value,
            }),
            _ => effect,
        });
    }
    if rd == PC {
        return None;
    }
    let effect = Effect::new(reads(&[rn]), bit(rd)).sets_flags(s);
    Some(match op {
        // and and bic
        0b0000 => effect.operation(Operation::AndConstant {
            register: rd,
            source: rn,
            mask: value,
        }),
        0b0001 => effect.operation(Operation::AndConstant {
            register: rd,
            source: rn,
            mask: !value,
        }),
        // mov.w and mvn
        0b0010 if rn == PC => effect.operation(Operation::Constant {
            register: rd,
            value,
        }),
        0b0011 if rn == PC => effect.operation(Operation::Constant {
            register: rd,
            value: !value,
        }),
        // add.w and sub.w
        0b1000 => effect.operation(Operation::AddConstant {
            register: rd,
            source: rn,
            amount: value,
        }),
        0b1101 => effect.operation(Operation::AddConstant {
            register: rd,
            source: rn,
            amount: value.wrapping_neg(),
        }),
        _ => effect,
    })
}

/// The value of a modified immediate: a byte, repeated in some bytes of
/// the word or rotated into place (`ThumbExpandImm` in the manual).
fn expand_immediate(imm12: u32) -> u32 {
    let byte = imm12 & 0xff;
    if imm12 >> 10 == 0 {
        match (imm12 >> 8) & 3 {
            0 => byte,
            1 => byte << 16 | byte,
            2 => byte << 24 | byte << 8,
            _ => byte * 0x0101_0101,
        }
    } else {
        (0x80 | imm12 & 0x7f).rotate_right(imm12 >> 7)
    }
}

/// The data-processing instructions with a plain 12- or 16-bit immediate:
/// `addw`, `adr.w`, `movw`, `subw`, `movt`, and the saturating and bit
/// field instructions.
fn plain_immediate(pc: u64, first: u16, second: u16) -> Option<Effect> {
    let rn = first & 0xf;
    let rd = (second >> 8) & 0xf;
    if rd == PC {
        return None;
    }
    let imm12 = u32::from((first >> 10) & 1) << 11
        | u32::from((second >> 12) & 7) << 8
        | u32::from(second & 0xff);
    let imm16 = u32::from(rn) << 12 | imm12;
    let effect = Effect::new(reads(&[rn]), bit(rd));
    Some(match (first >> 4) & 0x1f {
        // adr.w, after or before the instruction
        0b00000 if rn == PC => Effect::new(0, bit(rd)).operation(Operation::Address {
            register: rd,
            value: (align(pc) + u64::from(imm12)) as u32,
        }),
        0b01010 if rn == PC => Effect::new(0, bit(rd)).operation(Operation::Address {
            register: rd,
            value: align(pc).wrapping_sub(u64::from(imm12)) as u32,
        }),
        // addw and subw
        0b00000 => effect.operation(Operation::AddConstant {
            register: rd,
            source: rn,
            amount: imm12,
        }),
        0b01010 => effect.operation(Operation::AddConstant {
            register: rd,
            source: rn,
            amount: imm12.wrapping_neg(),
        }),
        // movw
        0b00100 => Effect::new(0, bit(rd)).operation(Operation::Constant {
            register: rd,
            value: imm16,
        }),
        // movt, which keeps the low half.
        0b01100 => Effect::new(bit(rd), bit(rd)).operation(Operation::HighHalf {
            register: rd,
            high: imm16,
        }),
        // ssat, ssat16, sbfx, usat, usat16 and ubfx
        0b10000 | 0b10010 | 0b10100 | 0b11000 | 0b11010 | 0b11100 => effect,
        // bfi and bfc, which keep the other bits.
        0b10110 => Effect::new(reads(&[rn, rd]), bit(rd)),
        _ => return None,
    })
}

/// Branches and the instructions in their encoding space: `bl`, `b.w`,
/// `b<cond>.w`, `msr`, `mrs`, hints such as `nop.w`, barriers and `udf.w`.
fn branch_or_control(pc: u64, first: u16, second: u16) -> Option<Effect> {
    let effect = Effect::new(0, 0);
    let target = |kind| match kind {
        Some(CodeReferenceKind::Call(target))
        | Some(CodeReferenceKind::Jump(target))
        | Some(CodeReferenceKind::TailJump(target)) => Some(target),
        _ => None,
    };
    match second & 0x5000 {
        0x5000 => Some(Effect::new(0, bit(LR)).flow(Flow::Call)),
        0x1000 => Some(effect.flow(Flow::Branch {
            target: target(branch(pc, first, second))?,
            when: When::Always,
        })),
        0x0000 if (first >> 6) & 0xf < 0xe => Some(effect.flow(Flow::Branch {
            target: target(branch(pc, first, second))?,
            when: When::Flags((first >> 6) & 0xf),
        })),
        0x0000 => match first & 0xffe0 {
            // msr, which can set the flags.
            0xf380 => Some(Effect::new(bit(first & 0xf), 0).sets_flags(true)),
            // Hints such as nop.w, and barriers such as dsb.
            0xf3a0 if first == 0xf3af => Some(effect),
            0xf3a0 if first == 0xf3bf => Some(effect),
            // mrs
            0xf3e0 if !matches!((second >> 8) & 0xf, SP | PC) => {
                Some(Effect::new(0, bit((second >> 8) & 0xf)))
            }
            // udf.w
            0xf7e0 if first & 0xfff0 == 0xf7f0 && second & 0xf000 == 0xa000 => {
                Some(effect.flow(Flow::Stop))
            }
            _ => None,
        },
        // blx to Arm code, which M-profile processors can't run, and
        // Armv8.1-M's loop instructions.
        _ => None,
    }
}

/// `str.w`, `strb.w` and `strh.w`, with any addressing.
fn store(first: u16, second: u16) -> Option<Effect> {
    let rn = first & 0xf;
    let rt = second >> 12;
    if first & 0x60 == 0x60 || rn == PC || rt == PC {
        return None;
    }
    let size = 1 << ((first >> 5) & 3);
    let mut reads = bit(rn) | bit(rt);
    let addressing = addressing(first, second)?;
    let (offset, writeback) = match addressing {
        Addressing::Offset(offset) => (Some(offset), 0),
        Addressing::Register(index) => {
            reads |= bit(index.register);
            (None, 0)
        }
        // str.w register, [sp, #-4]! pushes it.
        Addressing::Before(-4) if rn == SP && size == 4 => {
            return Some(Effect::new(reads, bit(SP)).operation(Operation::Push {
                registers: bit(rt),
                size: 4,
            }));
        }
        Addressing::Before(_) | Addressing::After(_) => (None, bit(rn)),
    };
    let effect = Effect::new(reads, writeback).operation(Operation::Store {
        register: (size == 4).then_some(rt),
        base: rn,
        offset,
        size,
    });
    Some(
        effect
            .writeback(addressing.writeback())
            .index(addressing.index()),
    )
}

/// How a 32-bit load or store of a byte, halfword or word gets its
/// address from its base register.
enum Addressing {
    /// The base plus an immediate: `[base, #imm]`.
    Offset(i32),
    /// The base plus a register, which may be shifted: `[base, register,
    /// lsl #imm]`.
    Register(Index),
    /// The base plus an immediate, which the base then holds: `[base,
    /// #imm]!`.
    Before(i32),
    /// The base, which then gets the immediate added: `[base], #imm`.
    After(i32),
}

impl Addressing {
    /// What it adds to the base register, when it does.
    fn writeback(&self) -> Option<Writeback> {
        match *self {
            Addressing::Before(amount) => Some(Writeback {
                amount,
                before: true,
            }),
            Addressing::After(amount) => Some(Writeback {
                amount,
                before: false,
            }),
            Addressing::Offset(_) | Addressing::Register(_) => None,
        }
    }

    /// The register it adds to the base, when it adds one.
    fn index(&self) -> Option<Index> {
        match *self {
            Addressing::Register(index) => Some(index),
            _ => None,
        }
    }
}

/// The addressing of the load or store made of `first` and `second`,
/// whose base isn't pc. `None` for an encoding that is undefined.
fn addressing(first: u16, second: u16) -> Option<Addressing> {
    // With a 12-bit immediate.
    if first & 0x80 != 0 {
        return Some(Addressing::Offset(i32::from(second & 0xfff)));
    }
    if second & 0x800 == 0 {
        return (second & 0x7c0 == 0).then_some(Addressing::Register(Index {
            register: second & 0xf,
            shift: u32::from((second >> 4) & 3),
        }));
    }
    // With an 8-bit immediate, P, U and W are bits 10 to 8.
    let imm8 = i32::from(second & 0xff);
    let offset = if second & 0x200 != 0 { imm8 } else { -imm8 };
    match (second >> 8) & 7 {
        0b110 | 0b100 => Some(Addressing::Offset(offset)),
        0b111 | 0b101 => Some(Addressing::Before(offset)),
        0b011 | 0b001 => Some(Addressing::After(offset)),
        _ => None,
    }
}

/// `ldr.w`, `ldrb.w`, `ldrh.w`, `ldrsb` and `ldrsh`, with any addressing,
/// and the preload hints `pld` and `pli`.
fn load(pc: u64, first: u16, second: u16) -> Option<Effect> {
    let rn = first & 0xf;
    let rt = second >> 12;
    let size = (first >> 5) & 3;
    let signed = first & 0x100 != 0;
    if size == 0b11 || (signed && size == 0b10) {
        return None;
    }
    let word = size == 0b10;
    if rn == PC {
        let effect = Effect::new(0, if rt == PC { 0 } else { bit(rt) });
        let offset = u64::from(second & 0xfff);
        let address = if first & 0x80 != 0 {
            align(pc) + offset
        } else {
            align(pc).wrapping_sub(offset)
        };
        return Some(match (rt, word) {
            // A byte or halfword into pc is a preload hint.
            (PC, false) => effect,
            // A veneer's jump.
            (PC, true) => effect.flow(Flow::JumpMemory),
            (_, true) => effect.operation(Operation::Literal {
                register: rt,
                address,
            }),
            (_, false) => effect,
        });
    }
    let mut reads = bit(rn);
    let addressing = addressing(first, second)?;
    let (offset, writeback) = match addressing {
        Addressing::Offset(offset) => (Some(offset), 0),
        Addressing::Register(index) => {
            reads |= bit(index.register);
            (None, 0)
        }
        // A word loaded with ldr.w register, [sp], #4 is popped.
        Addressing::After(4) if rn == SP && word => {
            let effect =
                Effect::new(reads, (bit(rt) | bit(SP)) & !bit(PC)).operation(Operation::Pop {
                    registers: bit(rt),
                    size: 4,
                });
            return Some(match rt {
                PC => effect.flow(Flow::Pop),
                _ => effect,
            });
        }
        Addressing::Before(_) | Addressing::After(_) => (None, bit(rn)),
    };
    let load = |register| Operation::Load {
        register,
        base: rn,
        offset,
    };
    if rt == PC {
        // A byte or halfword into pc is a preload hint.
        let effect = Effect::new(reads, writeback).operation(load(None));
        let effect = effect.writeback(addressing.writeback());
        return Some(match word {
            true => effect.flow(Flow::JumpMemory),
            false => effect,
        });
    }
    if writeback & bit(rt) != 0 {
        return None;
    }
    let effect = Effect::new(reads, bit(rt) | writeback).operation(load(word.then_some(rt)));
    Some(
        effect
            .writeback(addressing.writeback())
            .index(addressing.index()),
    )
}

/// The floating-point instructions, with Armv8.1-M's loads and stores of
/// its system registers. Only those that move values to or from the core
/// registers, and loads and stores that update their base register, touch
/// the registers elfq follows. Those of other coprocessors, and
/// Armv8.1-M's vector instructions, which share their encodings, aren't
/// followed.
fn coprocessor(first: u16, second: u16) -> Option<Effect> {
    // Coprocessor field 15, as some vector instructions have too, with P
    // or W set.
    if first & 0xfe00 == 0xec00 && second & 0x1f80 == 0x0f80 && first & 0x120 != 0 {
        return system_register(first, second);
    }
    // The floating-point instructions use coprocessor fields 9, for half
    // precision, 10 and 11.
    let field = (second >> 8) & 0xf;
    if !matches!(field, 0b1001..=0b1011) {
        return None;
    }
    let rn = first & 0xf;
    let rt = second >> 12;
    let loads = first & 0x10 != 0;
    match first & 0xef00 {
        // Advanced SIMD and the rest of Armv8.1-M's vector instructions.
        0xef00 => None,
        // mcrr, mrrc and vmov between two core registers and floating-point
        // ones.
        0xec00 if first & 0xffe0 == 0xec40 || first & 0xffe0 == 0xfc40 => {
            let pair = bit(rt) | bit(rn);
            if pair & (bit(PC) | bit(SP)) != 0 {
                return None;
            }
            Some(if loads {
                Effect::new(0, pair)
            } else {
                Effect::new(pair, 0)
            })
        }
        // Armv8-M's vlstm and vlldm, which save the floating-point
        // registers to the 0x88 bytes at Rn and load them back, and leave
        // Rn as it is.
        0xec00 if first & 0xffe0 == 0xec20 && second & 0xff7f == 0x0a00 => {
            let effect = Effect::new(bit(rn), 0);
            Some(if loads {
                effect.operation(Operation::Load {
                    register: None,
                    base: rn,
                    offset: Some(0),
                })
            } else {
                effect.operation(Operation::Store {
                    register: None,
                    base: rn,
                    offset: Some(0),
                    size: 0x88,
                })
            })
        }
        // The floating-point loads and stores: vldr, vstr, vldm, vstm,
        // vpush and vpop. P, U and W are bits 8, 7 and 5.
        0xec00 | 0xed00 => {
            let (p, u, w) = (first & 0x100 != 0, first & 0x80 != 0, first & 0x20 != 0);
            let writeback = w && !p || p && !u && w;
            let writes = if writeback && rn != PC { bit(rn) } else { 0 };
            let effect = Effect::new(reads(&[rn]), writes);
            let half = field == 0b1001;
            let size = u32::from(second & 0xff) * if half { 2 } else { 4 };
            let stack = !half && rn == SP && writeback;
            Some(match (loads, p) {
                // vpop and vpush
                (true, false) if stack && u => {
                    effect.operation(Operation::Pop { registers: 0, size })
                }
                (false, true) if stack && !u => {
                    effect.operation(Operation::Push { registers: 0, size })
                }
                // vldr of a half or a single, from one word.
                (true, true) if !w && field != 0b1011 => {
                    let offset = size as i32;
                    effect.operation(Operation::Load {
                        register: None,
                        base: rn,
                        offset: Some(if u { offset } else { -offset }),
                    })
                }
                (true, _) => effect.operation(Operation::Load {
                    register: None,
                    base: rn,
                    offset: None,
                }),
                // vstr, which stores a half, a single or a double.
                (false, true) if !w => {
                    let offset = size as i32;
                    effect.operation(Operation::Store {
                        register: None,
                        base: rn,
                        offset: Some(if u { offset } else { -offset }),
                        size: match field {
                            0b1001 => 2,
                            0b1010 => 4,
                            _ => 8,
                        },
                    })
                }
                (false, _) => effect.operation(Operation::Store {
                    register: None,
                    base: rn,
                    offset: None,
                    size: 0,
                }),
            })
        }
        // vmov between a core register and a floating-point one, vmrs and
        // vmsr, and the floating-point data processing.
        _ => {
            if second & 0x10 == 0 {
                // Data processing, which uses only floating-point
                // registers.
                return Some(Effect::new(0, 0));
            }
            if loads {
                // vmov to a core register, and vmrs, which into pc sets the
                // flags.
                return Some(if rt == PC {
                    Effect::new(0, 0).sets_flags(true)
                } else if rt == SP {
                    return None;
                } else {
                    Effect::new(0, bit(rt))
                });
            }
            Some(Effect::new(reads(&[rt]), 0))
        }
    }
}

/// Armv8.1-M's vldr and vstr of a floating-point system register, such as
/// the FPCXTNS that a function called from Non-secure code saves: one word
/// at Rn plus or minus `imm7` * 4, or at Rn, which it then changes by that.
fn system_register(first: u16, second: u16) -> Option<Effect> {
    let rn = first & 0xf;
    let (p, u, w) = (first & 0x100 != 0, first & 0x80 != 0, first & 0x20 != 0);
    if rn == PC {
        return None;
    }
    let amount = i32::from(second & 0x7f) * 4;
    let amount = if u { amount } else { -amount };
    let offset = Some(if p { amount } else { 0 });
    let effect = Effect::new(bit(rn), if w { bit(rn) } else { 0 });
    Some(match (first & 0x10 != 0, rn == SP && w) {
        (true, true) if !p && u => effect.operation(Operation::Pop {
            registers: 0,
            size: amount as u32,
        }),
        (false, true) if p && !u => effect.operation(Operation::Push {
            registers: 0,
            size: amount.unsigned_abs(),
        }),
        (true, _) => effect.operation(Operation::Load {
            register: None,
            base: rn,
            offset,
        }),
        (false, _) => effect.operation(Operation::Store {
            register: None,
            base: rn,
            offset,
            size: 4,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decodes the instruction in `bytes`, little-endian as in a file.
    fn effect(address: u64, bytes: &[u8]) -> Effect {
        let first = u16::from_le_bytes([bytes[0], bytes[1]]);
        let second = match bytes.get(2..4) {
            Some(second) => u16::from_le_bytes([second[0], second[1]]),
            None => 0,
        };
        decode(address, first, second).unwrap()
    }

    fn operation(bytes: &[u8]) -> Operation {
        effect(0, bytes).operation
    }

    #[test]
    fn instructions_that_build_constants() {
        // movs r3, #0x30
        assert_eq!(
            operation(&[0x30, 0x23]),
            Operation::Constant {
                register: 3,
                value: 0x30
            }
        );
        // lsls r3, r3, #8
        assert_eq!(
            operation(&[0x1b, 0x02]),
            Operation::ShiftLeft {
                register: 3,
                source: 3,
                shift: 8
            }
        );
        // adds r3, #0x29
        assert_eq!(
            operation(&[0x29, 0x33]),
            Operation::AddConstant {
                register: 3,
                source: 3,
                amount: 0x29
            }
        );
        // adds r7, r5, #1
        assert_eq!(
            operation(&[0x6f, 0x1c]),
            Operation::AddConstant {
                register: 7,
                source: 5,
                amount: 1
            }
        );
        // movw r3, #0x1a5 and movt r3, #0x3000
        assert_eq!(
            operation(&[0x40, 0xf2, 0xa5, 0x13]),
            Operation::Constant {
                register: 3,
                value: 0x1a5
            }
        );
        assert_eq!(
            operation(&[0xc3, 0xf2, 0x00, 0x03]),
            Operation::HighHalf {
                register: 3,
                high: 0x3000
            }
        );
        // movt ip, #0x1fff
        assert_eq!(
            operation(&[0xc1, 0xf6, 0xff, 0x7c]),
            Operation::HighHalf {
                register: 12,
                high: 0x1fff
            }
        );
        // mov.w r5, #0x1000 and mov.w r5, #0x8000
        assert_eq!(
            operation(&[0x4f, 0xf4, 0x80, 0x55]),
            Operation::Constant {
                register: 5,
                value: 0x1000
            }
        );
        assert_eq!(
            operation(&[0x4f, 0xf4, 0x00, 0x45]),
            Operation::Constant {
                register: 5,
                value: 0x8000
            }
        );
        // addw r5, r5, #0x201
        assert_eq!(
            operation(&[0x05, 0xf2, 0x01, 0x25]),
            Operation::AddConstant {
                register: 5,
                source: 5,
                amount: 0x201
            }
        );
        // add.w r1, r6, #0x4a8
        assert_eq!(
            operation(&[0x06, 0xf5, 0x95, 0x61]),
            Operation::AddConstant {
                register: 1,
                source: 6,
                amount: 0x4a8
            }
        );
        // sub.w r3, r3, #0xff00
        assert_eq!(
            operation(&[0xa3, 0xf5, 0x7f, 0x43]),
            Operation::AddConstant {
                register: 3,
                source: 3,
                amount: 0xff00u32.wrapping_neg()
            }
        );
        // mov ip, r0; mov ip, pc; add ip, r0; add r5, pc
        assert_eq!(
            operation(&[0x84, 0x46]),
            Operation::Copy {
                register: 12,
                source: 0
            }
        );
        assert_eq!(
            operation(&[0xfc, 0x46]),
            Operation::Copy {
                register: 12,
                source: PC
            }
        );
        assert_eq!(
            operation(&[0x84, 0x44]),
            Operation::Add {
                register: 12,
                a: 12,
                b: 0
            }
        );
        assert_eq!(
            operation(&[0x7d, 0x44]),
            Operation::Add {
                register: 5,
                a: 5,
                b: PC
            }
        );
        // movs r6, r0 (lsls r6, r0, #0)
        assert_eq!(
            operation(&[0x06, 0x00]),
            Operation::ShiftLeft {
                register: 6,
                source: 0,
                shift: 0
            }
        );
    }

    #[test]
    fn literal_loads_name_the_constant() {
        // 0x2: ldr r3, [pc, #8] reads 0xc.
        assert_eq!(
            effect(2, &[0x02, 0x4b]).operation,
            Operation::Literal {
                register: 3,
                address: 0xc
            }
        );
        // 0x132: ldr.w r8, [pc, #56] reads 0x16c.
        assert_eq!(
            effect(0x132, &[0xdf, 0xf8, 0x38, 0x80]).operation,
            Operation::Literal {
                register: 8,
                address: 0x16c
            }
        );
    }

    #[test]
    fn branches_through_registers() {
        // blx r3; bx r3; bx lr; mov pc, r3; add pc, ip
        let thumb = LowBit::Thumb;
        assert_eq!(effect(0, &[0x98, 0x47]).flow, Flow::CallRegister(3, thumb));
        assert_eq!(effect(0, &[0x18, 0x47]).flow, Flow::JumpRegister(3, thumb));
        assert_eq!(effect(0, &[0x70, 0x47]).flow, Flow::Return);
        assert_eq!(
            effect(0, &[0x9f, 0x46]).flow,
            Flow::JumpRegister(3, LowBit::Ignored)
        );
        assert_eq!(effect(0, &[0xe7, 0x44]).flow, Flow::JumpRelative(12));
        // blxns r4 and bxns r4
        let security = LowBit::Security;
        assert_eq!(
            effect(0, &[0xa4, 0x47]).flow,
            Flow::CallRegister(4, security)
        );
        assert_eq!(
            effect(0, &[0x24, 0x47]).flow,
            Flow::JumpRegister(4, security)
        );
        // pop {r0, pc}, pop.w {r4, r5, r6, r7, r8, pc} and ldr.w pc, [sp],
        // #4 load pc from the stack; ldr.w pc, [r3, #4] from elsewhere.
        assert_eq!(effect(0, &[0x01, 0xbd]).flow, Flow::Pop);
        assert_eq!(effect(0, &[0xbd, 0xe8, 0xf0, 0x81]).flow, Flow::Pop);
        assert_eq!(effect(0, &[0x5d, 0xf8, 0x04, 0xfb]).flow, Flow::Pop);
        assert_eq!(effect(0, &[0xd3, 0xf8, 0x04, 0xf0]).flow, Flow::JumpMemory);
    }

    #[test]
    fn branches_say_when_they_are_taken() {
        // beq, b, cbz r3 and cbnz r3
        let when = |bytes: &[u8]| match effect(0, bytes).flow {
            Flow::Branch { when, .. } => when,
            flow => panic!("{flow:?} isn't a branch"),
        };
        assert_eq!(when(&[0x02, 0xd0]), When::Flags(0));
        assert_eq!(when(&[0x02, 0xe0]), When::Always);
        assert_eq!(when(&[0x03, 0xb1]), When::Zero(3));
        assert_eq!(when(&[0x03, 0xb9]), When::NotZero(3));
        // bne.w and b.w
        assert_eq!(when(&[0x40, 0xf0, 0x02, 0x80]), When::Flags(1));
        assert_eq!(when(&[0x00, 0xf0, 0x02, 0xb8]), When::Always);
    }

    #[test]
    fn calls_write_the_return_address() {
        // bl and blx r5
        let call = effect(0, &[0xff, 0xf7, 0xd3, 0xff]);
        assert_eq!((call.writes, call.flow), (bit(LR), Flow::Call));
        let call = effect(0, &[0xa8, 0x47]);
        assert_eq!(
            (call.writes, call.flow),
            (bit(LR), Flow::CallRegister(5, LowBit::Thumb))
        );
    }

    #[test]
    fn the_stack() {
        // push {r4, lr} and pop {r0, pc}
        assert_eq!(
            operation(&[0x10, 0xb5]),
            Operation::Push {
                registers: bit(4) | bit(LR),
                size: 8
            }
        );
        assert_eq!(
            operation(&[0x01, 0xbd]),
            Operation::Pop {
                registers: bit(0) | bit(PC),
                size: 8
            }
        );
        // str.w r3, [sp, #-4]! and ldr.w r3, [sp], #4 push and pop one.
        assert_eq!(
            operation(&[0x4d, 0xf8, 0x04, 0x3d]),
            Operation::Push {
                registers: bit(3),
                size: 4
            }
        );
        assert_eq!(
            operation(&[0x5d, 0xf8, 0x04, 0x3b]),
            Operation::Pop {
                registers: bit(3),
                size: 4
            }
        );
        // vpush {d8}
        assert_eq!(
            operation(&[0x2d, 0xed, 0x02, 0x8b]),
            Operation::Push {
                registers: 0,
                size: 8
            }
        );
        // sub sp, #8
        assert_eq!(
            operation(&[0x82, 0xb0]),
            Operation::AddConstant {
                register: SP,
                source: SP,
                amount: (-8i32) as u32
            }
        );
    }

    #[test]
    fn loads_and_stores() {
        // str r0, [sp, #4] and ldr r1, [sp, #4]
        assert_eq!(
            operation(&[0x01, 0x90]),
            Operation::Store {
                register: Some(0),
                base: SP,
                offset: Some(4),
                size: 4
            }
        );
        assert_eq!(
            operation(&[0x01, 0x99]),
            Operation::Load {
                register: Some(1),
                base: SP,
                offset: Some(4)
            }
        );
        // str.w r3, [r7, #-12] and strb r0, [r7, #3]
        assert_eq!(
            operation(&[0x47, 0xf8, 0x0c, 0x3c]),
            Operation::Store {
                register: Some(3),
                base: 7,
                offset: Some(-12),
                size: 4
            }
        );
        assert_eq!(
            operation(&[0xf8, 0x70]),
            Operation::Store {
                register: None,
                base: 7,
                offset: Some(3),
                size: 1
            }
        );
        // ldrb r3, [r7, #7] loads no word; ldr r0, [r1, r2] gives no
        // offset, as it adds r2 (see `index` below).
        assert_eq!(
            operation(&[0xfb, 0x79]),
            Operation::Load {
                register: None,
                base: 7,
                offset: Some(7)
            }
        );
        assert_eq!(
            operation(&[0x88, 0x58]),
            Operation::Load {
                register: Some(0),
                base: 1,
                offset: None
            }
        );
        // ldr r3, [r3, #0] writes r3; str r4, [r3, #0] writes nothing.
        let load = effect(0, &[0x1b, 0x68]);
        assert_eq!((load.reads, load.writes), (bit(3), bit(3)));
        let store = effect(0, &[0x1c, 0x60]);
        assert_eq!((store.reads, store.writes), (bit(3) | bit(4), 0));
        // ldr.w r3, [r2, r3, lsl #2]
        let load = effect(0, &[0x52, 0xf8, 0x23, 0x30]);
        assert_eq!((load.reads, load.writes), (bit(2) | bit(3), bit(3)));
        assert_eq!(load.writeback, None);
        // ldr.w r3, [r5], #4 loads from r5 and then adds 4 to it; strb.w
        // r1, [r0, #-1]! subtracts 1 first.
        let load = effect(0, &[0x55, 0xf8, 0x04, 0x3b]);
        assert_eq!((load.reads, load.writes), (bit(5), bit(3) | bit(5)));
        assert_eq!(
            load.writeback,
            Some(Writeback {
                amount: 4,
                before: false
            })
        );
        let store = effect(0, &[0x00, 0xf8, 0x01, 0x1d]);
        assert_eq!((store.reads, store.writes), (bit(0) | bit(1), bit(0)));
        assert_eq!(
            store.writeback,
            Some(Writeback {
                amount: -1,
                before: true
            })
        );
        // stmia r0!, {r3, r4} stores two words from r0 on (see its words
        // below).
        assert_eq!(
            operation(&[0x18, 0xc0]),
            Operation::Store {
                register: None,
                base: 0,
                offset: None,
                size: 8
            }
        );
        // ldmia.w sp!, {r7, lr}
        let pop = effect(0, &[0xbd, 0xe8, 0x80, 0x40]);
        assert_eq!(pop.writes, bit(7) | bit(LR) | bit(SP));
        assert_eq!(pop.flow, Flow::Next);
        // ldrd r0, r1, [sp], #8
        let pair = effect(0, &[0xfd, 0xe8, 0x02, 0x01]);
        assert_eq!(pair.writes, bit(0) | bit(1) | bit(SP));
        // The register that str r0, [r1, r2], ldr r0, [r1, r2], ldr.w r3,
        // [r2, r3, lsl #2] and str.w r0, [r1, r2, lsl #2] add to their
        // base, that ldr r1, [sp, #4] adds none, and that str r0, [r1, r2]
        // stores a word.
        let index = |register, shift| Some(Index { register, shift });
        let store = effect(0, &[0x88, 0x50]);
        assert_eq!(store.index, index(2, 0));
        assert!(matches!(
            store.operation,
            Operation::Store {
                register: Some(0),
                size: 4,
                ..
            }
        ));
        assert_eq!(effect(0, &[0x88, 0x58]).index, index(2, 0));
        assert_eq!(effect(0, &[0x52, 0xf8, 0x23, 0x30]).index, index(3, 2));
        assert_eq!(effect(0, &[0x41, 0xf8, 0x22, 0x00]).index, index(2, 2));
        assert_eq!(effect(0, &[0x01, 0x99]).index, None);
        // The words of ldm and stm: ldmia r1!, {r3, r4} and stmia r0!,
        // {r3, r4} move their base past them, ldmia r1, {r1, r2} doesn't,
        // since it loads r1, ldmdb.w r0!, {r2, r3} and stmdb.w r0!, {r2,
        // r3} end at r0, which they move down first, and ldmia.w r4,
        // {r0-r3} doesn't move r4.
        let list = |first, down| {
            Some(Words::List {
                registers: bit(first) | bit(first + 1),
                down,
            })
        };
        let moves = |amount, before| Some(Writeback { amount, before });
        let load = effect(0, &[0x18, 0xc9]);
        assert_eq!(
            (load.words, load.writeback),
            (list(3, false), moves(8, false))
        );
        let store = effect(0, &[0x18, 0xc0]);
        assert_eq!(
            (store.words, store.writeback),
            (list(3, false), moves(8, false))
        );
        let load = effect(0, &[0x06, 0xc9]);
        assert_eq!((load.words, load.writeback), (list(1, false), None));
        let load = effect(0, &[0x30, 0xe9, 0x0c, 0x00]);
        assert_eq!(
            (load.words, load.writeback),
            (list(2, true), moves(-8, true))
        );
        let store = effect(0, &[0x20, 0xe9, 0x0c, 0x00]);
        assert_eq!(
            (store.words, store.writeback),
            (list(2, true), moves(-8, true))
        );
        assert_eq!(effect(0, &[0x94, 0xe8, 0x0f, 0x00]).writeback, None);
        // The words of ldrd and strd: ldrd r2, r3, [r1, #-8], strd r2, r3,
        // [r0, #8]!, and ldrd r0, r1, [r4], #8, which adds 8 after.
        let pair = |first, offset| {
            Some(Words::Pair {
                first,
                second: first + 1,
                offset,
            })
        };
        let load = effect(0, &[0x51, 0xe9, 0x02, 0x23]);
        assert_eq!((load.words, load.writeback), (pair(2, -8), None));
        let store = effect(0, &[0xe0, 0xe9, 0x02, 0x23]);
        assert_eq!((store.words, store.writeback), (pair(2, 8), moves(8, true)));
        let load = effect(0, &[0xf4, 0xe8, 0x02, 0x01]);
        assert_eq!((load.words, load.writeback), (pair(0, 0), moves(8, false)));
        assert_eq!(load.writes, bit(0) | bit(1) | bit(4));
        // stl r2, [r3] and stlh r2, [r3] write nothing; strexb r1, r2,
        // [r3] and stlex r1, r2, [r3] write their status to r1.
        assert_eq!(effect(0, &[0xc3, 0xe8, 0xaf, 0x2f]).writes, 0);
        assert_eq!(effect(0, &[0xc3, 0xe8, 0x9f, 0x2f]).writes, 0);
        assert_eq!(effect(0, &[0xc3, 0xe8, 0x41, 0x2f]).writes, bit(1));
        assert_eq!(effect(0, &[0xc3, 0xe8, 0xe1, 0x2f]).writes, bit(1));
        // Armv8.1-M's clrm {r0, r1, r2, r4-fp, ip, APSR} zeroes them and
        // the flags, and goes on.
        let clear = effect(0, &[0x9f, 0xe8, 0xf7, 0x9f]);
        assert_eq!(clear.writes, 0x1ff7);
        assert!(clear.sets_flags);
        assert_eq!(clear.flow, Flow::Next);
    }

    #[test]
    fn it_and_the_flags() {
        // ite gt
        assert_eq!(
            effect(0, &[0xcc, 0xbf]).flow,
            Flow::It {
                firstcond: 0xc,
                mask: 0xc
            }
        );
        // cmp r0, #5 sets the flags; adds r0, #1 doesn't inside it.
        let compare = effect(0, &[0x05, 0x28]);
        assert!(compare.sets_flags);
        assert_eq!(
            compare.operation,
            Operation::Compare {
                register: 0,
                value: 5
            }
        );
        assert!(!effect(0, &[0x01, 0x30]).sets_flags);
        // cmp.w r0, #-1
        let compare = effect(0, &[0xb0, 0xf1, 0xff, 0x3f]);
        assert!(compare.sets_flags);
        assert_eq!(
            compare.operation,
            Operation::Compare {
                register: 0,
                value: u32::MAX
            }
        );
    }

    #[test]
    fn instructions_that_work_out_lengths_and_compare_them() {
        // subs r2, r2, r0 and sub.w r2, r3, r1
        assert_eq!(
            operation(&[0x12, 0x1a]),
            Operation::Subtract {
                register: 2,
                a: 2,
                b: 0
            }
        );
        assert_eq!(
            operation(&[0xa3, 0xeb, 0x01, 0x02]),
            Operation::Subtract {
                register: 2,
                a: 3,
                b: 1
            }
        );
        // lsrs r2, r2, #2, asrs r1, r0, #31 and lsrs r0, r1, #32
        assert_eq!(
            operation(&[0x92, 0x08]),
            Operation::ShiftRight {
                register: 2,
                source: 2,
                shift: 2,
                signed: false
            }
        );
        assert_eq!(
            operation(&[0xc1, 0x17]),
            Operation::ShiftRight {
                register: 1,
                source: 0,
                shift: 31,
                signed: true
            }
        );
        assert_eq!(
            operation(&[0x08, 0x08]),
            Operation::ShiftRight {
                register: 0,
                source: 1,
                shift: 32,
                signed: false
            }
        );
        // lsr.w r2, r2, #2 and asr.w r0, r1, #1
        assert_eq!(
            operation(&[0x4f, 0xea, 0x92, 0x02]),
            Operation::ShiftRight {
                register: 2,
                source: 2,
                shift: 2,
                signed: false
            }
        );
        assert_eq!(
            operation(&[0x4f, 0xea, 0x61, 0x00]),
            Operation::ShiftRight {
                register: 0,
                source: 1,
                shift: 1,
                signed: true
            }
        );
        // and r0, r1, #7, and bic r0, r0, #3, which keeps the other bits.
        assert_eq!(
            operation(&[0x01, 0xf0, 0x07, 0x00]),
            Operation::AndConstant {
                register: 0,
                source: 1,
                mask: 7
            }
        );
        assert_eq!(
            operation(&[0x20, 0xf0, 0x03, 0x00]),
            Operation::AndConstant {
                register: 0,
                source: 0,
                mask: !3
            }
        );
        // ands r2, r2, #0xff sets the flags.
        let and = effect(0, &[0x12, 0xf0, 0xff, 0x02]);
        assert!(and.sets_flags);
        assert_eq!(
            and.operation,
            Operation::AndConstant {
                register: 2,
                source: 2,
                mask: 0xff
            }
        );
        // bics r2, r1 and ands r2, r1, as Cortex-M0 code rounds a length,
        // and bic.w r0, r1, r2 and and.w r0, r1, r2.
        assert_eq!(
            operation(&[0x8a, 0x43]),
            Operation::And {
                register: 2,
                a: 2,
                b: 1,
                complement: true
            }
        );
        assert_eq!(
            operation(&[0x0a, 0x40]),
            Operation::And {
                register: 2,
                a: 2,
                b: 1,
                complement: false
            }
        );
        assert_eq!(
            operation(&[0x21, 0xea, 0x02, 0x00]),
            Operation::And {
                register: 0,
                a: 1,
                b: 2,
                complement: true
            }
        );
        assert_eq!(
            operation(&[0x01, 0xea, 0x02, 0x00]),
            Operation::And {
                register: 0,
                a: 1,
                b: 2,
                complement: false
            }
        );
        // ands.w r0, r1, r2, lsl #2 works on a shifted register, which
        // isn't followed, but for add.w r0, r3, r0, lsl #2 and sub.w r2,
        // r3, r1, lsl #2; add.w r0, r3, r0, lsr #2 isn't.
        assert_eq!(operation(&[0x11, 0xea, 0x82, 0x00]), Operation::Nothing);
        assert_eq!(
            operation(&[0x03, 0xeb, 0x80, 0x00]),
            Operation::AddShifted {
                register: 0,
                a: 3,
                b: 0,
                shift: 2,
                subtract: false
            }
        );
        assert_eq!(
            operation(&[0xa3, 0xeb, 0x81, 0x02]),
            Operation::AddShifted {
                register: 2,
                a: 3,
                b: 1,
                shift: 2,
                subtract: true
            }
        );
        assert_eq!(operation(&[0x03, 0xeb, 0x90, 0x00]), Operation::Nothing);
        // negs r0, r1
        assert_eq!(
            operation(&[0x48, 0x42]),
            Operation::Negate {
                register: 0,
                source: 1
            }
        );
        // Inside an `it` block, the 16-bit ands, bics, subs, lsrs and asrs
        // leave the flags as they are.
        for bytes in [
            [0x0a, 0x40],
            [0x8a, 0x43],
            [0x12, 0x1a],
            [0x92, 0x08],
            [0xc1, 0x17],
        ] {
            assert!(!effect(0, &bytes).sets_flags);
        }
        // cmp r0, r1, cmp r8, r1 and cmp.w r0, r1 set the flags and write
        // nothing.
        for bytes in [&[0x88, 0x42][..], &[0x88, 0x45], &[0xb0, 0xeb, 0x01, 0x0f]] {
            let compare = effect(0, bytes);
            assert!(compare.sets_flags);
            assert_eq!(compare.writes, 0);
        }
        assert_eq!(
            operation(&[0x88, 0x42]),
            Operation::CompareRegisters { a: 0, b: 1 }
        );
        assert_eq!(
            operation(&[0x88, 0x45]),
            Operation::CompareRegisters { a: 8, b: 1 }
        );
        assert_eq!(
            operation(&[0xb0, 0xeb, 0x01, 0x0f]),
            Operation::CompareRegisters { a: 0, b: 1 }
        );
        // cmp.w r0, r1, lsl #2 compares with a shifted register, which
        // isn't followed.
        assert_eq!(operation(&[0xb0, 0xeb, 0x81, 0x0f]), Operation::Nothing);
    }

    #[test]
    fn a_guard_on_a_length_and_a_test_of_two_addresses() {
        // GCC -Os guards a length: subs r1, r0, #3; cmp r1, r3; it hi;
        // movhi r2, #0.
        let guard: Vec<Effect> = [[0xc1, 0x1e], [0x99, 0x42], [0x88, 0xbf], [0x00, 0x22]]
            .iter()
            .map(|bytes| effect(0, bytes))
            .collect();
        assert_eq!(
            guard[0].operation,
            Operation::AddConstant {
                register: 1,
                source: 0,
                amount: 3u32.wrapping_neg()
            }
        );
        assert_eq!(
            guard[1].operation,
            Operation::CompareRegisters { a: 1, b: 3 }
        );
        assert_eq!(
            guard[2].flow,
            Flow::It {
                firstcond: 0x8,
                mask: 0x8
            }
        );
        assert_eq!(
            guard[3].operation,
            Operation::Constant {
                register: 2,
                value: 0
            }
        );
        // An empty section's loop is skipped: cmp r0, r1; bcs past it.
        assert_eq!(
            operation(&[0x88, 0x42]),
            Operation::CompareRegisters { a: 0, b: 1 }
        );
        assert_eq!(
            effect(10, &[0x00, 0xd2]).flow,
            Flow::Branch {
                target: 14,
                when: When::Flags(2)
            }
        );
    }

    #[test]
    fn floating_point_instructions_leave_the_core_registers() {
        // vldr s15, [r3]: reads r3. vmul.f32 s0, s0, s1: nothing.
        let load = effect(0, &[0xd3, 0xed, 0x00, 0x7a]);
        assert_eq!((load.reads, load.writes), (bit(3), 0));
        assert_eq!(effect(0, &[0x20, 0xee, 0x20, 0x0a]).writes, 0);
        // vmov r0, s15 writes r0; vmrs APSR_nzcv, fpscr sets the flags.
        assert_eq!(effect(0, &[0x17, 0xee, 0x90, 0x0a]).writes, bit(0));
        assert!(effect(0, &[0xf1, 0xee, 0x10, 0xfa]).sets_flags);
        // vpush {d8}: writes sp.
        assert_eq!(effect(0, &[0x2d, 0xed, 0x02, 0x8b]).writes, bit(SP));
        // Half precision: vmov.f16 r0, s0 writes r0, vmul.f16 s0, s16, s0
        // nothing, and vstr.16 s0, [sp, #4] stores two bytes there.
        assert_eq!(effect(0, &[0x10, 0xee, 0x10, 0x09]).writes, bit(0));
        assert_eq!(effect(0, &[0x28, 0xee, 0x00, 0x09]).writes, 0);
        assert_eq!(
            operation(&[0x8d, 0xed, 0x02, 0x09]),
            Operation::Store {
                register: None,
                base: SP,
                offset: Some(4),
                size: 2
            }
        );
        // vldr s15, [r7, #4] and vldr.16 s15, [r7, #6] load from one word.
        let vldr = |offset| Operation::Load {
            register: None,
            base: 7,
            offset: Some(offset),
        };
        assert_eq!(operation(&[0xd7, 0xed, 0x01, 0x7a]), vldr(4));
        assert_eq!(operation(&[0xd7, 0xed, 0x03, 0x79]), vldr(6));
        // vlstm sp and vlldm sp save the registers to the stack and load
        // them back, and leave sp as it is.
        let vlstm = effect(0, &[0x2d, 0xec, 0x00, 0x0a]);
        assert_eq!((vlstm.reads, vlstm.writes), (bit(SP), 0));
        assert_eq!(
            vlstm.operation,
            Operation::Store {
                register: None,
                base: SP,
                offset: Some(0),
                size: 0x88
            }
        );
        let vlldm = effect(0, &[0x3d, 0xec, 0x00, 0x0a]);
        assert_eq!((vlldm.reads, vlldm.writes), (bit(SP), 0));
        assert_eq!(
            vlldm.operation,
            Operation::Load {
                register: None,
                base: SP,
                offset: Some(0)
            }
        );
        // Armv8.1-M's system registers: vstr FPCXTNS, [sp, #-4]! and vldr
        // FPCXTNS, [sp], #4 push and pop a word, and vstr FPSCR, [r0]
        // stores one.
        assert_eq!(
            operation(&[0x6d, 0xed, 0x81, 0xcf]),
            Operation::Push {
                registers: 0,
                size: 4
            }
        );
        assert_eq!(
            operation(&[0xfd, 0xec, 0x81, 0xcf]),
            Operation::Pop {
                registers: 0,
                size: 4
            }
        );
        assert_eq!(
            operation(&[0x80, 0xed, 0x80, 0x2f]),
            Operation::Store {
                register: None,
                base: 0,
                offset: Some(0),
                size: 4
            }
        );
    }

    #[test]
    fn unknown_instructions_are_none() {
        // blx to Arm code, and Armv8.1-M's le.
        assert_eq!(decode(0, 0xf000, 0xe800), None);
        assert_eq!(decode(0, 0xf00f, 0xc001), None);
        // Armv8.1-M's cset r0, ne, lsll r0, r1, #3 and asrl r2, r3, r4,
        // which aren't the orr.w they look like.
        assert_eq!(decode(0, 0xea5f, 0x900f), None);
        assert_eq!(decode(0, 0xea50, 0x01cf), None);
        assert_eq!(decode(0, 0xea52, 0x432d), None);
        // Armv8.1-M's vector instructions, which use coprocessor encodings:
        // vaddv.u32 r4, q0, vmov r0, r1, q0[2], q0[0], vidup.u32 q0, r0, #4
        // and vldrw.32 q0, [r0, #16]!. And the custom instruction cx1 p0,
        // r0, #0.
        assert_eq!(decode(0, 0xfef9, 0x4f00), None);
        assert_eq!(decode(0, 0xec01, 0x0f00), None);
        assert_eq!(decode(0, 0xee21, 0x0fee), None);
        assert_eq!(decode(0, 0xedb0, 0x1f04), None);
        assert_eq!(decode(0, 0xee00, 0x0000), None);
    }
}
