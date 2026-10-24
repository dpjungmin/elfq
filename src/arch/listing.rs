//! A function's code as lines of text, one per instruction, for
//! `elfq FILE browse`. The lines start where elfq's own decoding says
//! instructions start, so every reference that the scanner finds is at
//! the start of a line.
//!
//! x86-64 code is printed with iced-x86's Intel formatter, from the same
//! decoder that `scan` uses. Thumb code is cut into lines by the sweep that
//! `scan` makes (`thumb::layout`), and only then printed, by thumb-asm: a
//! line that it can't decode, or that it reads as an instruction of another
//! size, is printed as `.inst`, and the data among the instructions as
//! `.word`, `.short` or `.byte`. This file is all that knows about
//! thumb-asm, so another printer could take its place.

use std::ops::Range;

use iced_x86::{
    Decoder, DecoderOptions, FlowControl, Formatter, Instruction, IntelFormatter, OpKind,
};
use object::{Architecture, Endian, Endianness};

use super::thumb::{self, Piece};

/// One instruction, or one piece of data among them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub address: u64,
    /// Such as `call`, or `.word` for data.
    pub mnemonic: String,
    /// Such as `rsi, [rip+0x1b1]`, or empty.
    pub operands: String,
    /// What the line refers to, other than registers.
    pub refers: Option<Refers>,
}

/// What a line refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refers {
    /// A direct call or jump to this address. The operands end with it,
    /// so that the browser can put a name in its place.
    Branch(u64),
    /// The address that a Thumb `adr` computes, which the operands end
    /// with.
    Address(u64),
    /// The address and size of the constant that a Thumb `ldr` or `vldr`
    /// reads from a literal pool: 4 bytes, or 8 for a `vldr` of a `d`
    /// register.
    Literal { address: u64, size: usize },
    /// A word of data, with this value.
    Word(u64),
    /// A word of a table of addresses that `ldr.w pc` jumps through, which
    /// holds the Thumb address of this place in the function: a case of a
    /// switch.
    Case(u64),
}

/// The lines of one function's code, which starts at `address`, or `None`
/// when elfq doesn't print this instruction set's code. `endianness` is the
/// file's, for reading data among Thumb code, and `marked` lists the bytes
/// that the file marks as data there, as offsets into `code` (see
/// `Binary::data_in_code`).
pub fn listing(
    architecture: Architecture,
    endianness: Endianness,
    code: &[u8],
    address: u64,
    marked: &[Range<usize>],
) -> Option<Vec<Line>> {
    match architecture {
        Architecture::X86_64 => Some(x86_64(code, address)),
        Architecture::Arm => Some(thumb(code, address, endianness, marked)),
        _ => None,
    }
}

/// x86-64 code in Intel syntax, as iced-x86 writes it: the instructions
/// that objdump -M intel prints, with fewer zeros, sizes only where the
/// operands don't imply them, and iced's own spellings, such as `mov` for
/// objdump's `movabs` and `imul edi, 0xe` for `imul edi, edi, 0xe`:
/// `lea rsi, [rip+0x1b1]`, `call 0x1080`.
fn x86_64(code: &[u8], address: u64) -> Vec<Line> {
    let mut formatter = IntelFormatter::new();
    let options = formatter.options_mut();
    options.set_space_after_operand_separator(true);
    options.set_hex_prefix("0x");
    options.set_hex_suffix("");
    options.set_uppercase_hex(false);
    options.set_leading_zeros(false);
    options.set_branch_leading_zeros(false);
    // `call 0x1080`, not `call near ptr 0x1080`.
    options.set_show_branch_size(false);
    // `[rip+0x1b1]`, as the instruction holds it: the browser names what
    // is there.
    options.set_rip_relative_addresses(true);

    let mut decoder = Decoder::with_ip(64, code, address, DecoderOptions::NONE);
    let mut instruction = Instruction::default();
    let mut lines = Vec::new();
    while decoder.can_decode() {
        decoder.decode_out(&mut instruction);
        let mut mnemonic = String::new();
        formatter.format_mnemonic(&instruction, &mut mnemonic);
        let mut operands = String::new();
        formatter.format_all_operands(&instruction, &mut operands);
        let direct = matches!(
            instruction.flow_control(),
            FlowControl::Call | FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch
        ) && instruction.op0_kind() == OpKind::NearBranch64;
        lines.push(Line {
            address: instruction.ip(),
            mnemonic,
            operands,
            refers: direct.then(|| Refers::Branch(instruction.near_branch_target())),
        });
    }
    lines
}

/// Thumb code, as thumb-asm prints it, in the lines that the sweep cuts
/// it into. Data is printed as objdump prints it: `.word 0x20000444`.
fn thumb(code: &[u8], address: u64, endianness: Endianness, marked: &[Range<usize>]) -> Vec<Line> {
    use thumb_asm::isa::{Decoder, FpReg, Operand, Target};

    let layout = thumb::layout(code, address, endianness, marked);
    let mut lines = Vec::with_capacity(layout.len());
    // The address of the byte at `offset`, as thumb-asm takes it. Thumb
    // code has 32-bit addresses.
    let at = |offset: usize| (address + offset as u64) as u32;
    // One decoder reads each run of instructions, so that it knows which
    // instructions an `it` makes conditional. Armv7E-M's instructions are
    // those of every core that gets text (see
    // `ArmArchitecture::is_cortex_m_up_to_v7`), and thumb-asm reads the
    // others' the same way.
    let mut decoder = Decoder::at(code, 0, at(0)).target(Target::V7EM);
    for (offset, size, piece) in layout {
        let bytes = &code[offset..offset + size];
        let line_address = address + offset as u64;
        if piece != Piece::Instruction {
            lines.push(data_line(line_address, bytes, endianness, piece));
            continue;
        }
        if decoder.pos() != offset {
            decoder = Decoder::at(code, offset, at(offset)).target(Target::V7EM);
        }
        let Some(instruction) = decoder.next().filter(|i| i.len() == size) else {
            lines.push(inst_line(line_address, bytes));
            // Start again after it.
            decoder = Decoder::at(code, offset + size, at(offset + size)).target(Target::V7EM);
            continue;
        };
        let text = instruction.to_string();
        let (mnemonic, mut operands) = match text.split_once(' ') {
            Some((mnemonic, operands)) => (mnemonic, operands.to_string()),
            None => (text.as_str(), String::new()),
        };
        let mnemonic = condition_first(mnemonic, instruction.mnemonic, instruction.cond.is_some());
        let reads_memory = instruction
            .operands
            .as_slice()
            .any(|operand| matches!(operand, Operand::Mem(_)));
        let refers = instruction.branch_target().and_then(|target| {
            let target = u64::from(target);
            if reads_memory {
                // A load from a literal pool, which thumb-asm prints with
                // the address it reads after the brackets:
                // `ldr r3, [pc, #8], 0x30000314`. That word has a line of
                // its own, so the address goes, and an offset of -0, as in
                // a veneer's `ldr.w pc, [pc]`, is written as objdump
                // writes it.
                if let Some(before) = operands.strip_suffix(&format!(", {target:#x}")) {
                    operands = before.replace("[pc, #-0]", "[pc]");
                }
                // The note shows what a load of a word or a double reads.
                // Loads of a byte, a halfword or two registers from a pool
                // are rare, and get none.
                let size = match (instruction.mnemonic, instruction.operands.get(0)) {
                    ("ldr", _) | ("vldr", Some(Operand::FpReg(FpReg::S(_)))) => 4,
                    ("vldr", Some(Operand::FpReg(FpReg::D(_)))) => 8,
                    _ => return None,
                };
                Some(Refers::Literal {
                    address: target,
                    size,
                })
            } else if instruction.mnemonic == "adr" {
                Some(Refers::Address(target))
            } else {
                Some(Refers::Branch(target))
            }
        });
        lines.push(Line {
            address: line_address,
            mnemonic,
            operands,
            refers,
        });
    }
    lines
}

/// A line of data among Thumb code: `.word`, `.short` or `.byte`, by its
/// size, with the value in the file's byte order.
fn data_line(address: u64, bytes: &[u8], endianness: Endianness, piece: Piece) -> Line {
    let (mnemonic, value, digits) = match *bytes {
        [a, b, c, d] => (".word", endianness.read_u32([a, b, c, d]).into(), 8),
        [a, b] => (".short", endianness.read_u16([a, b]).into(), 4),
        _ => (".byte", u64::from(bytes[0]), 2),
    };
    let refers = match piece {
        Piece::Case => Refers::Case(value & !1),
        _ if mnemonic == ".word" => Refers::Word(value),
        _ => {
            return line(
                address,
                mnemonic,
                format!("{value:#0width$x}", width = digits + 2),
            );
        }
    };
    Line {
        refers: Some(refers),
        ..line(address, mnemonic, format!("{value:#010x}"))
    }
}

/// A line for an instruction that thumb-asm can't print, as objdump prints
/// one it doesn't know: `.inst.w 0xf7f0a000`, with the first 16 bits
/// first. Instructions are little-endian, even in big-endian files.
fn inst_line(address: u64, bytes: &[u8]) -> Line {
    let halfword = |index: usize| u16::from_le_bytes([bytes[index], bytes[index + 1]]);
    match bytes.len() {
        4 => {
            let bits = u32::from(halfword(0)) << 16 | u32::from(halfword(2));
            line(address, ".inst.w", format!("{bits:#010x}"))
        }
        _ => line(address, ".inst.n", format!("{:#06x}", halfword(0))),
    }
}

fn line(address: u64, mnemonic: &str, operands: String) -> Line {
    Line {
        address,
        mnemonic: mnemonic.to_string(),
        operands,
        refers: None,
    }
}

/// The mnemonic `printed` of a conditional VFP instruction with the
/// condition before the data types, as objdump and GNU as write it:
/// `vmovgt.f32`. thumb-asm puts it after them, as in `vmov.f32gt`, where
/// `base` is the mnemonic without suffixes (`vmov.f32`).
fn condition_first(printed: &str, base: &str, conditional: bool) -> String {
    match (conditional, base.split_once('.')) {
        (true, Some((name, types))) => match printed.strip_prefix(base) {
            // What thumb-asm adds after the data types: the condition, and
            // `s` or a width before or after it.
            Some(suffixes) => format!("{name}{suffixes}.{types}"),
            None => printed.to_string(),
        },
        _ => printed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each line as `address mnemonic operands`.
    fn text(architecture: Architecture, code: &[u8], address: u64) -> Vec<String> {
        listing(architecture, Endianness::Little, code, address, &[])
            .unwrap()
            .iter()
            .map(|line| format!("{:#x} {} {}", line.address, line.mnemonic, line.operands))
            .collect()
    }

    #[test]
    fn prints_x86_64_code() {
        // log_msg in testdata/bin/c-x86_64.elf, as objdump shows it.
        let code = [
            0xf3, 0x0f, 0x1e, 0xfa, // endbr64
            0x53, // push rbx
            0x80, 0x3f, 0x00, // cmp BYTE PTR [rdi],0x0
            0x48, 0x89, 0xfb, // mov rbx,rdi
            0x74, 0x1b, // je 1638
            0x48, 0x8d, 0x7f, 0x01, // lea rdi,[rdi+0x1]
            0xe8, 0x5a, 0xfa, 0xff, 0xff, // call 1080 <strlen@plt>
            0x48, 0x8d, 0x35, 0x00, 0x01, 0x00, 0x00, // lea rsi,[rip+0x100]
        ];
        assert_eq!(
            text(Architecture::X86_64, &code, 0x1610),
            [
                "0x1610 endbr64 ",
                "0x1614 push rbx",
                "0x1615 cmp byte ptr [rdi], 0",
                "0x1618 mov rbx, rdi",
                "0x161b je 0x1638",
                "0x161d lea rdi, [rdi+1]",
                "0x1621 call 0x1080",
                "0x1626 lea rsi, [rip+0x100]",
            ]
        );
        let lines = listing(Architecture::X86_64, Endianness::Little, &code, 0x1610, &[]).unwrap();
        // The calls and jumps, with where they go.
        let targets: Vec<(u64, Refers)> = lines
            .iter()
            .filter_map(|line| Some((line.address, line.refers?)))
            .collect();
        assert_eq!(
            targets,
            [
                (0x161b, Refers::Branch(0x1638)),
                (0x1621, Refers::Branch(0x1080))
            ]
        );
    }

    #[test]
    fn a_cut_instruction_ends_the_listing_without_a_panic() {
        // The first 3 bytes of `call 0x1080`.
        let code = [0x53, 0xe8, 0x5a, 0xfa];
        let lines = listing(Architecture::X86_64, Endianness::Little, &code, 0x1000, &[]).unwrap();
        assert_eq!(lines[0].mnemonic, "push");
        assert!(lines.len() <= 2);
    }

    #[test]
    fn prints_thumb_code() {
        // app_tick in testdata/bin/c-cortex-m7.elf.
        let code = [
            0x0b, 0x4b, 0x0c, 0x4a, 0x00, 0xfb, 0x03, 0xf3, 0xb2, 0xeb, 0xb3, 0x0f, 0x10, 0xb5,
            0x04, 0x46, 0x05, 0xd2, 0x21, 0x46, 0x00, 0x20, 0xbd, 0xe8, 0x10, 0x40, 0xff, 0xf7,
            0xe5, 0xbf, 0x06, 0x48, 0xff, 0xf7, 0xc0, 0xff, 0x21, 0x46, 0x00, 0x20, 0xbd, 0xe8,
            0x10, 0x40, 0xff, 0xf7, 0xdc, 0xbf, 0x29, 0x5c, 0x8f, 0xc2, 0x28, 0x5c, 0x8f, 0x02,
            0xe8, 0x0d, 0x00, 0x30,
        ];
        // objdump writes mul.w for mul, bcs.n for bhs, and ldmia.w sp!,
        // {r4, lr} for pop.w; the rest is the same.
        assert_eq!(
            text(Architecture::Arm, &code, 0x3000_02e4),
            [
                "0x300002e4 ldr r3, [pc, #44]",
                "0x300002e6 ldr r2, [pc, #48]",
                "0x300002e8 mul r3, r0, r3",
                "0x300002ec cmp.w r2, r3, ror #2",
                "0x300002f0 push {r4, lr}",
                "0x300002f2 mov r4, r0",
                "0x300002f4 bhs 0x30000302",
                "0x300002f6 mov r1, r4",
                "0x300002f8 movs r0, #0",
                "0x300002fa pop.w {r4, lr}",
                "0x300002fe b.w 0x300002cc",
                "0x30000302 ldr r0, [pc, #24]",
                "0x30000304 bl 0x30000288",
                "0x30000308 mov r1, r4",
                "0x3000030a movs r0, #0",
                "0x3000030c pop.w {r4, lr}",
                "0x30000310 b.w 0x300002cc",
                "0x30000314 .word 0xc28f5c29",
                "0x30000318 .word 0x028f5c28",
                "0x3000031c .word 0x30000de8",
            ]
        );
        let lines = listing(
            Architecture::Arm,
            Endianness::Little,
            &code,
            0x3000_02e4,
            &[],
        );
        let refers: Vec<Refers> = lines
            .unwrap()
            .iter()
            .filter_map(|line| line.refers)
            .collect();
        assert_eq!(
            refers[..4],
            [
                Refers::Literal {
                    address: 0x3000_0314,
                    size: 4
                },
                Refers::Literal {
                    address: 0x3000_0318,
                    size: 4
                },
                Refers::Branch(0x3000_0302),
                Refers::Branch(0x3000_02cc),
            ]
        );
        assert_eq!(refers[refers.len() - 1], Refers::Word(0x3000_0de8));
    }

    #[test]
    fn loads_from_a_literal_pool_say_what_they_read() {
        // ldr r0, [pc, #12]; vldr s15, [pc, #12]; vldr d8, [pc, #12];
        // ldrb r1, [pc, #4]; nop; then the pool: 1.0 as a float at 0x10,
        // and as a double at 0x14.
        let code = [
            0x03, 0x48, 0xdf, 0xed, 0x03, 0x7a, 0x9f, 0xed, 0x03, 0x8b, 0x9f, 0xf8, 0x04, 0x10,
            0x00, 0xbf, 0x00, 0x00, 0x80, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x3f,
        ];
        let lines = listing(Architecture::Arm, Endianness::Little, &code, 0, &[]).unwrap();
        let loads: Vec<(String, Option<Refers>)> = lines
            .iter()
            .map(|line| (format!("{} {}", line.mnemonic, line.operands), line.refers))
            .collect();
        let literal = |address, size| Some(Refers::Literal { address, size });
        assert_eq!(
            loads[..4],
            [
                ("ldr r0, [pc, #12]".to_string(), literal(0x10, 4)),
                ("vldr s15, [pc, #12]".to_string(), literal(0x10, 4)),
                ("vldr d8, [pc, #12]".to_string(), literal(0x14, 8)),
                ("ldrb r1, [pc, #4]".to_string(), None),
            ]
        );
    }

    #[test]
    fn conditional_vfp_instructions_put_the_condition_first() {
        // ite gt; vmovgt.f32 s0, s14; vmovle.f32 s0, s15, as objdump
        // writes them.
        let code = [0xcc, 0xbf, 0xb0, 0xee, 0x47, 0x0a, 0xb0, 0xee, 0x67, 0x0a];
        assert_eq!(
            text(Architecture::Arm, &code, 0),
            [
                "0x0 ite gt",
                "0x2 vmovgt.f32 s0, s14",
                "0x6 vmovle.f32 s0, s15",
            ]
        );
    }

    #[test]
    fn a_table_of_addresses_lists_its_cases() {
        // adr r0, 0xe008; ldr.w pc, [r0, r2, lsl #2]; nop; a table of the
        // Thumb addresses 0xe015, 0xe017 and 0xe019; bx lr three times.
        let code = [
            0x01, 0xa0, 0x50, 0xf8, 0x22, 0xf0, 0x00, 0xbf, 0x15, 0xe0, 0x00, 0x00, 0x17, 0xe0,
            0x00, 0x00, 0x19, 0xe0, 0x00, 0x00, 0x70, 0x47, 0x70, 0x47, 0x70, 0x47,
        ];
        let lines = listing(Architecture::Arm, Endianness::Little, &code, 0xe000, &[]).unwrap();
        let text: Vec<String> = lines
            .iter()
            .map(|line| format!("{:#x} {} {}", line.address, line.mnemonic, line.operands))
            .collect();
        assert_eq!(
            text,
            [
                "0xe000 adr r0, 0xe008",
                "0xe002 ldr.w pc, [r0, r2, lsl #2]",
                "0xe006 nop ",
                "0xe008 .word 0x0000e015",
                "0xe00c .word 0x0000e017",
                "0xe010 .word 0x0000e019",
                "0xe014 bx lr",
                "0xe016 bx lr",
                "0xe018 bx lr",
            ]
        );
        let refers: Vec<Refers> = lines.iter().filter_map(|line| line.refers).collect();
        assert_eq!(
            refers,
            [
                Refers::Address(0xe008),
                Refers::Case(0xe014),
                Refers::Case(0xe016),
                Refers::Case(0xe018),
            ]
        );
    }

    #[test]
    fn what_thumb_asm_cant_print_is_inst() {
        // 0xf8700000, which no instruction is, bx lr, then a word that
        // $d marks as data, which isn't aligned to 4 here.
        let code = [0x70, 0xf8, 0x00, 0x00, 0x70, 0x47, 0x01, 0x02, 0x03, 0x04];
        let marked = [Range { start: 6, end: 10 }];
        let lines = listing(Architecture::Arm, Endianness::Little, &code, 0, &marked).unwrap();
        let text: Vec<String> = lines
            .iter()
            .map(|line| format!("{:#x} {} {}", line.address, line.mnemonic, line.operands))
            .collect();
        assert_eq!(
            text,
            [
                "0x0 .inst.w 0xf8700000",
                "0x4 bx lr",
                "0x6 .short 0x0201",
                "0x8 .short 0x0403",
            ]
        );
    }
}
