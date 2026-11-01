use object::Endianness;

use super::sweep::{Scanner, Swept};
use super::{Instruction, PC, is_wide};
use crate::arch::Ending;

/// How many bytes of `code` its instructions and constants take, leaving
/// out the padding after them: `nop`s, zeros, and the `0xd4d4` that lld
/// fills gaps with. A constant that a load reads is never padding.
pub(in crate::arch) fn code_size(code: &[u8], address: u64, endianness: Endianness) -> usize {
    let mut scanner = Scanner::new(code, address, endianness);
    let mut size = 0;
    for swept in scanner.sweep() {
        match swept {
            Swept::Data { end } => size = end,
            Swept::Instruction(instruction) if !instruction.is_padding() => {
                size = instruction.offset + instruction.size();
            }
            Swept::Instruction(_) => {}
            Swept::Cut(_) => return code.len(),
        }
    }
    size.min(code.len())
}

/// How the code that starts at `address` ends: whether its last
/// instructions could run on past its end. Padding after them doesn't
/// count.
pub(in crate::arch) fn ending(code: &[u8], address: u64, endianness: Endianness) -> Ending {
    last_instruction(code, address, endianness).map_or(Ending::Stops, |(_, ending)| ending)
}

/// The last instruction of the code that starts at `address`, as an offset
/// into `code`, and how the code ends after it (see [`ending`]). Padding
/// after it doesn't count. `None` when the code ends with data, such as a
/// literal pool, or is all padding: it stops before then.
pub(in crate::arch) fn last_instruction(
    code: &[u8],
    address: u64,
    endianness: Endianness,
) -> Option<(usize, Ending)> {
    let mut scanner = Scanner::new(code, address, endianness);
    let mut last = None;
    // How many of the next instructions an `it` makes conditional.
    let mut conditional = 0;
    for swept in scanner.sweep() {
        let instruction = match swept {
            Swept::Data { .. } => {
                (last, conditional) = (None, 0);
                continue;
            }
            Swept::Instruction(instruction) => instruction,
            // The code ends inside an instruction.
            Swept::Cut(offset) => return Some((offset, Ending::RunsOn)),
        };
        let first = instruction.first;
        if !instruction.is_padding() {
            let ending = match conditional {
                0 => exit(first, instruction.second),
                _ => Ending::RunsOn,
            };
            last = Some((instruction.offset, ending));
        }
        conditional = match first {
            // it, with a mask that says how many instructions follow.
            _ if first & 0xff00 == 0xbf00 && first & 0xf != 0 => 4 - (first & 0xf).trailing_zeros(),
            _ => conditional.saturating_sub(1),
        };
    }
    if conditional > 0 {
        return last.map(|(offset, _)| (offset, Ending::RunsOn));
    }
    last
}

/// How many bytes of padding `code` starts with.
pub(in crate::arch) fn leading_padding(code: &[u8]) -> usize {
    let halfword = |offset: usize| {
        let bytes = code.get(offset..offset + 2)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    };
    let mut offset = 0;
    while let Some(first) = halfword(offset) {
        // A `nop.w` that the end of the code cuts off isn't padding.
        let second = match is_wide(first) {
            true => halfword(offset + 2).unwrap_or(0),
            false => 0,
        };
        let instruction = Instruction {
            offset,
            first,
            second,
        };
        if !instruction.is_padding() {
            break;
        }
        offset += instruction.size();
    }
    offset
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

#[cfg(test)]
mod tests {
    use object::Endianness::Little;

    use super::*;

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
}
