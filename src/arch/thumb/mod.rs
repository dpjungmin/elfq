//! Thumb code, the instruction set of Cortex-M microcontrollers.
//!
//! Every Thumb instruction is 2 or 4 bytes long, and its first 16 bits say
//! which, so the scanner can step through a function and decode only the
//! instructions that refer to addresses: branches, and the ways code gets an
//! address into a register.
//!
//! Arm code keeps most addresses as constants next to the instructions, in
//! "literal pools", and reads them with loads relative to the program
//! counter. The scanner reports the address each such load reads, and marks
//! the constant as data, so that it doesn't decode it as an instruction when
//! it gets there. It does the same for the jump tables of `tbb` and `tbh`,
//! and reports a jump to each of their targets.
//!
//! This sweep reads each instruction on its own. Then the `values` module
//! follows the addresses that code puts in registers, using what `decode`
//! says each instruction does with them, to find where a branch through a
//! register goes.

mod decode;
mod values;

use std::collections::BTreeMap;

use object::{Endian, Endianness};

use super::CodeReferenceKind::{self, *};
use super::{CodeReference, Ending};

// The stack pointer, link register and program counter.
const SP: u16 = 13;
const LR: u16 = 14;
const PC: u16 = 15;

/// The references in the Thumb code `code`, which starts at `address`, in
/// the order of their instructions. The constants and jump tables that the
/// code reads are skipped, not decoded. A branch through a register goes
/// where the code before it put an address there, if it did.
pub(super) fn scan(code: &[u8], address: u64, endianness: Endianness) -> Vec<CodeReference> {
    let mut scanner = Scanner::new(code, address, endianness);
    // Instructions take 2 bytes or more.
    scanner.instructions = Some(Vec::with_capacity(code.len() / 2));
    let mut scanner = sweep(scanner);
    let mut instructions = scanner.instructions.take().unwrap_or_default();
    // The sweep decodes a constant that only a later instruction loads, as
    // in Clang's islands of constants, before it knows the constant is
    // data. Both lists are in order, so one pass over each drops those
    // instructions.
    let mut data = scanner.data.iter().peekable();
    instructions.retain(|instruction| {
        while data
            .next_if(|&(_, &end)| end <= instruction.offset)
            .is_some()
        {}
        data.peek()
            .is_none_or(|&(&start, _)| start >= instruction.offset + instruction.size())
    });
    values::follow(code, address, endianness, &instructions, scanner.references)
}

/// The references that each instruction in `code` makes on its own: like
/// [`scan`], but without following addresses from one instruction to the
/// next, so a branch through a register is always through a pointer.
pub(super) fn scan_instructions(
    code: &[u8],
    address: u64,
    endianness: Endianness,
) -> Vec<CodeReference> {
    sweep(Scanner::new(code, address, endianness)).references
}

/// Decodes the instructions in the scanner's code, skipping the constants
/// and jump tables they read.
fn sweep(mut scanner: Scanner) -> Scanner {
    let mut offset = 0;
    while let Some(first) = scanner.halfword(offset) {
        if let Some(end) = scanner.data_end(offset) {
            offset = end;
            continue;
        }
        let Some(size) = scanner.step(offset, first) else {
            break;
        };
        offset += size;
    }
    scanner
}

/// How many bytes of `code` its instructions and constants take, leaving
/// out the padding after them: `nop`s, zeros, and the `0xd4d4` that lld
/// fills gaps with. A constant that a load reads is never padding.
pub(super) fn code_size(code: &[u8], address: u64, endianness: Endianness) -> usize {
    let mut scanner = Scanner::new(code, address, endianness);
    let mut size = 0;
    let mut offset = 0;
    while let Some(first) = scanner.halfword(offset) {
        if let Some(end) = scanner.data_end(offset) {
            (offset, size) = (end, end);
            continue;
        }
        let Some(step) = scanner.step(offset, first) else {
            return code.len();
        };
        if !is_padding(first, scanner.halfword(offset + 2)) {
            size = offset + step;
        }
        offset += step;
    }
    size.min(code.len())
}

/// How the code that starts at `address` ends: whether its last
/// instructions could run on past its end. Padding after them doesn't
/// count.
pub(super) fn ending(code: &[u8], address: u64, endianness: Endianness) -> Ending {
    let mut scanner = Scanner::new(code, address, endianness);
    let mut ending = Ending::Stops;
    // How many of the next instructions an `it` makes conditional.
    let mut conditional = 0;
    let mut offset = 0;
    while let Some(first) = scanner.halfword(offset) {
        if let Some(end) = scanner.data_end(offset) {
            (offset, ending, conditional) = (end, Ending::Stops, 0);
            continue;
        }
        let Some(step) = scanner.step(offset, first) else {
            // The code ends inside an instruction.
            return Ending::RunsOn;
        };
        let second = scanner.halfword(offset + 2);
        if !is_padding(first, second) {
            ending = match conditional {
                0 => exit(first, second.unwrap_or(0)),
                _ => Ending::RunsOn,
            };
        }
        conditional = match first {
            // it, with a mask that says how many instructions follow.
            _ if first & 0xff00 == 0xbf00 && first & 0xf != 0 => 4 - (first & 0xf).trailing_zeros(),
            _ => conditional.saturating_sub(1),
        };
        offset += step;
    }
    if conditional > 0 {
        return Ending::RunsOn;
    }
    ending
}

/// Whether `code` is one of the veneers that GNU ld and LLD add for
/// Cortex-M. Each jumps to the target; all but LLD's lone `b.w` put the
/// target, or its offset from pc, in a register, or on the stack where
/// they pop pc from:
///
/// ```text
/// GNU ld, Cortex-M3 and up:  ldr.w pc, [pc]; .word target
/// GNU ld, Cortex-M0 and M23: push {r0}; ldr r0, [pc, #8]; mov ip, r0;
///                            pop {r0}; bx ip; nop; .word target
/// GNU ld, --pic-veneer:      push {r0}; ldr r0, [pc, #8]; mov ip, pc;
///                            add ip, r0; pop {r0}; bx ip; .word offset
/// GNU ld, -mpure-code, and LLD, Cortex-M3 and up and M23:
///                            movw ip, #lo; movt ip, #hi; bx ip
/// LLD, --pic-veneer:         movw ip, #lo; movt ip, #hi; add ip, pc; bx ip
/// LLD, Cortex-M0:            push {r0, r1}; ldr r0, [pc, #4];
///                            str r0, [sp, #4]; pop {r0, pc}; .word target
/// LLD, Cortex-M0, execute-only code:
///                            push {r0, r1}; movs r0, #b3; lsls r0, #8;
///                            adds r0, #b2; lsls r0, #8; adds r0, #b1;
///                            lsls r0, #8; adds r0, #b0; str r0, [sp, #4];
///                            pop {r0, pc}
/// LLD, Cortex-M0, -pie:      push {r0}; ldr r0, [pc, #8]; mov ip, r0;
///                            pop {r0}; add pc, ip; nop; .word offset
/// LLD, when the veneer is in reach of the target, though the call isn't:
///                            b.w target
/// ```
///
/// Code that a compiler made, such as `ldr r3, [pc, #0]; bx r3`, is not a
/// veneer: it is a function someone wrote. Clang makes the same code as
/// the `movw` and `movt` veneers for a function that only calls on with
/// `-mexecute-only` and `-mlong-calls`, when its arguments fill r0 to r3,
/// so code like theirs is a veneer only without a `name` from the symbol
/// table, or under one that a linker gives veneers (see
/// [`is_linker_name`]). Compilers make a lone `b.w` all the time, as a
/// tail call, so it is a veneer only under such a name.
pub(super) fn is_veneer(code: &[u8], name: Option<&str>) -> bool {
    VENEERS.iter().any(|veneer| {
        let size = 2 * veneer.instructions.len() + if veneer.word { 4 } else { 0 };
        code.len() == size
            && veneer
                .instructions
                .iter()
                .enumerate()
                .all(|(index, halfword)| {
                    let bits = u16::from_le_bytes([code[2 * index], code[2 * index + 1]]);
                    bits & halfword.mask == halfword.bits
                })
            && match veneer.names {
                Names::Any => true,
                Names::NoneOrLinkers => name.is_none_or(is_linker_name),
                Names::Linkers => name.is_some_and(is_linker_name),
            }
    })
}

/// Whether `name` is one that GNU ld or LLD gives a veneer, such as
/// `__flash_work_veneer` or `__Thumbv7ABSLongThunk_flash_work`.
fn is_linker_name(name: &str) -> bool {
    name.starts_with("__") && (name.ends_with("_veneer") || name.contains("LongThunk_"))
}

/// A veneer that [`is_veneer`] recognizes.
struct Veneer {
    instructions: &'static [Halfword],
    /// Whether a word, the target or its offset from pc, follows them.
    word: bool,
    /// The names that code like it may have and be a veneer.
    names: Names,
}

/// The names a [`Veneer`] may have, from the symbol table.
enum Names {
    /// Any: compilers don't make such code.
    Any,
    /// None, or one that a linker gives veneers: compilers make the same
    /// code for some functions someone wrote.
    NoneOrLinkers,
    /// Only one that a linker gives veneers: compilers make such code all
    /// the time.
    Linkers,
}

/// The veneers that [`is_veneer`] recognizes, in the order of its list.
const VENEERS: [Veneer; 9] = [
    Veneer {
        instructions: &[exactly(0xf85f), exactly(0xf000)],
        word: true,
        names: Names::Any,
    },
    Veneer {
        instructions: &[
            exactly(0xb401),
            exactly(0x4802),
            exactly(0x4684),
            exactly(0xbc01),
            exactly(0x4760),
            exactly(0xbf00),
        ],
        word: true,
        names: Names::Any,
    },
    Veneer {
        instructions: &[
            exactly(0xb401),
            exactly(0x4802),
            exactly(0x46fc),
            exactly(0x4484),
            exactly(0xbc01),
            exactly(0x4760),
        ],
        word: true,
        names: Names::Any,
    },
    Veneer {
        instructions: &[MOVW_IP, TO_IP, MOVT_IP, TO_IP, exactly(0x4760)],
        word: false,
        names: Names::NoneOrLinkers,
    },
    Veneer {
        instructions: &[
            MOVW_IP,
            TO_IP,
            MOVT_IP,
            TO_IP,
            exactly(0x44fc),
            exactly(0x4760),
        ],
        word: false,
        names: Names::NoneOrLinkers,
    },
    Veneer {
        instructions: &[
            exactly(0xb403),
            exactly(0x4801),
            exactly(0x9001),
            exactly(0xbd01),
        ],
        word: true,
        names: Names::Any,
    },
    Veneer {
        instructions: &[
            exactly(0xb403),
            MOVS_R0,
            exactly(0x0200),
            ADDS_R0,
            exactly(0x0200),
            ADDS_R0,
            exactly(0x0200),
            ADDS_R0,
            exactly(0x9001),
            exactly(0xbd01),
        ],
        word: false,
        names: Names::Any,
    },
    Veneer {
        instructions: &[
            exactly(0xb401),
            exactly(0x4802),
            exactly(0x4684),
            exactly(0xbc01),
            exactly(0x44e7),
            exactly(0x46c0),
        ],
        word: true,
        names: Names::Any,
    },
    Veneer {
        instructions: &[
            Halfword {
                bits: 0xf000,
                mask: 0xf800,
            },
            Halfword {
                bits: 0x9000,
                mask: 0xd000,
            },
        ],
        word: false,
        names: Names::Linkers,
    },
];

/// 16 bits of a veneer's instructions: the bits under `mask` are `bits`.
/// The others hold part of the target, which differs from one veneer to
/// the next.
#[derive(Debug, Clone, Copy)]
struct Halfword {
    bits: u16,
    mask: u16,
}

/// 16 bits that are the same in every veneer.
const fn exactly(bits: u16) -> Halfword {
    Halfword { bits, mask: 0xffff }
}

/// The first 16 bits of `movw` and `movt`, and their second 16 bits when
/// they write ip.
const MOVW_IP: Halfword = Halfword {
    bits: 0xf240,
    mask: 0xfbf0,
};
const MOVT_IP: Halfword = Halfword {
    bits: 0xf2c0,
    mask: 0xfbf0,
};
const TO_IP: Halfword = Halfword {
    bits: 0x0c00,
    mask: 0x8f00,
};

/// `movs r0, #imm8` and `adds r0, #imm8`.
const MOVS_R0: Halfword = Halfword {
    bits: 0x2000,
    mask: 0xff00,
};
const ADDS_R0: Halfword = Halfword {
    bits: 0x3000,
    mask: 0xff00,
};

/// How many bytes of padding `code` starts with.
pub(super) fn leading_padding(code: &[u8]) -> usize {
    let halfword = |offset: usize| {
        let bytes = code.get(offset..offset + 2)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    };
    let mut offset = 0;
    while let Some(first) = halfword(offset)
        && is_padding(first, halfword(offset + 2))
    {
        offset += if first == 0xf3af { 4 } else { 2 };
    }
    offset
}

/// Whether the instruction that starts with `first`, followed by `second`,
/// is padding that compilers and linkers put between functions: a `nop`,
/// zeros, or lld's `0xd4d4`.
fn is_padding(first: u16, second: Option<u16>) -> bool {
    is_nop(first, second) || matches!(first, 0x0000 | 0xd4d4)
}

/// Whether the instruction that starts with `first`, followed by `second`,
/// is a `nop`, `mov r8, r8` or `nop.w`, the padding that compilers put
/// inside a function to align a loop.
fn is_nop(first: u16, second: Option<u16>) -> bool {
    matches!(first, 0xbf00 | 0x46c0) || (first, second) == (0xf3af, Some(0x8000))
}

/// Where the processor goes after the instruction made of `first` and, if
/// it is 32 bits long, `second`.
fn exit(first: u16, second: u16) -> Ending {
    let stops = if !is_wide(first) {
        // blx register
        if first & 0xff80 == 0x4780 {
            return Ending::Call;
        }
        // b, bx register, mov pc, add pc, pop {..., pc} and udf
        first & 0xf800 == 0xe000
            || first & 0xff80 == 0x4700
            || first & 0xff87 == 0x4687
            || first & 0xff87 == 0x4487
            || first & 0xff00 == 0xbd00
            || first & 0xff00 == 0xde00
    } else {
        let branch = first & 0xf800 == 0xf000;
        // bl
        if branch && second & 0xd000 == 0xd000 {
            return Ending::Call;
        }
        // b.w, a load into pc, pop.w {..., pc}, tbb and tbh, and udf.w
        (branch && second & 0xd000 == 0x9000)
            || (matches!(first & 0xfff0, 0xf850 | 0xf8d0) && second >> 12 == PC)
            || (first == 0xe8bd && second & 0x8000 != 0)
            || (first == 0xe8df && second & 0xffe0 == 0xf000)
            || (first & 0xfff0 == 0xf7f0 && second & 0xf000 == 0xa000)
    };
    if stops { Ending::Stops } else { Ending::RunsOn }
}

/// One instruction that the sweep decoded.
#[derive(Debug, Clone, Copy)]
struct Instruction {
    /// Where it is, as an offset into the code.
    offset: usize,
    /// Its first 16 bits.
    first: u16,
    /// Its second 16 bits, if it is 32 bits long, or 0.
    second: u16,
}

impl Instruction {
    /// Its size in bytes.
    fn size(&self) -> usize {
        if is_wide(self.first) { 4 } else { 2 }
    }
}

/// Whether the instruction that starts with `first` is 32 bits long: those
/// start with 0b11101, 0b11110 or 0b11111.
fn is_wide(first: u16) -> bool {
    first >> 11 >= 0b11101
}

struct Scanner<'a> {
    code: &'a [u8],
    /// The address of `code[0]`.
    address: u64,
    /// The order of the bytes in constants and jump tables. Instructions
    /// are always little-endian, even in big-endian (BE8) files.
    endianness: Endianness,
    references: Vec<CodeReference>,
    /// Constants and jump tables found so far, as offsets into `code`: the
    /// start of each, mapped to its end.
    data: BTreeMap<usize, usize>,
    /// For each register, the value of its last `movw`, which a `movt`
    /// completes into a 32-bit address.
    low_halves: [Option<u32>; 16],
    /// The last two instructions decoded, as (offset, first halfword,
    /// second halfword or 0), which can say how long a jump table is.
    previous: [Option<(usize, u16, u16)>; 2],
    /// The instructions decoded so far, when they are kept, for following
    /// the registers through them.
    instructions: Option<Vec<Instruction>>,
}

impl<'a> Scanner<'a> {
    fn new(code: &'a [u8], address: u64, endianness: Endianness) -> Self {
        Scanner {
            code,
            address,
            endianness,
            references: Vec::new(),
            data: BTreeMap::new(),
            low_halves: [None; 16],
            previous: [None; 2],
            instructions: None,
        }
    }

    /// Decodes the instruction at `offset` and reports its reference, if
    /// any. Returns the instruction's size.
    fn step(&mut self, offset: usize, first: u16) -> Option<usize> {
        let (kind, size, second) = if is_wide(first) {
            let second = self.halfword(offset + 2)?;
            (self.wide(offset, first, second), 4, second)
        } else {
            (self.narrow(offset, first), 2, 0)
        };
        self.previous = [self.previous[1], Some((offset, first, second))];
        if let Some(instructions) = &mut self.instructions {
            instructions.push(Instruction {
                offset,
                first,
                second,
            });
        }
        if let Some(kind) = kind {
            self.references.push(CodeReference {
                site: self.address + offset as u64,
                kind,
            });
        }
        Some(size)
    }

    /// A 16-bit instruction.
    fn narrow(&mut self, offset: usize, op: u16) -> Option<CodeReferenceKind> {
        let pc = self.pc(offset);
        // b<cond> label. Conditions 0b1110 and 0b1111 are other instructions.
        if op & 0xf000 == 0xd000 && (op >> 8) & 0xf < 0xe {
            return Some(Jump(relative(pc, u32::from(op & 0xff) << 1, 9)));
        }
        // b label
        if op & 0xf800 == 0xe000 {
            return Some(Jump(relative(pc, u32::from(op & 0x7ff) << 1, 12)));
        }
        // cbz, cbnz: always forward.
        if op & 0xf500 == 0xb100 {
            let distance = u64::from((op >> 9) & 1) << 6 | u64::from((op >> 3) & 0x1f) << 1;
            return Some(Jump(pc + distance));
        }
        // bx and blx with a register. `bx lr` returns.
        if op & 0xff00 == 0x4700 {
            let calls = op & 0x80 != 0;
            return match (op >> 3) & 0xf {
                _ if calls => Some(IndirectCall),
                LR => None,
                _ => Some(IndirectJump),
            };
        }
        // mov pc, register. `mov pc, lr` returns.
        if op & 0xff00 == 0x4600 && ((op >> 4) & 8 | op & 7) == PC {
            return ((op >> 3) & 0xf != LR).then_some(IndirectJump);
        }
        // ldr register, [pc, #imm]
        if op & 0xf800 == 0x4800 {
            let value = self.constant(align(pc) + u64::from(op & 0xff) * 4, 4)?;
            return Some(Address(value));
        }
        // adr register, label
        if op & 0xf800 == 0xa000 {
            return Some(Address(
                self.pointed_at(align(pc) + u64::from(op & 0xff) * 4),
            ));
        }
        None
    }

    /// A 32-bit instruction, made of `first` and `second`.
    fn wide(&mut self, offset: usize, first: u16, second: u16) -> Option<CodeReferenceKind> {
        let pc = self.pc(offset);
        let rt = second >> 12;
        // The immediates of plain-binary instructions such as adr and movw,
        // spread over imm4:i:imm3:imm8.
        let imm12 = u32::from((first >> 10) & 1) << 11
            | u32::from((second >> 12) & 7) << 8
            | u32::from(second & 0xff);
        let imm16 = u32::from(first & 0xf) << 12 | imm12;

        // bl, b.w and b<cond>.w
        if first & 0xf800 == 0xf000 && second & 0x8000 != 0 {
            return branch(pc, first, second);
        }
        // movw register, #imm16: maybe the low half of an address.
        if first & 0xfbf0 == 0xf240 && second & 0x8000 == 0 {
            self.low_halves[usize::from((second >> 8) & 0xf)] = Some(imm16);
            return None;
        }
        // movt register, #imm16: the high half, completing the movw.
        if first & 0xfbf0 == 0xf2c0 && second & 0x8000 == 0 {
            let low = self.low_halves[usize::from((second >> 8) & 0xf)].take()?;
            return Some(Address(u64::from(imm16 << 16 | low)));
        }
        // adr.w register, label, before or after the instruction.
        if first & 0xfbff == 0xf2af && second & 0x8000 == 0 {
            let address = align(pc).wrapping_sub(u64::from(imm12)) & 0xffff_ffff;
            return Some(Address(self.pointed_at(address)));
        }
        if first & 0xfbff == 0xf20f && second & 0x8000 == 0 {
            return Some(Address(self.pointed_at(align(pc) + u64::from(imm12))));
        }
        // ldr.w register, [pc, #imm]. Into pc it jumps, as in the veneers
        // that linkers add for calls too far for bl.
        if first & 0xff7f == 0xf85f {
            let value = self.constant(literal(pc, first, u64::from(second & 0xfff)), 4)?;
            return Some(if rt == PC {
                TailJump(value & !1)
            } else {
                Address(value)
            });
        }
        // Loads of other sizes from literal pools hold numbers, not
        // addresses, but they are still data to skip: ldrb, ldrsb, ldrh and
        // ldrsh (into pc these are preload hints), ldrd and vldr.
        if first & 0xfe5f == 0xf81f && rt != PC {
            let size = if first & 0x20 == 0 { 1 } else { 2 };
            self.constant(literal(pc, first, u64::from(second & 0xfff)), size);
            return None;
        }
        if first & 0xff7f == 0xe95f {
            self.constant(literal(pc, first, u64::from(second & 0xff) * 4), 8);
            return None;
        }
        // vldr of a half, a single or a double: its size, and what its
        // offset counts in.
        let vldr = match (second >> 8) & 0xf {
            0b1001 => Some((2, 2)),
            0b1010 => Some((4, 4)),
            0b1011 => Some((8, 4)),
            _ => None,
        };
        if first & 0xff3f == 0xed1f
            && let Some((size, scale)) = vldr
        {
            self.constant(literal(pc, first, u64::from(second & 0xff) * scale), size);
            return None;
        }
        // ldr pc, [register, ...]: a jump through memory, or a return when
        // it pops the stack.
        if (first & 0xfff0 == 0xf8d0 || first & 0xfff0 == 0xf850) && rt == PC {
            return (first & 0xf != SP).then_some(IndirectJump);
        }
        // tbb [pc, register] and tbh [pc, register, lsl #1]: a jump table,
        // whose targets are all in this function.
        if first == 0xe8df && second & 0xffe0 == 0xf000 {
            let entries = self.table_entries(offset, second & 0xf);
            let entry_size = if second & 0x10 == 0 { 1 } else { 2 };
            self.jump_table(offset, entry_size, entries);
        }
        None
    }

    /// How many entries the jump table of a `tbb` or `tbh` on `register`
    /// at `offset` has, when the two instructions before it say so: `cmp
    /// register, #n` and then `bhi`, which leaves n + 1.
    fn table_entries(&self, offset: usize, register: u16) -> Option<usize> {
        let [
            Some((cmp, first, second)),
            Some((bhi, branch, branch_second)),
        ] = self.previous
        else {
            return None;
        };
        let bhi_size = if branch & 0xff00 == 0xd800 {
            2
        } else if branch & 0xfbc0 == 0xf200 && branch_second & 0xd000 == 0x8000 {
            4
        } else {
            return None;
        };
        // cmp and cmp.w, with an immediate below 256.
        let (cmp_size, cmp_register, n) = if first & 0xf800 == 0x2800 {
            (2, (first >> 8) & 7, first & 0xff)
        } else if first & 0xfff0 == 0xf1b0 && second & 0xff00 == 0x0f00 {
            (4, first & 0xf, second & 0xff)
        } else {
            return None;
        };
        (cmp + cmp_size == bhi && bhi + bhi_size == offset && cmp_register == register)
            .then_some(usize::from(n) + 1)
    }

    /// Marks the jump table after the `tbb` or `tbh` at `site` as data, and
    /// reports a jump to each of its targets. Its entries are distances
    /// forward from the table's start, in halfwords. It has `entries`
    /// entries when the code before says so, and otherwise ends where the
    /// nearest target begins.
    fn jump_table(&mut self, site: usize, entry_size: usize, entries: Option<usize>) {
        let start = site + 4;
        let mut end = entries.map_or(self.code.len(), |entries| start + entries * entry_size);
        let mut entry = start;
        let mut targets = Vec::new();
        while entry + entry_size <= end.min(self.code.len()) {
            let distance = match entry_size {
                1 => u16::from(self.code[entry]),
                _ => self.table_entry(entry).unwrap_or(0),
            };
            let target = start + usize::from(distance) * 2;
            if entries.is_none() {
                end = end.min(target);
            }
            targets.push(target);
            entry += entry_size;
        }
        self.data.insert(start, entry.next_multiple_of(2));
        // An entry of 0, as in the byte that pads a table, points at the
        // table itself.
        targets.retain(|&target| target >= entry);
        for target in targets {
            self.references.push(CodeReference {
                site: self.address + site as u64,
                kind: Jump(self.address + target as u64),
            });
        }
    }

    /// Reads the constant of `size` bytes at `address` and marks it as
    /// data. Returns its value if it is a 32-bit word inside this function.
    fn constant(&mut self, address: u64, size: usize) -> Option<u64> {
        let offset = usize::try_from(address.checked_sub(self.address)?).ok()?;
        let bytes = self.code.get(offset..offset.checked_add(size)?)?;
        let end = self.data.entry(offset).or_insert(offset + size);
        *end = (*end).max(offset + size);
        let word: [u8; 4] = bytes.try_into().ok()?;
        Some(u64::from(self.endianness.read_u32(word)))
    }

    /// Marks what an `adr` at `address` points to in this function as data,
    /// and returns `address`. Compilers point `adr` at a `double`, or at a
    /// table of words, so its first 8 bytes are data. An odd address is
    /// Thumb code, as when hand-written assembly takes a function's address.
    fn pointed_at(&mut self, address: u64) -> u64 {
        if address & 1 == 0 {
            self.constant(address, 8);
        }
        address
    }

    /// Where the data at `offset` ends, if `offset` is inside data.
    fn data_end(&self, offset: usize) -> Option<usize> {
        let (_, &end) = self.data.range(..=offset).next_back()?;
        (offset < end).then_some(end)
    }

    /// The 16 bits of an instruction at `offset`.
    fn halfword(&self, offset: usize) -> Option<u16> {
        let bytes = self.code.get(offset..offset + 2)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// The entry of a `tbh` jump table at `offset`, which is data.
    fn table_entry(&self, offset: usize) -> Option<u16> {
        let bytes: [u8; 2] = self.code.get(offset..offset + 2)?.try_into().ok()?;
        Some(self.endianness.read_u16(bytes))
    }

    /// The value of the program counter when the instruction at `offset`
    /// runs: four bytes past the instruction.
    fn pc(&self, offset: usize) -> u64 {
        self.address + offset as u64 + 4
    }
}

/// `bl`, `b.w` or `b<cond>.w`, all of which spread their offset over
/// both halves.
fn branch(pc: u64, first: u16, second: u16) -> Option<CodeReferenceKind> {
    let s = u32::from((first >> 10) & 1);
    let j1 = u32::from((second >> 13) & 1);
    let j2 = u32::from((second >> 11) & 1);
    let imm11 = u32::from(second & 0x7ff) << 1;
    // bl and b.w: 25 bits, where I1 = !(J1 ^ S) and I2 = !(J2 ^ S).
    let long = || {
        let i1 = !(j1 ^ s) & 1;
        let i2 = !(j2 ^ s) & 1;
        let offset = s << 24 | i1 << 23 | i2 << 22 | u32::from(first & 0x3ff) << 12 | imm11;
        relative(pc, offset, 25)
    };
    match second & 0x5000 {
        0x5000 => Some(CodeReferenceKind::Call(long())),
        0x1000 => Some(CodeReferenceKind::TailJump(long())),
        // b<cond>.w: 21 bits. Conditions 0b1110 and 0b1111 are other
        // instructions, such as barriers.
        0x0000 if (first >> 6) & 0xf < 0xe => {
            let offset = s << 20 | j2 << 19 | j1 << 18 | u32::from(first & 0x3f) << 12 | imm11;
            Some(CodeReferenceKind::Jump(relative(pc, offset, 21)))
        }
        // blx to Arm code, which Cortex-M can't run.
        _ => None,
    }
}

/// The address of a constant that a load reads, `offset` bytes after (or,
/// when bit 7 of `first` is clear, before) the word-aligned `pc`.
fn literal(pc: u64, first: u16, offset: u64) -> u64 {
    if first & 0x80 != 0 {
        align(pc) + offset
    } else {
        align(pc).wrapping_sub(offset)
    }
}

/// `pc` plus `offset`, a signed number of `bits` bits.
fn relative(pc: u64, offset: u32, bits: u32) -> u64 {
    let shift = 32 - bits;
    let offset = ((offset << shift) as i32) >> shift;
    pc.wrapping_add_signed(i64::from(offset))
}

/// `pc` rounded down to a multiple of 4, as PC-relative loads use it.
fn align(pc: u64) -> u64 {
    pc & !3
}

#[cfg(test)]
mod tests {
    use object::Endianness::{Big, Little};

    use super::*;

    fn kinds(code: &[u8], address: u64) -> Vec<(u64, CodeReferenceKind)> {
        scan(code, address, Little)
            .into_iter()
            .map(|r| (r.site, r.kind))
            .collect()
    }

    #[test]
    fn calls_and_jumps() {
        // From the Cortex-M C sample:
        //   800010e: f7ff ffd3  bl   80000b8 <filter_tap>
        //   8000184: f000 b808  b.w  8000198 <parse_list>
        //   80000ca: e001       b.n  80000d0
        assert_eq!(
            kinds(&[0xff, 0xf7, 0xd3, 0xff], 0x0800_010e),
            [(0x0800_010e, Call(0x0800_00b8))]
        );
        assert_eq!(
            kinds(&[0x00, 0xf0, 0x08, 0xb8], 0x0800_0184),
            [(0x0800_0184, TailJump(0x0800_0198))]
        );
        assert_eq!(
            kinds(&[0x01, 0xe0], 0x0800_00ca),
            [(0x0800_00ca, Jump(0x0800_00d0))]
        );
        // bx lr returns; blx r3 calls through a register.
        assert_eq!(kinds(&[0x70, 0x47], 0), []);
        assert_eq!(kinds(&[0x98, 0x47], 0), [(0, IndirectCall)]);
    }

    #[test]
    fn jumps_that_can_carry_on_in_another_function() {
        // A conditional b.w stays inside its function; from the C sample:
        //   80002dc: f040 8091  bne.w 8000402 <process_frame+0x13a>
        assert_eq!(
            kinds(&[0x40, 0xf0, 0x91, 0x80], 0x0800_02dc),
            [(0x0800_02dc, Jump(0x0800_0402))]
        );
        // A linker's veneer loads where to jump; from the Cortex-M7 sample:
        //   300009b0: f85f f000  ldr.w pc, [pc]
        //   300009b4: 00000011   .word 0x00000011 <crc32 + 1>
        assert_eq!(
            kinds(
                &[0x5f, 0xf8, 0x00, 0xf0, 0x11, 0x00, 0x00, 0x00],
                0x3000_09b0
            ),
            [(0x3000_09b0, TailJump(0x10))]
        );
    }

    #[test]
    fn veneers_are_recognized_by_their_instructions() {
        // One of each, from the far-call fixtures.
        let veneers: [&[u8]; 9] = [
            // GNU ld for Cortex-M7: ldr.w pc, [pc]; .word 0x3000087d
            &[0x5f, 0xf8, 0x00, 0xf0, 0x7d, 0x08, 0x00, 0x30],
            // GNU ld for Cortex-M0: push {r0}; ldr r0, [pc, #8];
            // mov ip, r0; pop {r0}; bx ip; nop; .word 0x30000069
            &[
                0x01, 0xb4, 0x02, 0x48, 0x84, 0x46, 0x01, 0xbc, 0x60, 0x47, 0x00, 0xbf, 0x69, 0x00,
                0x00, 0x30,
            ],
            // GNU ld --pic-veneer: push {r0}; ldr r0, [pc, #8]; mov ip, pc;
            // add ip, r0; pop {r0}; bx ip; .word 0x30000021
            &[
                0x01, 0xb4, 0x02, 0x48, 0xfc, 0x46, 0x84, 0x44, 0x01, 0xbc, 0x60, 0x47, 0x21, 0x00,
                0x00, 0x30,
            ],
            // GNU ld -mpure-code: movw ip, #0x61; movt ip, #0x3000; bx ip
            &[0x40, 0xf2, 0x61, 0x0c, 0xc3, 0xf2, 0x00, 0x0c, 0x60, 0x47],
            // The same to the ROM: movw ip, #0x1; movt ip, #0x1fff; bx ip
            &[0x40, 0xf2, 0x01, 0x0c, 0xc1, 0xf6, 0xff, 0x7c, 0x60, 0x47],
            // LLD --pic-veneer: movw ip, #0x59; movt ip, #0x3000;
            // add ip, pc; bx ip
            &[
                0x40, 0xf2, 0x59, 0x0c, 0xc3, 0xf2, 0x00, 0x0c, 0xfc, 0x44, 0x60, 0x47,
            ],
            // LLD for Cortex-M0: push {r0, r1}; ldr r0, [pc, #4];
            // str r0, [sp, #4]; pop {r0, pc}; .word 0x300000f9
            &[
                0x03, 0xb4, 0x01, 0x48, 0x01, 0x90, 0x01, 0xbd, 0xf9, 0x00, 0x00, 0x30,
            ],
            // LLD for Cortex-M0 execute-only code: push {r0, r1};
            // movs r0, #0x30; lsls r0, #8; adds r0, #0; lsls r0, #8;
            // adds r0, #1; lsls r0, #8; adds r0, #0x3d; str r0, [sp, #4];
            // pop {r0, pc}
            &[
                0x03, 0xb4, 0x30, 0x20, 0x00, 0x02, 0x00, 0x30, 0x00, 0x02, 0x01, 0x30, 0x00, 0x02,
                0x3d, 0x30, 0x01, 0x90, 0x01, 0xbd,
            ],
            // LLD -pie for Cortex-M0: push {r0}; ldr r0, [pc, #8];
            // mov ip, r0; pop {r0}; add pc, ip; nop; .word 0x300000a5
            &[
                0x01, 0xb4, 0x02, 0x48, 0x84, 0x46, 0x01, 0xbc, 0xe7, 0x44, 0xc0, 0x46, 0xa5, 0x00,
                0x00, 0x30,
            ],
        ];
        for veneer in veneers {
            assert!(is_veneer(veneer, None), "{veneer:02x?}");
            assert!(is_veneer(veneer, Some("__flash_work_veneer")));
            assert!(is_veneer(veneer, Some("__Thumbv7ABSLongThunk_flash_work")));
            // Not the same code in a longer function.
            assert!(!is_veneer(&[veneer, &[0x70, 0x47]].concat(), None));
        }
        // A b.w is one only under a linker's name, as LLD gives it when
        // the veneer is in reach of the target.
        let branch = [0x00, 0xf1, 0x00, 0xb0];
        assert!(is_veneer(&branch, Some("__Thumbv7ABSLongThunk_far_target")));
        assert!(!is_veneer(&branch, Some("ram_forward")));
        assert!(!is_veneer(&branch, None));
        // Nor is a function that a compiler made:
        // ldr r3, [pc, #0]; bx r3; .word 0x30000069
        assert!(!is_veneer(
            &[0x00, 0x4b, 0x18, 0x47, 0x69, 0x00, 0x00, 0x30],
            None
        ));
        // Nor movw and movt of another register: r3, then bx r3.
        assert!(!is_veneer(
            &[0x40, 0xf2, 0x61, 0x03, 0xc3, 0xf2, 0x00, 0x03, 0x18, 0x47],
            None
        ));
        // Clang makes movw ip, #0x59; movt ip, #0x3000; bx ip for a
        // function that only calls on, which is a veneer only under a
        // linker's name. A linker's other veneers are veneers under any.
        let movw = [0x40, 0xf2, 0x59, 0x0c, 0xc3, 0xf2, 0x00, 0x0c, 0x60, 0x47];
        assert!(!is_veneer(&movw, Some("ram_forward")));
        assert!(is_veneer(veneers[0], Some("ram_forward")));
    }

    #[test]
    fn constants_in_literal_pools_are_read_and_skipped() {
        // ldr r2, [pc, #0]; bx lr; then the constant 0x20000024, which
        // would otherwise decode as instructions.
        let code = [0x00, 0x4a, 0x70, 0x47, 0x24, 0x00, 0x00, 0x20];
        assert_eq!(
            kinds(&code, 0x0800_0040),
            [(0x0800_0040, Address(0x2000_0024))]
        );
    }

    #[test]
    fn half_precision_constants_are_skipped() {
        // vldr.16 s2, [pc, #2]; bx lr; then two halves that vldr.16 loads,
        // which would otherwise decode as a bl.
        let code = [0x9f, 0xed, 0x01, 0x19, 0x70, 0x47, 0x00, 0xf0, 0x01, 0xf8];
        assert_eq!(kinds(&code, 0), []);
        assert_eq!(code_size(&code, 0, Little), 10);
    }

    #[test]
    fn movw_and_movt_build_an_address() {
        // movw r0, #0x1234; movt r0, #0x0800
        let code = [0x41, 0xf2, 0x34, 0x20, 0xc0, 0xf6, 0x00, 0x00];
        assert_eq!(kinds(&code, 0), [(4, Address(0x0800_1234))]);
    }

    #[test]
    fn padding_after_the_code_is_left_out() {
        // bx lr, then nop and nop.w, which align what follows.
        assert_eq!(
            code_size(&[0x70, 0x47, 0x00, 0xbf, 0xaf, 0xf3, 0x00, 0x80], 0, Little),
            2
        );
        // b . (an endless loop), then lld's filler.
        assert_eq!(code_size(&[0xfe, 0xe7, 0xd4, 0xd4], 0, Little), 2);
        // ldr r2, [pc, #0]; bx lr; then the constant it loads, which is 0.
        let code = [0x00, 0x4a, 0x70, 0x47, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(code_size(&code, 0x0800_0040, Little), 8);
    }

    #[test]
    fn what_adr_points_to_is_data() {
        // adr r0, 0x4; bx lr; then the double 0.0 it points to, which would
        // otherwise look like padding.
        let code = [0x00, 0xa0, 0x70, 0x47, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(kinds(&code, 0), [(0, Address(4))]);
        assert_eq!(code_size(&code, 0, Little), 12);
    }

    #[test]
    fn how_code_ends() {
        // bx lr returns, then nop pads.
        assert_eq!(ending(&[0x70, 0x47, 0x00, 0xbf], 0, Little), Ending::Stops);
        // bl calls, and movs r0, #1 runs on.
        assert_eq!(
            ending(&[0xff, 0xf7, 0xd3, 0xff], 0x0800_010e, Little),
            Ending::Call
        );
        assert_eq!(ending(&[0x01, 0x20], 0, Little), Ending::RunsOn);
        // The first half of movw r0, #0x1234.
        assert_eq!(ending(&[0x41, 0xf2], 0, Little), Ending::RunsOn);
        // it eq; bxeq lr: the return may not happen. And ite eq; moveq r0,
        // #1: the code ends inside the `it` block.
        assert_eq!(ending(&[0x08, 0xbf, 0x70, 0x47], 0, Little), Ending::RunsOn);
        assert_eq!(ending(&[0x0c, 0xbf, 0x01, 0x20], 0, Little), Ending::RunsOn);
        // ldr r2, [pc, #0]; bx lr; then the constant 1 it loads.
        let code = [0x00, 0x4a, 0x70, 0x47, 0x01, 0x00, 0x00, 0x00];
        assert_eq!(ending(&code, 0, Little), Ending::Stops);
    }

    #[test]
    fn padding_before_the_code_is_counted() {
        // nop, nop.w, then bx lr.
        let code = [0x00, 0xbf, 0xaf, 0xf3, 0x00, 0x80, 0x70, 0x47];
        assert_eq!(leading_padding(&code), 6);
        assert_eq!(leading_padding(&code[6..]), 0);
    }

    #[test]
    fn jump_tables_are_skipped_and_jump_to_their_targets() {
        // tbb [pc, r0], then a table of four byte offsets, then four cases
        // of bx lr. The table ends where the nearest case starts.
        let code = [
            0xdf, 0xe8, 0x00, 0xf0, 0x02, 0x03, 0x04, 0x05, 0x70, 0x47, 0x70, 0x47, 0x70, 0x47,
            0x70, 0x47,
        ];
        let jumps = [8, 10, 12, 14].map(|target| (0, Jump(target)));
        assert_eq!(kinds(&code, 0), jumps);
        // cmp r0, #1; bhi 0xa; tbb [pc, r0], then a table of two entries.
        // The code for other values follows it (movs r0, #5; bx lr), before
        // the two cases (bx lr): cmp and bhi say where the table ends.
        let code = [
            0x01, 0x28, 0x02, 0xd8, 0xdf, 0xe8, 0x00, 0xf0, 0x03, 0x04, 0x05, 0x20, 0x70, 0x47,
            0x70, 0x47, 0x70, 0x47,
        ];
        assert_eq!(
            kinds(&code, 0),
            [(2, Jump(0xa)), (4, Jump(0xe)), (4, Jump(0x10))]
        );
    }

    #[test]
    fn big_endian_files_have_big_endian_constants_and_tables() {
        // In big-endian (BE8) firmware, instructions are still
        // little-endian. ldr r2, [pc, #0]; bx lr; then 0x20000024.
        let code = [0x00, 0x4a, 0x70, 0x47, 0x20, 0x00, 0x00, 0x24];
        let kinds = |code: &[u8]| -> Vec<(u64, CodeReferenceKind)> {
            scan(code, 0, Big)
                .into_iter()
                .map(|r| (r.site, r.kind))
                .collect()
        };
        assert_eq!(kinds(&code), [(0, Address(0x2000_0024))]);
        // tbh [pc, r0, lsl #1], then a table of two halfwords, 2 and 3,
        // then two cases of bx lr.
        let code = [
            0xdf, 0xe8, 0x10, 0xf0, 0x00, 0x02, 0x00, 0x03, 0x70, 0x47, 0x70, 0x47,
        ];
        assert_eq!(kinds(&code), [(0, Jump(8)), (0, Jump(10))]);
    }

    #[test]
    fn an_adr_to_thumb_code_marks_no_data() {
        // adr.w r0, 0xd, the address of Thumb code at 0xc; bx lr; four
        // nops; bl 0x20, which the scan still decodes.
        let code = [
            0x0f, 0xf2, 0x09, 0x00, 0x70, 0x47, 0x00, 0xbf, 0x00, 0xbf, 0x00, 0xbf, 0x00, 0xbf,
            0x00, 0xf0, 0x07, 0xf8,
        ];
        assert_eq!(kinds(&code, 0), [(0, Address(0xd)), (14, Call(0x20))]);
    }

    #[test]
    fn addresses_before_0_are_not_an_error() {
        // adr.w r0 and ldr.w r0 eight bytes before the program counter, in
        // code at address 0. Addresses are 32 bits long, so adr.w wraps.
        let adr = [0xaf, 0xf2, 0x08, 0x00];
        assert_eq!(kinds(&adr, 0), [(0, Address(0xffff_fffc))]);
        assert_eq!(kinds(&[0x5f, 0xf8, 0x08, 0x00], 0), []);
    }
}
