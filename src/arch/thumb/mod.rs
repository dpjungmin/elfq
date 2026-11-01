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
//!
//! `startup` walks the code from reset, one way through as the processor
//! runs it, using the same `decode`, to work out what startup code does
//! (see `arch::startup`).

mod bounds;
mod decode;
#[cfg(feature = "tui")]
mod layout;
mod operands;
pub(super) mod startup;
mod sweep;
mod values;
mod veneers;

use object::Endianness;

use super::CodeReference;
use super::CodeReferenceKind;

pub(super) use bounds::{code_size, ending, last_instruction, leading_padding};
#[cfg(feature = "tui")]
pub(in crate::arch) use layout::{Piece, layout};
pub(super) use operands::operands;
pub(super) use sweep::scan_instructions;
pub(super) use veneers::is_veneer;

use sweep::Scanner;

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
    let instructions = scanner.sweep_instructions();
    values::follow(code, address, endianness, &instructions, scanner.references)
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

    /// Whether it is padding that compilers and linkers put between
    /// functions: a `nop` (see [`Instruction::is_nop`]), zeros, or lld's
    /// `0xd4d4`.
    fn is_padding(&self) -> bool {
        self.is_nop() || matches!(self.first, 0x0000 | 0xd4d4)
    }

    /// Whether it is a `nop`, `mov r8, r8` or `nop.w`, the padding that
    /// compilers put inside a function to align a loop.
    fn is_nop(&self) -> bool {
        matches!(self.first, 0xbf00 | 0x46c0) || (self.first, self.second) == (0xf3af, 0x8000)
    }
}

/// Whether the instruction that starts with `first` is 32 bits long: those
/// start with 0b11101, 0b11110 or 0b11111.
fn is_wide(first: u16) -> bool {
    first >> 11 >= 0b11101
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
