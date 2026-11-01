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
