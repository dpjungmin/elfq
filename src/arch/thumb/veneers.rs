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
pub(in crate::arch) fn is_veneer(code: &[u8], name: Option<&str>) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
