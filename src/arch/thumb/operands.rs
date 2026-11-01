use object::Endianness;

use super::sweep::{Scanner, sweep_instructions};
use crate::arch::{Operands, SplitNumber};

/// The bits of `code`, which starts at `address`, that say where something
/// is (see `arch::operands`): those that the sweep that
/// [`scan`](super::scan) makes finds in the instructions it keeps.
pub(in crate::arch) fn operands(code: &[u8], address: u64, endianness: Endianness) -> Operands {
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

/// The bits that say where something is, as the sweep finds them, before
/// [`operands`] leaves out those of constants that it decoded.
#[derive(Default)]
pub(super) struct Found {
    /// Each distance from an instruction to an address, as (the
    /// instruction's offset, the bits of its first and second 16 bits that
    /// hold the distance, the address).
    pub(super) relative: Vec<(usize, [u16; 2], u64)>,
    /// Each number that a `movw` and a `movt` put together, as (their
    /// offsets, the number).
    pub(super) split: Vec<([usize; 2], u32)>,
}

#[cfg(test)]
mod tests {
    use object::Endianness::Little;

    use super::*;

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
