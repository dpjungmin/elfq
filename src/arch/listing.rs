//! A function's code as lines of text, one per instruction, for
//! `elfq FILE browse`. The lines come from the same decoder that `scan`
//! uses, so every reference that the scanner finds is at the start of a
//! line.
//!
//! Only x86-64 code is printed so far, with iced-x86's Intel formatter.
//! Thumb code isn't, yet: the browser then lists only the lines that use
//! another item.

use iced_x86::{
    Decoder, DecoderOptions, FlowControl, Formatter, Instruction, IntelFormatter, OpKind,
};
use object::Architecture;

/// One instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub address: u64,
    /// Such as `call`.
    pub mnemonic: String,
    /// Such as `rsi, [rip+0x1b1]`, or empty.
    pub operands: String,
    /// Where a direct call or jump goes. The operands end with this
    /// address, so that the browser can put a name in its place.
    pub target: Option<u64>,
}

/// The lines of one function's code, which starts at `address`, or `None`
/// when elfq doesn't print this instruction set's code yet.
pub fn listing(architecture: Architecture, code: &[u8], address: u64) -> Option<Vec<Line>> {
    match architecture {
        Architecture::X86_64 => Some(x86_64(code, address)),
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
            target: direct.then(|| instruction.near_branch_target()),
        });
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each line as `address mnemonic operands`.
    fn text(code: &[u8], address: u64) -> Vec<String> {
        listing(Architecture::X86_64, code, address)
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
            text(&code, 0x1610),
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
        let lines = listing(Architecture::X86_64, &code, 0x1610).unwrap();
        // The calls and jumps, with where they go.
        let targets: Vec<(u64, u64)> = lines
            .iter()
            .filter_map(|line| Some((line.address, line.target?)))
            .collect();
        assert_eq!(targets, [(0x161b, 0x1638), (0x1621, 0x1080)]);
    }

    #[test]
    fn a_cut_instruction_ends_the_listing_without_a_panic() {
        // The first 3 bytes of `call 0x1080`.
        let lines = listing(Architecture::X86_64, &[0x53, 0xe8, 0x5a, 0xfa], 0x1000).unwrap();
        assert_eq!(lines[0].mnemonic, "push");
        assert!(lines.len() <= 2);
    }

    #[test]
    fn thumb_code_isnt_printed_yet() {
        assert_eq!(listing(Architecture::Arm, &[0x70, 0x47], 0x1000), None);
    }
}
