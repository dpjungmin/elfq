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
//! and reports a jump to each of their targets, and for the tables of
//! addresses that `ldr.w pc` jumps through.
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
use super::{CodeReference, Ending, Operands, SplitNumber};

// The stack pointer, link register and program counter.
const SP: u16 = 13;
const LR: u16 = 14;
const PC: u16 = 15;

/// The references in the Thumb code `code`, which starts at `address`, in
/// the order of their instructions. The constants and jump tables that the
/// code reads are skipped, not decoded. A branch through a register goes
/// where the code before it put an address there, if it did.
pub(super) fn scan(code: &[u8], address: u64, endianness: Endianness) -> Vec<CodeReference> {
    let (scanner, instructions) = sweep_instructions(Scanner::new(code, address, endianness));
    values::follow(code, address, endianness, &instructions, scanner.references)
}

/// Sweeps the scanner's code, keeping the instructions it decodes. The
/// sweep decodes a constant that only a later instruction loads, as in
/// Clang's islands of constants, before it knows the constant is data, so
/// those instructions are dropped.
fn sweep_instructions(mut scanner: Scanner) -> (Scanner, Vec<Instruction>) {
    // Instructions take 2 bytes or more.
    scanner.instructions = Some(Vec::with_capacity(scanner.code.len() / 2));
    let mut scanner = sweep(scanner);
    let mut instructions = scanner.instructions.take().unwrap_or_default();
    // Both lists are in order, so one pass over each drops them.
    let mut data = scanner.data.iter().peekable();
    instructions.retain(|instruction| {
        while data
            .next_if(|&(_, &end)| end <= instruction.offset)
            .is_some()
        {}
        data.peek()
            .is_none_or(|&(&start, _)| start >= instruction.offset + instruction.size())
    });
    (scanner, instructions)
}

/// What a piece of a function's code is, as [`layout`] cuts it.
#[cfg(feature = "tui")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::arch) enum Piece {
    /// An instruction, 2 or 4 bytes long.
    Instruction,
    /// Data that the code reads, such as a constant in a literal pool or
    /// a jump table of `tbb` or `tbh`, or bytes the file marks as data.
    Data,
    /// A word of a table of addresses that `ldr.w pc` jumps through: the
    /// Thumb address of a case of a switch, in this function.
    Case,
}

/// Where the instructions and the data in `code` are, as the sweep that
/// [`scan`] makes finds them, as (offset, size, piece), in order. Data is
/// cut into words, or halfwords and bytes where a word doesn't fit, as
/// objdump prints it, and so are the bytes that no instruction covers,
/// such as a cut-off instruction at the end. `marked` lists more data, as
/// ranges of offsets, such as the bytes that the file's `$d` symbols mark:
/// the sweep skips them too, so it starts again where they end.
///
/// Without `marked`, this is `scan`'s own sweep, so every reference that
/// `scan` reports starts a piece: an instruction, or a constant that the
/// sweep decoded before a later load showed that it is data. With it, the
/// two sweeps can cut the code differently after a mark, until they meet
/// again: `every_use_that_elfq_finds_starts_a_line_of_code`, in
/// `commands/browse/views/item.rs`, checks on every sample that every use
/// that the graph lists still starts a line.
#[cfg(feature = "tui")]
pub(in crate::arch) fn layout(
    code: &[u8],
    address: u64,
    endianness: Endianness,
    marked: &[std::ops::Range<usize>],
) -> Vec<(usize, usize, Piece)> {
    let mut scanner = Scanner::new(code, address, endianness);
    for range in marked {
        let end = scanner.data.entry(range.start).or_insert(range.end);
        *end = (*end).max(range.end);
    }
    let (scanner, instructions) = sweep_instructions(scanner);
    let mut pieces: Vec<(usize, usize, Piece)> = instructions
        .iter()
        .map(|instruction| (instruction.offset, instruction.size(), Piece::Instruction))
        .collect();
    // The bytes between instructions, in order.
    let mut gaps = Vec::new();
    let mut covered = 0;
    for instruction in &instructions {
        if covered < instruction.offset {
            gaps.push(covered..instruction.offset);
        }
        covered = instruction.offset + instruction.size();
    }
    if covered < code.len() {
        gaps.push(covered..code.len());
    }
    for gap in gaps {
        let mut at = gap.start;
        while at < gap.end {
            let case = scanner
                .address_tables
                .iter()
                .any(|table| table.contains(&at));
            // A word, halfword or byte, as fits before the end of the gap
            // and the start of the next stretch of data, as objdump cuts
            // data at each `$d`.
            let next = scanner
                .data
                .range(at + 1..)
                .next()
                .map_or(gap.end, |(&start, _)| start);
            let aligned = |size: u64| (address + at as u64).is_multiple_of(size);
            let size = match next.min(gap.end) - at {
                left if left >= 4 && aligned(4) => 4,
                left if left >= 2 && aligned(2) => 2,
                _ => 1,
            };
            let piece = if case && size == 4 {
                Piece::Case
            } else {
                Piece::Data
            };
            pieces.push((at, size, piece));
            at += size;
        }
    }
    pieces.sort_unstable_by_key(|&(offset, _, _)| offset);
    pieces
}

/// The bits of `code`, which starts at `address`, that say where something
/// is (see `arch::operands`): those that the sweep that [`scan`] makes
/// finds in the instructions it keeps.
pub(super) fn operands(code: &[u8], address: u64, endianness: Endianness) -> Operands {
    let mut scanner = Scanner::new(code, address, endianness);
    scanner.operands = Some(Found::default());
    let (mut scanner, instructions) = sweep_instructions(scanner);
    let found = scanner.operands.take().unwrap_or_default();
    // Constants that the sweep decoded before a later load showed they are
    // data hold no instructions, nor their operands.
    let kept = |offset: usize| {
        instructions
            .binary_search_by_key(&offset, |instruction| instruction.offset)
            .is_ok()
    };
    let outside = |target: u64| target.wrapping_sub(address) >= code.len() as u64;
    let mut relative = vec![0; code.len()];
    let mut targets = Vec::new();
    for (offset, halfwords, target) in found.relative {
        if kept(offset) && outside(target) {
            set_bits(&mut relative, offset, halfwords);
            targets.push((offset, target));
        }
    }
    targets.sort_unstable();
    let split = found
        .split
        .into_iter()
        .filter(|(offsets, _)| offsets.iter().all(|&offset| kept(offset)))
        .map(|(offsets, value)| SplitNumber {
            value: u64::from(value),
            bits: offsets
                .into_iter()
                .flat_map(|offset| (offset..).zip(MOVW_IMMEDIATE))
                .collect(),
        })
        .collect();
    Operands {
        relative,
        targets,
        split,
    }
}

/// Sets, in `bits`, which holds bits for each byte of the code, the bits
/// of the instruction at `offset` that are set in `halfwords`, its first
/// and second 16 bits, which are stored little-endian.
fn set_bits(bits: &mut [u8], offset: usize, halfwords: [u16; 2]) {
    for (half, mask) in halfwords.into_iter().enumerate() {
        for (byte, mask) in mask.to_le_bytes().into_iter().enumerate() {
            if let Some(bits) = bits.get_mut(offset + 2 * half + byte) {
                *bits |= mask;
            }
        }
    }
}

/// The bits of each of the 4 bytes of `movw` and `movt`, as they are
/// stored, that hold their 16-bit number, spread over imm4, i, imm3 and
/// imm8.
const MOVW_IMMEDIATE: [u8; 4] = [0x0f, 0x04, 0xff, 0x70];

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
    last_instruction(code, address, endianness).map_or(Ending::Stops, |(_, ending)| ending)
}

/// The last instruction of the code that starts at `address`, as an offset
/// into `code`, and how the code ends after it (see [`ending`]). Padding
/// after it doesn't count. `None` when the code ends with data, such as a
/// literal pool, or is all padding: it stops before then.
pub(super) fn last_instruction(
    code: &[u8],
    address: u64,
    endianness: Endianness,
) -> Option<(usize, Ending)> {
    let mut scanner = Scanner::new(code, address, endianness);
    let mut last = None;
    // How many of the next instructions an `it` makes conditional.
    let mut conditional = 0;
    let mut offset = 0;
    while let Some(first) = scanner.halfword(offset) {
        if let Some(end) = scanner.data_end(offset) {
            (offset, last, conditional) = (end, None, 0);
            continue;
        }
        let Some(step) = scanner.step(offset, first) else {
            // The code ends inside an instruction.
            return Some((offset, Ending::RunsOn));
        };
        let second = scanner.halfword(offset + 2);
        if !is_padding(first, second) {
            let ending = match conditional {
                0 => exit(first, second.unwrap_or(0)),
                _ => Ending::RunsOn,
            };
            last = Some((offset, ending));
        }
        conditional = match first {
            // it, with a mask that says how many instructions follow.
            _ if first & 0xff00 == 0xbf00 && first & 0xf != 0 => 4 - (first & 0xf).trailing_zeros(),
            _ => conditional.saturating_sub(1),
        };
        offset += step;
    }
    if conditional > 0 {
        return last.map(|(offset, _)| (offset, Ending::RunsOn));
    }
    last
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
    /// For each register, where its last `movw` is and its value, which a
    /// `movt` completes into a 32-bit address.
    low_halves: [Option<(usize, u32)>; 16],
    /// The last three instructions decoded, the last one last, as
    /// (offset, first halfword, second halfword or 0), which can say how
    /// long a jump table is.
    previous: [Option<(usize, u16, u16)>; 3],
    /// The instructions decoded so far, when they are kept, for following
    /// the registers through them.
    instructions: Option<Vec<Instruction>>,
    /// The tables of addresses found so far, which are data too, as ranges
    /// of offsets: the browser links their words to the cases.
    address_tables: Vec<std::ops::Range<usize>>,
    /// The bits that say where something is, when they are kept, for
    /// [`operands`].
    operands: Option<Found>,
}

/// The bits that say where something is, as the sweep finds them, before
/// [`operands`] leaves out those of constants that it decoded.
#[derive(Default)]
struct Found {
    /// Each distance from an instruction to an address, as (the
    /// instruction's offset, the bits of its first and second 16 bits that
    /// hold the distance, the address).
    relative: Vec<(usize, [u16; 2], u64)>,
    /// Each number that a `movw` and a `movt` put together, as (their
    /// offsets, the number).
    split: Vec<([usize; 2], u32)>,
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
            previous: [None; 3],
            instructions: None,
            address_tables: Vec::new(),
            operands: None,
        }
    }

    /// Notes, when [`operands`] asks, that the bits under `halfwords` of
    /// the instruction at `offset` hold its distance to `target`.
    fn relative(&mut self, offset: usize, halfwords: [u16; 2], target: u64) {
        if let Some(found) = &mut self.operands {
            found.relative.push((offset, halfwords, target));
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
        self.previous = [
            self.previous[1],
            self.previous[2],
            Some((offset, first, second)),
        ];
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
            let target = relative(pc, u32::from(op & 0xff) << 1, 9);
            self.relative(offset, [0x00ff, 0], target);
            return Some(Jump(target));
        }
        // b label
        if op & 0xf800 == 0xe000 {
            let target = relative(pc, u32::from(op & 0x7ff) << 1, 12);
            self.relative(offset, [0x07ff, 0], target);
            return Some(Jump(target));
        }
        // cbz, cbnz: always forward.
        if op & 0xf500 == 0xb100 {
            let distance = u64::from((op >> 9) & 1) << 6 | u64::from((op >> 3) & 0x1f) << 1;
            self.relative(offset, [0x02f8, 0], pc + distance);
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
            let address = align(pc) + u64::from(op & 0xff) * 4;
            self.relative(offset, [0x00ff, 0], address);
            let value = self.constant(address, 4)?;
            return Some(Address(value));
        }
        // adr register, label
        if op & 0xf800 == 0xa000 {
            let address = align(pc) + u64::from(op & 0xff) * 4;
            self.relative(offset, [0x00ff, 0], address);
            return Some(Address(self.pointed_at(address)));
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
            let kind = branch(pc, first, second);
            if let Some(Call(target) | Jump(target) | TailJump(target)) = kind {
                // b<cond>.w has a condition where the others have the top
                // bits of the distance.
                let conditional = second & 0x1000 == 0;
                let halfwords = [if conditional { 0x043f } else { 0x07ff }, 0x2fff];
                self.relative(offset, halfwords, target);
            }
            return kind;
        }
        // movw register, #imm16: maybe the low half of an address.
        if first & 0xfbf0 == 0xf240 && second & 0x8000 == 0 {
            self.low_halves[usize::from((second >> 8) & 0xf)] = Some((offset, imm16));
            return None;
        }
        // movt register, #imm16: the high half, completing the movw.
        if first & 0xfbf0 == 0xf2c0 && second & 0x8000 == 0 {
            let (movw, low) = self.low_halves[usize::from((second >> 8) & 0xf)].take()?;
            let value = imm16 << 16 | low;
            if let Some(found) = &mut self.operands {
                found.split.push(([movw, offset], value));
            }
            return Some(Address(u64::from(value)));
        }
        // adr.w register, label, before or after the instruction: bits 5
        // and 7 say which.
        let adr = [0x04a0, 0x70ff];
        if first & 0xfbff == 0xf2af && second & 0x8000 == 0 {
            let address = align(pc).wrapping_sub(u64::from(imm12)) & 0xffff_ffff;
            self.relative(offset, adr, address);
            return Some(Address(self.pointed_at(address)));
        }
        if first & 0xfbff == 0xf20f && second & 0x8000 == 0 {
            let address = align(pc) + u64::from(imm12);
            self.relative(offset, adr, address);
            return Some(Address(self.pointed_at(address)));
        }
        // Loads from a literal pool: bit 7 says whether the constant is
        // after the instruction or before it, and the last 12 or 8 bits how
        // far.
        //
        // ldr.w register, [pc, #imm]. Into pc it jumps, as in the veneers
        // that linkers add for calls too far for bl.
        if first & 0xff7f == 0xf85f {
            let address = literal(pc, first, u64::from(second & 0xfff));
            self.relative(offset, [0x0080, 0x0fff], address);
            let value = self.constant(address, 4)?;
            return Some(if rt == PC {
                TailJump(value & !1)
            } else {
                Address(value)
            });
        }
        // Loads of other sizes from literal pools hold numbers, not
        // addresses, but they are still data to skip: ldrb, ldrsb, ldrh and
        // ldrsh (into pc these are preload hints), ldrd and vldr.
        if first & 0xfe5f == 0xf81f {
            let address = literal(pc, first, u64::from(second & 0xfff));
            self.relative(offset, [0x0080, 0x0fff], address);
            if rt != PC {
                let size = if first & 0x20 == 0 { 1 } else { 2 };
                self.constant(address, size);
            }
            return None;
        }
        if first & 0xff7f == 0xe95f {
            let address = literal(pc, first, u64::from(second & 0xff) * 4);
            self.relative(offset, [0x0080, 0x00ff], address);
            self.constant(address, 8);
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
            let address = literal(pc, first, u64::from(second & 0xff) * scale);
            self.relative(offset, [0x0080, 0x00ff], address);
            self.constant(address, size);
            return None;
        }
        // ldr pc, [register, ...]: a jump through memory, or a return when
        // it pops the stack.
        if (first & 0xfff0 == 0xf8d0 || first & 0xfff0 == 0xf850) && rt == PC {
            self.address_table(offset, first, second);
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

    /// Marks the table of addresses that `ldr.w pc, [rX, rY, lsl #2]` at
    /// `offset` jumps through as data, when the instruction just before it
    /// is `adr rX, table`. GCC builds a switch this way where `tbb` and `tbh`
    /// can't reach its cases:
    ///
    /// ```text
    /// adr   r0, table
    /// ldr.w pc, [r0, r2, lsl #2]
    /// table: .word case0 + 1, .word case1 + 1, ...
    /// ```
    ///
    /// GCC puts `cmp rY, #n` and `bhi` before the `adr`, which say that the
    /// table has n + 1 words. The table runs on while its words are the
    /// Thumb addresses (odd) of code in this function, up to that many, or
    /// without the `cmp`, up to the first case after it. No jump to the
    /// cases is reported: the code there keeps what reading each
    /// instruction on its own finds, as before.
    fn address_table(&mut self, offset: usize, first: u16, second: u16) {
        // The register form, with lsl #2.
        if first & 0xfff0 != 0xf850 || second & 0x0fc0 != 0 || (second >> 4) & 3 != 2 {
            return;
        }
        let base = first & 0xf;
        let [cmp, bhi, Some((adr, adr_first, adr_second))] = self.previous else {
            return;
        };
        let pc = self.pc(adr);
        // adr and adr.w, forward, into the base register, right before.
        let (table, size) = if adr_first & 0xf800 == 0xa000 && (adr_first >> 8) & 7 == base {
            (align(pc) + u64::from(adr_first & 0xff) * 4, 2)
        } else if adr_first & 0xfbff == 0xf20f
            && adr_second & 0x8000 == 0
            && (adr_second >> 8) & 0xf == base
        {
            let imm12 = u32::from((adr_first >> 10) & 1) << 11
                | u32::from((adr_second >> 12) & 7) << 8
                | u32::from(adr_second & 0xff);
            (align(pc) + u64::from(imm12), 4)
        } else {
            return;
        };
        let Some(start) = table.checked_sub(self.address) else {
            return;
        };
        let Ok(start) = usize::try_from(start) else {
            return;
        };
        if adr + size != offset || start < offset + 4 {
            return;
        }
        let function_end = self.address + self.code.len() as u64;
        // Where the table ends: after its n + 1 words, or else where the
        // first case after it starts, as far as the words read so far say.
        let mut first_case = match Self::entries(cmp, bhi, adr, second & 0xf) {
            Some(entries) => start + entries * 4,
            None => self.code.len(),
        };
        let mut at = start;
        while at + 4 <= first_case {
            let Some(word) = self.code.get(at..at + 4).and_then(|b| b.try_into().ok()) else {
                break;
            };
            let word = u64::from(self.endianness.read_u32(word));
            let case = word & !1;
            if word & 1 == 0 || case < self.address || case >= function_end {
                break;
            }
            // Cases can come before the table, but not inside it.
            let case = (case - self.address) as usize;
            if (start..at + 4).contains(&case) {
                break;
            }
            if case > at {
                first_case = first_case.min(case);
            }
            at += 4;
        }
        if at > start {
            let end = self.data.entry(start).or_insert(at);
            *end = (*end).max(at);
            self.address_tables.push(start..at);
        }
    }

    /// How many entries the jump table of a `tbb` or `tbh` on `register`
    /// at `offset` has, when the two instructions before it say so: `cmp
    /// register, #n` and then `bhi`, which leaves n + 1.
    fn table_entries(&self, offset: usize, register: u16) -> Option<usize> {
        let [_, cmp, bhi] = self.previous;
        Self::entries(cmp, bhi, offset, register)
    }

    /// How many entries a table on `register` has when the instructions
    /// `cmp` and `bhi`, which end right before `next`, are `cmp register,
    /// #n` and `bhi`: n + 1.
    fn entries(
        cmp: Option<(usize, u16, u16)>,
        bhi: Option<(usize, u16, u16)>,
        next: usize,
        register: u16,
    ) -> Option<usize> {
        let (Some((cmp, first, second)), Some((bhi, branch, branch_second))) = (cmp, bhi) else {
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
        (cmp + cmp_size == bhi && bhi + bhi_size == next && cmp_register == register)
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
    fn the_last_instruction_is_before_the_padding() {
        // movs r0, #1; eor.w r3, r3, #0x80000000, as libgcc's
        // __aeabi_dsub runs on into __adddf3; then nop.
        let code = [0x01, 0x20, 0x83, 0xf0, 0x00, 0x43, 0x00, 0xbf];
        assert_eq!(
            last_instruction(&code, 0, Little),
            Some((2, Ending::RunsOn))
        );
        // movs r0, #1; bx lr.
        let code = [0x01, 0x20, 0x70, 0x47];
        assert_eq!(last_instruction(&code, 0, Little), Some((2, Ending::Stops)));
        // movs r0, #1; then the first half of movw r0, #0x1234.
        let code = [0x01, 0x20, 0x41, 0xf2];
        assert_eq!(
            last_instruction(&code, 0, Little),
            Some((2, Ending::RunsOn))
        );
        // ldr r2, [pc, #0]; bx lr; then the constant it loads.
        let code = [0x00, 0x4a, 0x70, 0x47, 0x01, 0x00, 0x00, 0x00];
        assert_eq!(last_instruction(&code, 0, Little), None);
        // Padding alone.
        assert_eq!(last_instruction(&[0x00, 0xbf, 0x00, 0xbf], 0, Little), None);
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
    fn tables_of_addresses_are_skipped() {
        // adr r0, 0xe008; ldr.w pc, [r0, r2, lsl #2]; nop; then a table of
        // three cases, as GCC makes for a switch, and the cases, bx lr. The
        // last word, 0xe019, would otherwise decode as b.n 0xe046.
        let code = [
            0x01, 0xa0, 0x50, 0xf8, 0x22, 0xf0, 0x00, 0xbf, 0x15, 0xe0, 0x00, 0x00, 0x17, 0xe0,
            0x00, 0x00, 0x19, 0xe0, 0x00, 0x00, 0x70, 0x47, 0x70, 0x47, 0x70, 0x47,
        ];
        assert_eq!(
            kinds(&code, 0xe000),
            [(0xe000, Address(0xe008)), (0xe002, IndirectJump)]
        );
        // The same with adr into another register than the load's: the
        // words are instructions.
        let mut other = code;
        other[1] = 0xa1;
        assert!(kinds(&other, 0xe000).contains(&(0xe010, Jump(0xe046))));
    }

    #[test]
    fn a_table_of_addresses_ends_before_its_first_case_after_it() {
        // adr r0, 0xe008; ldr.w pc, [r0, r2, lsl #2]; nop; a table of two
        // words that both hold 0xe011, then that case at 0xe010: b.n 0xe03e,
        // whose first word, 0x0000e015, is also the Thumb address of code
        // in the function. Then bx lr up to 0xe040.
        let mut code = vec![
            0x01, 0xa0, 0x50, 0xf8, 0x22, 0xf0, 0x00, 0xbf, 0x11, 0xe0, 0x00, 0x00, 0x11, 0xe0,
            0x00, 0x00, 0x15, 0xe0, 0x00, 0x00,
        ];
        while code.len() < 0x40 {
            code.extend_from_slice(&[0x70, 0x47]);
        }
        assert_eq!(
            kinds(&code, 0xe000),
            [
                (0xe000, Address(0xe008)),
                (0xe002, IndirectJump),
                (0xe010, Jump(0xe03e))
            ]
        );
    }

    #[test]
    fn a_table_of_addresses_has_as_many_words_as_its_cmp_says() {
        // Code in RAM: a switch on r2 whose two cases, at 0x20004800 and
        // 0x20004802, come before its table, and after the table, code that
        // is no case: ldr r1, [pc, #260]; movs r0, #0; bx r1. Its first
        // word, 0x20004941, is also the Thumb address of code in the
        // function, but cmp r2, #1 says that the table has two words.
        let mut code = vec![
            0x01, 0x20, 0x70, 0x47, // movs r0, #1; bx lr
            0x01, 0x2a, 0xfc, 0xd8, // cmp r2, #1; bhi 0x20004802
            0x01, 0xa0, // adr r0, 0x20004810
            0x50, 0xf8, 0x22, 0xf0, 0x00, 0xbf, // ldr.w pc, [r0, r2, lsl #2]; nop
            0x01, 0x48, 0x00, 0x20, 0x03, 0x48, 0x00, 0x20, // the table
            0x41, 0x49, 0x00, 0x20, 0x08, 0x47, // ldr r1, [pc, #260]; movs r0, #0; bx r1
        ];
        while code.len() < 0x120 {
            code.extend_from_slice(&[0x00, 0xbf]);
        }
        // The word that the ldr reads, then bx lr up to 0x20004950.
        code.extend_from_slice(&[0x01, 0x01, 0x00, 0x08]);
        while code.len() < 0x150 {
            code.extend_from_slice(&[0x70, 0x47]);
        }
        let found = kinds(&code, 0x2000_4800);
        assert!(found.contains(&(0x2000_480a, IndirectJump)), "{found:x?}");
        assert!(
            found.contains(&(0x2000_4818, Address(0x0800_0101))),
            "{found:x?}"
        );
        #[cfg(feature = "tui")]
        {
            let cases: Vec<usize> = layout(&code, 0x2000_4800, Little, &[])
                .into_iter()
                .filter(|&(_, _, piece)| piece == Piece::Case)
                .map(|(offset, _, _)| offset)
                .collect();
            assert_eq!(cases, [0x10, 0x14]);
        }
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

    /// The bits of `code` at `address` that hold a distance to an address
    /// outside it, as `operands` gives them.
    fn relative_bits(code: &[u8], address: u64) -> Vec<u8> {
        operands(code, address, Little).relative
    }

    // Each instruction below is from this code, assembled at 0 with far at
    // 0x2000, each tested on its own, so that what it leads to is outside:
    //
    //    0: f001 fffe  bl     2000 <far>
    //    4: f001 bffc  b.w    2000 <far>
    //    8: f001 87fa  beq.w  2000 <far>
    //    c: d00a       beq.n  24
    //    e: e009       b.n    24
    //   10: b148       cbz    r0, 26
    //   12: 4806       ldr    r0, [pc, #24]  @ (2c <pool>)
    //   14: a105       adr    r1, 2c <pool>
    //   16: f245 6278  movw   r2, #0x5678
    //   1a: f2c1 2234  movt   r2, #0x1234
    //   1e: f8df 300c  ldr.w  r3, [pc, #12]  @ 2c <pool>

    #[test]
    fn bl_and_b_w_hold_a_distance_in_both_halves() {
        assert_eq!(
            relative_bits(&[0x01, 0xf0, 0xfe, 0xff], 0),
            [0xff, 0x07, 0xff, 0x2f]
        );
        assert_eq!(
            relative_bits(&[0x01, 0xf0, 0xfc, 0xbf], 4),
            [0xff, 0x07, 0xff, 0x2f]
        );
    }

    #[test]
    fn b_cond_holds_a_distance_but_not_its_condition() {
        // beq.w keeps its condition in bits 6 to 9 of the first half.
        assert_eq!(
            relative_bits(&[0x01, 0xf0, 0xfa, 0x87], 8),
            [0x3f, 0x04, 0xff, 0x2f]
        );
        // beq.n and b.n
        assert_eq!(relative_bits(&[0x0a, 0xd0], 0xc), [0xff, 0x00]);
        assert_eq!(relative_bits(&[0x09, 0xe0], 0xe), [0xff, 0x07]);
    }

    #[test]
    fn cbz_holds_a_distance_but_not_its_register() {
        assert_eq!(relative_bits(&[0x48, 0xb1], 0x10), [0xf8, 0x02]);
    }

    #[test]
    fn loads_from_a_literal_pool_hold_a_distance() {
        assert_eq!(relative_bits(&[0x06, 0x48], 0x12), [0xff, 0x00]);
        // ldr.w also has whether the constant is before or after it.
        assert_eq!(
            relative_bits(&[0xdf, 0xf8, 0x0c, 0x30], 0x1e),
            [0x80, 0x00, 0xff, 0x0f]
        );
    }

    #[test]
    fn adr_holds_a_distance() {
        assert_eq!(relative_bits(&[0x05, 0xa1], 0x14), [0xff, 0x00]);
        assert_eq!(operands(&[0x05, 0xa1], 0x14, Little).targets, [(0, 0x2c)]);
    }

    #[test]
    fn adr_w_holds_a_distance_and_its_direction() {
        // subw r0, pc, #16 and addw r0, pc, #36: bits 5 and 7 of the first
        // half say which.
        let bits = [0xa0, 0x04, 0xff, 0x70];
        assert_eq!(relative_bits(&[0xaf, 0xf2, 0x10, 0x00], 0x100), bits);
        assert_eq!(relative_bits(&[0x0f, 0xf2, 0x24, 0x00], 0x104), bits);
    }

    #[test]
    fn loads_of_other_sizes_from_a_literal_pool_hold_a_distance() {
        // ldrb.w r4, [pc, #64], with 12 bits of distance, and ldrd r0, r1,
        // [pc, #24] and vldr d0, [pc, #-16], with 8 bits of words.
        assert_eq!(
            relative_bits(&[0x9f, 0xf8, 0x40, 0x40], 0x108),
            [0x80, 0x00, 0xff, 0x0f]
        );
        let imm8 = [0x80, 0x00, 0xff, 0x00];
        assert_eq!(relative_bits(&[0xdf, 0xe9, 0x06, 0x01], 0x10c), imm8);
        assert_eq!(relative_bits(&[0x1f, 0xed, 0x04, 0x0b], 0x110), imm8);
    }

    #[test]
    fn movw_and_movt_hold_a_number() {
        let code = [0x45, 0xf2, 0x78, 0x62, 0xc1, 0xf2, 0x34, 0x22];
        let operands = operands(&code, 0x16, Little);
        assert_eq!(operands.relative, [0; 8]);
        assert_eq!(
            operands.split,
            [SplitNumber {
                value: 0x1234_5678,
                bits: vec![
                    (0, 0x0f),
                    (1, 0x04),
                    (2, 0xff),
                    (3, 0x70),
                    (4, 0x0f),
                    (5, 0x04),
                    (6, 0xff),
                    (7, 0x70)
                ],
            }]
        );
    }

    #[test]
    fn a_distance_to_a_place_inside_the_code_is_left_out() {
        // The code above, up to its literal pool: only the branches to far
        // lead outside it.
        let mut code = vec![
            0x01, 0xf0, 0xfe, 0xff, 0x01, 0xf0, 0xfc, 0xbf, 0x01, 0xf0, 0xfa, 0x87, 0x0a, 0xd0,
            0x09, 0xe0, 0x48, 0xb1, 0x06, 0x48, 0x05, 0xa1, 0x45, 0xf2, 0x78, 0x62, 0xc1, 0xf2,
            0x34, 0x22, 0xdf, 0xf8, 0x0c, 0x30,
        ];
        code.extend([0x00, 0xbf].repeat(5));
        code.extend([0x00, 0x00, 0x00, 0x20]);
        let mut expected = vec![0; code.len()];
        for at in [0, 4] {
            expected[at..at + 4].copy_from_slice(&[0xff, 0x07, 0xff, 0x2f]);
        }
        expected[8..12].copy_from_slice(&[0x3f, 0x04, 0xff, 0x2f]);
        assert_eq!(relative_bits(&code, 0), expected);
    }
}
