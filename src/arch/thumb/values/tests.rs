//! Tests of following the registers, on hand-assembled functions.

use object::Endianness::Little;

use crate::arch::CodeReferenceKind::{self, *};
use crate::arch::thumb::scan;

/// Code made of 16-bit units, as a disassembler shows them: a 32-bit
/// instruction as its first 16 bits, then the second, and a word as
/// its low half, then the high.
fn halfwords(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
}

/// What elfq finds in `code` at 0x8000100.
fn kinds(code: &[u8]) -> Vec<(u64, CodeReferenceKind)> {
    scan(code, 0x0800_0100, Little)
        .into_iter()
        .map(|reference| (reference.site, reference.kind))
        .collect()
}

#[test]
fn a_call_through_a_register_set_to_a_constant_is_a_call() {
    // The address comes from a literal pool, as from GCC's long calls,
    // and isn't also reported as taken, since the code only calls it:
    //   8000100: b510       push  {r4, lr}
    //   8000102: 4b01       ldr   r3, [pc, #4]
    //   8000104: 4798       blx   r3
    //   8000106: bd10       pop   {r4, pc}
    //   8000108: 30000101   .word 0x30000101
    let code = halfwords(&[0xb510, 0x4b01, 0x4798, 0xbd10, 0x0101, 0x3000]);
    assert_eq!(kinds(&code), [(0x0800_0104, Call(0x3000_0100))]);
    // From movw and movt, as with -mpure-code:
    //   8000100: b510       push  {r4, lr}
    //   8000102: f240 1301  movw  r3, #0x101
    //   8000106: f2c3 0300  movt  r3, #0x3000
    //   800010a: 4798       blx   r3
    //   800010c: bd10       pop   {r4, pc}
    let code = halfwords(&[0xb510, 0xf240, 0x1301, 0xf2c3, 0x0300, 0x4798, 0xbd10]);
    assert_eq!(kinds(&code), [(0x0800_010a, Call(0x3000_0100))]);
    // A byte at a time, as on Cortex-M0 with -mpure-code:
    //   8000100: b510       push  {r4, lr}
    //   8000102: 2330       movs  r3, #0x30
    //   8000104: 021b       lsls  r3, r3, #8
    //   8000106: 3300       adds  r3, #0
    //   8000108: 021b       lsls  r3, r3, #8
    //   800010a: 3301       adds  r3, #1
    //   800010c: 021b       lsls  r3, r3, #8
    //   800010e: 333d       adds  r3, #0x3d
    //   8000110: 4798       blx   r3
    //   8000112: bd10       pop   {r4, pc}
    let code = halfwords(&[
        0xb510, 0x2330, 0x021b, 0x3300, 0x021b, 0x3301, 0x021b, 0x333d, 0x4798, 0xbd10,
    ]);
    assert_eq!(kinds(&code), [(0x0800_0110, Call(0x3000_013c))]);
    // As an offset from pc, as in position-independent code. Reading
    // pc at 0x8000104 gives 0x8000108:
    //   8000100: b510       push  {r4, lr}
    //   8000102: 4b02       ldr   r3, [pc, #8]
    //   8000104: 447b       add   r3, pc
    //   8000106: 4798       blx   r3
    //   8000108: bd10       pop   {r4, pc}
    //   800010a: bf00       nop
    //   800010c: 27fffff9   .word 0x27fffff9
    let code = halfwords(&[
        0xb510, 0x4b02, 0x447b, 0x4798, 0xbd10, 0xbf00, 0xfff9, 0x27ff,
    ]);
    assert_eq!(kinds(&code), [(0x0800_0106, Call(0x3000_0100))]);
}

#[test]
fn a_jump_through_a_register_set_to_a_constant_is_a_tail_call() {
    //   8000100: 4b00       ldr   r3, [pc, #0]
    //   8000102: 4718       bx    r3
    //   8000104: 30000101   .word 0x30000101
    let code = halfwords(&[0x4b00, 0x4718, 0x0101, 0x3000]);
    assert_eq!(kinds(&code), [(0x0800_0102, TailJump(0x3000_0100))]);
}

#[test]
fn a_register_that_may_hold_several_addresses_calls_each() {
    // Picked by an `it` block:
    //   8000100: b510       push  {r4, lr}
    //   8000102: 2805       cmp   r0, #5
    //   8000104: bfcc       ite   gt
    //   8000106: 4b02       ldrgt r3, [pc, #8]
    //   8000108: 4b02       ldrle r3, [pc, #8]
    //   800010a: 4798       blx   r3
    //   800010c: bd10       pop   {r4, pc}
    //   800010e: 0000       .short 0x0000
    //   8000110: 30000101   .word 0x30000101
    //   8000114: 30000201   .word 0x30000201
    let code = halfwords(&[
        0xb510, 0x2805, 0xbfcc, 0x4b02, 0x4b02, 0x4798, 0xbd10, 0x0000, 0x0101, 0x3000, 0x0201,
        0x3000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_010a, Call(0x3000_0100)),
            (0x0800_010a, Call(0x3000_0200)),
        ]
    );
    // A pointer loaded from memory, or a constant when it is null: a
    // call to the constant, and one through the pointer.
    //   8000100: b510       push  {r4, lr}
    //   8000102: 6803       ldr   r3, [r0, #0]
    //   8000104: b903       cbnz  r3, 8000108
    //   8000106: 4b01       ldr   r3, [pc, #4]
    //   8000108: 4798       blx   r3
    //   800010a: bd10       pop   {r4, pc}
    //   800010c: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb510, 0x6803, 0xb903, 0x4b01, 0x4798, 0xbd10, 0x0101, 0x3000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0104, Jump(0x0800_0108)),
            (0x0800_0108, Call(0x3000_0100)),
            (0x0800_0108, IndirectCall),
        ]
    );
}

#[test]
fn it_blocks_on_the_same_flags_run_on_the_same_path() {
    // GCC builds far_a | 1 = 0x30000179 over two `it` blocks, and loads
    // from a table where the first block's condition holds:
    //   8000100: b508       push    {r3, lr}
    //   8000102: 2803       cmp     r0, #3
    //   8000104: bf9b       ittet   ls
    //   8000106: f240 22c0  movwls  r2, #0x2c0
    //   800010a: f2c3 0200  movtls  r2, #0x3000
    //   800010e: f240 1379  movwhi  r3, #0x179
    //   8000112: f852 3020  ldrls.w r3, [r2, r0, lsl #2]
    //   8000116: 4608       mov     r0, r1
    //   8000118: bf88       it      hi
    //   800011a: f2c3 0300  movthi  r3, #0x3000
    //   800011e: 4798       blx     r3
    //   8000120: bd08       pop     {r3, pc}
    let code = halfwords(&[
        0xb508, 0x2803, 0xbf9b, 0xf240, 0x22c0, 0xf2c3, 0x0200, 0xf240, 0x1379, 0xf852, 0x3020,
        0x4608, 0xbf88, 0xf2c3, 0x0300, 0x4798, 0xbd08,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_010a, Address(0x3000_02c0)),
            (0x0800_011e, Call(0x3000_0178)),
            (0x0800_011e, IndirectCall),
        ]
    );
    // Flags that change between the blocks join the paths.
    //   8000116: 1c08       adds    r0, r1, #0
    let mut code = code;
    code[0x16..0x18].copy_from_slice(&0x1c08_u16.to_le_bytes());
    assert!(kinds(&code).contains(&(0x0800_011e, Call(0x178))));
}

#[test]
fn a_branch_to_an_even_address_goes_nowhere() {
    // Unoptimized code loads a weak function's address again after
    // testing it. The program doesn't have the function, so it is 0,
    // and Cortex-M cores fault on a branch to an even address.
    //   8000100: b580       push    {r7, lr}
    //   8000102: 4b03       ldr     r3, [pc, #12]
    //   8000104: 2b00       cmp     r3, #0
    //   8000106: d001       beq     800010c
    //   8000108: 4b01       ldr     r3, [pc, #4]
    //   800010a: 4798       blx     r3
    //   800010c: bd80       pop     {r7, pc}
    //   800010e: bf00       nop
    //   8000110: 00000000   .word   0x00000000
    let code = halfwords(&[
        0xb580, 0x4b03, 0x2b00, 0xd001, 0x4b01, 0x4798, 0xbd80, 0xbf00, 0x0000, 0x0000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0102, Address(0)),
            (0x0800_0106, Jump(0x0800_010c)),
            (0x0800_0108, Address(0)),
        ]
    );
}

#[test]
fn a_pointer_loaded_from_memory_stays_a_pointer() {
    // As when calling a routine through a table in a chip's ROM:
    //   8000100: b510       push  {r4, lr}
    //   8000102: 4b02       ldr   r3, [pc, #8]
    //   8000104: 681b       ldr   r3, [r3, #0]
    //   8000106: 4798       blx   r3
    //   8000108: bd10       pop   {r4, pc}
    //   800010a: 0000       .short 0x0000
    //   800010c: 1fff0040   .word 0x1fff0040
    let code = halfwords(&[
        0xb510, 0x4b02, 0x681b, 0x4798, 0xbd10, 0x0000, 0x0040, 0x1fff,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0102, Address(0x1fff_0040)),
            (0x0800_0106, IndirectCall)
        ]
    );
}

#[test]
fn a_call_may_change_the_registers_that_calls_need_not_keep() {
    // The bl may take the address in r3 as an argument. It may change
    // r3, or keep it, as code compiled with GCC's -fipa-ra counts on,
    // so blx r3 may call the address or anything. r5 keeps its
    // address.
    //   8000100: b570       push  {r4, r5, r6, lr}
    //   8000102: 4b03       ldr   r3, [pc, #12]
    //   8000104: 4d03       ldr   r5, [pc, #12]
    //   8000106: f7ff fffb  bl    8000100
    //   800010a: 4798       blx   r3
    //   800010c: 47a8       blx   r5
    //   800010e: bd70       pop   {r4, r5, r6, pc}
    //   8000110: 30000101   .word 0x30000101
    //   8000114: 30000201   .word 0x30000201
    let code = halfwords(&[
        0xb570, 0x4b03, 0x4d03, 0xf7ff, 0xfffb, 0x4798, 0x47a8, 0xbd70, 0x0101, 0x3000, 0x0201,
        0x3000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0102, Address(0x3000_0101)),
            (0x0800_0106, Call(0x0800_0100)),
            (0x0800_010a, Call(0x3000_0100)),
            (0x0800_010a, IndirectCall),
            (0x0800_010c, Call(0x3000_0200)),
        ]
    );
}

#[test]
fn r0_holds_what_a_call_returns() {
    // Clang for `int x = f(); return x ? x : 5;`, with f far away:
    // what the function returns is f's result, not its address.
    //   8000100: b580       push  {r7, lr}
    //   8000102: 4803       ldr   r0, [pc, #12]
    //   8000104: 4780       blx   r0
    //   8000106: 2800       cmp   r0, #0
    //   8000108: bf08       it    eq
    //   800010a: 2005       moveq r0, #5
    //   800010c: bd80       pop   {r7, pc}
    //   800010e: 0000       .short 0x0000
    //   8000110: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb580, 0x4803, 0x4780, 0x2800, 0xbf08, 0x2005, 0xbd80, 0x0000, 0x0101, 0x3000,
    ]);
    assert_eq!(kinds(&code), [(0x0800_0104, Call(0x3000_0100))]);
    // `old = swap(h); old()`: what the blx calls is what the bl
    // returned, not h.
    //   8000100: b510       push  {r4, lr}
    //   8000102: 4803       ldr   r0, [pc, #12]
    //   8000104: f7ff fffc  bl    8000100
    //   8000108: 4603       mov   r3, r0
    //   800010a: 4798       blx   r3
    //   800010c: bd10       pop   {r4, pc}
    //   800010e: 0000       .short 0x0000
    //   8000110: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb510, 0x4803, 0xf7ff, 0xfffc, 0x4603, 0x4798, 0xbd10, 0x0000, 0x0101, 0x3000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0102, Address(0x3000_0101)),
            (0x0800_0104, Call(0x0800_0100)),
            (0x0800_010a, IndirectCall),
        ]
    );
    // GCC's -fipa-ra may keep r0 too, so a branch through it after a
    // call uses what it held:
    //   8000100: b508       push  {r3, lr}
    //   8000102: 4802       ldr   r0, [pc, #8]
    //   8000104: 4780       blx   r0
    //   8000106: 4780       blx   r0
    //   8000108: bd08       pop   {r3, pc}
    //   800010a: 0000       .short 0x0000
    //   800010c: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb508, 0x4802, 0x4780, 0x4780, 0xbd08, 0x0000, 0x0101, 0x3000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0102, Address(0x3000_0101)),
            (0x0800_0104, Call(0x3000_0100)),
            (0x0800_0106, IndirectCall),
        ]
    );
    // Code that sets the high half of what a call returned, as Clang
    // makes for `(read(4) & 0xffff) | 0x20010000`, completes a number
    // elfq can't tell, not the 4 that r0 held before the call:
    //   8000100: b580       push  {r7, lr}
    //   8000102: 2004       movs  r0, #4
    //   8000104: f7ff fffc  bl    8000100
    //   8000108: f2c2 0001  movt  r0, #0x2001
    //   800010c: bd80       pop   {r7, pc}
    let code = halfwords(&[0xb580, 0x2004, 0xf7ff, 0xfffc, 0xf2c2, 0x0001, 0xbd80]);
    assert_eq!(kinds(&code), [(0x0800_0104, Call(0x0800_0100))]);
}

#[test]
fn a_call_on_one_path_leaves_the_other_path_s_addresses() {
    // GCC -Os with -fipa-ra for `switch (event) { ... handler = on_x;
    // ... } if (verbose) log_event(); handler();`, where log_event
    // keeps r0. Where cbz skips the call, r0 still holds one of the
    // five addresses; where it doesn't, it may hold anything.
    //   8000100: b510       push    {r4, lr}
    //   8000102: 2803       cmp     r0, #3
    //   8000104: d814       bhi.n   8000130
    //   8000106: e8df f000  tbb     [pc, r0]
    //   800010a: 0902 0e15  .bytes  2, 9, 21, 14
    //   800010e: 480a       ldr     r0, [pc, #40]
    //   8000110: b109       cbz     r1, 8000116
    //   8000112: f7ff ffed  bl      80000f0
    //   8000116: e8bd 4010  ldmia.w sp!, {r4, lr}
    //   800011a: 4700       bx      r0
    //   800011c: 4b07       ldr     r3, [pc, #28]
    //   800011e: 4808       ldr     r0, [pc, #32]
    //   8000120: 2201       movs    r2, #1
    //   8000122: 605a       str     r2, [r3, #4]
    //   8000124: e7f4       b.n     8000110
    //   8000126: 4b05       ldr     r3, [pc, #20]
    //   8000128: 4806       ldr     r0, [pc, #24]
    //   800012a: 2203       movs    r2, #3
    //   800012c: 605a       str     r2, [r3, #4]
    //   800012e: e7ef       b.n     8000110
    //   8000130: 4805       ldr     r0, [pc, #20]
    //   8000132: e7ed       b.n     8000110
    //   8000134: 4805       ldr     r0, [pc, #20]
    //   8000136: e7eb       b.n     8000110
    //   8000138: .word 0x30000069, 0x20000000, 0x30000079, 0x30000099,
    //                  0x300000a9, 0x30000089
    let code = halfwords(&[
        0xb510, 0x2803, 0xd814, 0xe8df, 0xf000, 0x0902, 0x0e15, 0x480a, 0xb109, 0xf7ff, 0xffed,
        0xe8bd, 0x4010, 0x4700, 0x4b07, 0x4808, 0x2201, 0x605a, 0xe7f4, 0x4b05, 0x4806, 0x2203,
        0x605a, 0xe7ef, 0x4805, 0xe7ed, 0x4805, 0xe7eb, 0x0069, 0x3000, 0x0000, 0x2000, 0x0079,
        0x3000, 0x0099, 0x3000, 0x00a9, 0x3000, 0x0089, 0x3000,
    ]);
    let at_bx = kinds(&code)
        .into_iter()
        .filter(|&(site, _)| site == 0x0800_011a)
        .collect::<Vec<_>>();
    assert_eq!(
        at_bx,
        [
            (0x0800_011a, TailJump(0x3000_0068)),
            (0x0800_011a, TailJump(0x3000_0078)),
            (0x0800_011a, TailJump(0x3000_0088)),
            (0x0800_011a, TailJump(0x3000_0098)),
            (0x0800_011a, TailJump(0x3000_00a8)),
            (0x0800_011a, IndirectJump),
        ]
    );
}

#[test]
fn a_register_kept_across_a_call_is_followed() {
    // GCC -O2 with -fipa-ra, which knows that the function keeps r2:
    //   8000100: b508       push  {r3, lr}
    //   8000102: 4a03       ldr   r2, [pc, #12]
    //   8000104: 4790       blx   r2
    //   8000106: 4603       mov   r3, r0
    //   8000108: 4790       blx   r2
    //   800010a: 4418       add   r0, r3
    //   800010c: bd08       pop   {r3, pc}
    //   800010e: 0000       .short 0x0000
    //   8000110: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb508, 0x4a03, 0x4790, 0x4603, 0x4790, 0x4418, 0xbd08, 0x0000, 0x0101, 0x3000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0104, Call(0x3000_0100)),
            (0x0800_0108, Call(0x3000_0100)),
            (0x0800_0108, IndirectCall),
        ]
    );
}

/// Nine times `cmp r0, #k; bne next; movw r3, #0x101; movt r3,
/// #0x3000; blx r3; b join`, then at join, `str r3, [r1]` if `reads`,
/// and `pop {r3, pc}`.
fn nine_calls(reads: bool) -> Vec<u8> {
    let mut units = vec![0xb508];
    let join: i32 = 1 + 9 * 8;
    for k in 0..9 {
        units.extend([0x2800 | k, 0xd105, 0xf240, 0x1301, 0xf2c3, 0x0300, 0x4798]);
        let offset = join - (units.len() as i32 + 2);
        units.push(0xe000 | (offset as u16 & 0x7ff));
    }
    if reads {
        units.push(0x600b);
    }
    units.push(0xbd08);
    halfwords(&units)
}

#[test]
fn a_kept_address_that_elfq_stops_following_counts_only_if_read() {
    // What it finds, other than the jumps between the cases.
    let found = |reads| {
        let mut found = kinds(&nine_calls(reads));
        found.retain(|(_, kind)| !matches!(kind, Jump(_)));
        found
    };
    // r3 may hold any of the nine addresses at the join: more than elfq
    // follows. Popping r3 doesn't read them.
    let calls: Vec<_> = (0..9)
        .map(|k| (0x0800_010e + 16 * k, Call(0x3000_0100)))
        .collect();
    assert_eq!(found(false), calls);
    // Storing r3 may store any of them.
    let addresses: Vec<_> = (0..9)
        .map(|k| (0x0800_010a + 16 * k, Address(0x3000_0101)))
        .collect();
    let mut both = [calls, addresses].concat();
    both.sort_by_key(|&(site, _)| site);
    assert_eq!(found(true), both);
}

#[test]
fn code_reached_only_through_a_table_leaves_the_function_to_the_sweep() {
    // GCC's switch for Cortex-M0, which jumps to each case through a
    // table of addresses elfq doesn't read; each case joins the
    // default one at the blx:
    //   8000100: b510       push  {r4, lr}
    //   8000102: 2801       cmp   r0, #1
    //   8000104: d807       bhi.n 8000116
    //   8000106: 4b05       ldr   r3, [pc, #20]
    //   8000108: 0080       lsls  r0, r0, #2
    //   800010a: 581b       ldr   r3, [r3, r0]
    //   800010c: 469f       mov   pc, r3
    //   800010e: 4c04       ldr   r4, [pc, #16]
    //   8000110: e002       b.n   8000118
    //   8000112: 4c04       ldr   r4, [pc, #16]
    //   8000114: e000       b.n   8000118
    //   8000116: 4c04       ldr   r4, [pc, #16]
    //   8000118: 47a0       blx   r4
    //   800011a: bd10       pop   {r4, pc}
    //   800011c: 08000200   .word 0x08000200
    //   8000120: 30000101   .word 0x30000101
    //   8000124: 30000201   .word 0x30000201
    //   8000128: 30000301   .word 0x30000301
    let code = halfwords(&[
        0xb510, 0x2801, 0xd807, 0x4b05, 0x0080, 0x581b, 0x469f, 0x4c04, 0xe002, 0x4c04, 0xe000,
        0x4c04, 0x47a0, 0xbd10, 0x0200, 0x0800, 0x0101, 0x3000, 0x0201, 0x3000, 0x0301, 0x3000,
    ]);
    let found = kinds(&code);
    assert!(found.contains(&(0x0800_0118, IndirectCall)), "{found:x?}");
    assert!(found.contains(&(0x0800_010e, Address(0x3000_0101))));
    assert!(!found.contains(&(0x0800_0118, Call(0x3000_0300))));
    // The same with a case laid out after the code that ends the
    // function:
    //   8000100: b510       push  {r4, lr}
    //   8000102: 4c04       ldr   r4, [pc, #16]
    //   8000104: 2800       cmp   r0, #0
    //   8000106: d001       beq.n 800010c
    //   8000108: 680b       ldr   r3, [r1, #0]
    //   800010a: 469f       mov   pc, r3
    //   800010c: 47a0       blx   r4
    //   800010e: bd10       pop   {r4, pc}
    //   8000110: 4c01       ldr   r4, [pc, #4]
    //   8000112: e7fb       b.n   800010c
    //   8000114: 30000101   .word 0x30000101
    //   8000118: 30000201   .word 0x30000201
    let code = halfwords(&[
        0xb510, 0x4c04, 0x2800, 0xd001, 0x680b, 0x469f, 0x47a0, 0xbd10, 0x4c01, 0xe7fb, 0x0101,
        0x3000, 0x0201, 0x3000,
    ]);
    let found = kinds(&code);
    assert!(found.contains(&(0x0800_010c, IndirectCall)), "{found:x?}");
    assert!(!found.contains(&(0x0800_010c, Call(0x3000_0100))));
    // And with one that runs on into the blx through padding:
    //   8000100: b510       push  {r4, lr}
    //   8000102: 4c04       ldr   r4, [pc, #16]
    //   8000104: 2800       cmp   r0, #0
    //   8000106: d003       beq.n 8000110
    //   8000108: 680b       ldr   r3, [r1, #0]
    //   800010a: 469f       mov   pc, r3
    //   800010c: 4c02       ldr   r4, [pc, #8]
    //   800010e: bf00       nop
    //   8000110: 47a0       blx   r4
    //   8000112: bd10       pop   {r4, pc}
    //   8000114: 30000101   .word 0x30000101
    //   8000118: 30000201   .word 0x30000201
    let code = halfwords(&[
        0xb510, 0x4c04, 0x2800, 0xd003, 0x680b, 0x469f, 0x4c02, 0xbf00, 0x47a0, 0xbd10, 0x0101,
        0x3000, 0x0201, 0x3000,
    ]);
    let found = kinds(&code);
    assert!(found.contains(&(0x0800_0110, IndirectCall)), "{found:x?}");
    assert!(!found.contains(&(0x0800_0110, Call(0x3000_0100))));
}

#[test]
fn constants_between_instructions_are_not_cases() {
    // A constant that only a later instruction loads, as in Clang's
    // islands of constants, isn't code that runs on into the blx:
    //   8000100: b510       push  {r4, lr}
    //   8000102: 4c04       ldr   r4, [pc, #16]
    //   8000104: b910       cbnz  r0, 800010c
    //   8000106: e004       b.n   8000112
    //   8000108: 12345678   .word 0x12345678
    //   800010c: f85f 0008  ldr.w r0, [pc, #-8]
    //   8000110: 47a0       blx   r4
    //   8000112: bd10       pop   {r4, pc}
    //   8000114: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb510, 0x4c04, 0xb910, 0xe004, 0x5678, 0x1234, 0xf85f, 0x0008, 0x47a0, 0xbd10, 0x0101,
        0x3000,
    ]);
    let found = kinds(&code);
    assert!(
        found.contains(&(0x0800_0110, Call(0x3000_0100))),
        "{found:x?}"
    );
    assert!(!found.contains(&(0x0800_0110, IndirectCall)));
    // Nor is the end of a table of small numbers that an adr points
    // at, whose high halves are zeros:
    //   8000100: b510       push  {r4, lr}
    //   8000102: 4c07       ldr   r4, [pc, #28]
    //   8000104: a101       adr   r1, 800010c
    //   8000106: f851 0020  ldr.w r0, [r1, r0, lsl #2]
    //   800010a: e007       b.n   800011c
    //   800010c: 00000001   .word 1, 2, 3, 4
    //   800011c: 47a0       blx   r4
    //   800011e: bd10       pop   {r4, pc}
    //   8000120: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb510, 0x4c07, 0xa101, 0xf851, 0x0020, 0xe007, 0x0001, 0x0000, 0x0002, 0x0000, 0x0003,
        0x0000, 0x0004, 0x0000, 0x47a0, 0xbd10, 0x0101, 0x3000,
    ]);
    let found = kinds(&code);
    assert!(
        found.contains(&(0x0800_011c, Call(0x3000_0100))),
        "{found:x?}"
    );
    assert!(!found.contains(&(0x0800_011c, IndirectCall)));
}

#[test]
fn a_non_secure_call_keeps_the_stack() {
    // Clang's call to Non-secure code, which saves the floating-point
    // registers on the stack with vlstm and loads them back with
    // vlldm, around a long call:
    //   8000100: b590       push    {r4, r7, lr}
    //   8000102: 4f07       ldr     r7, [pc, #28]
    //   8000104: e92d 0ff0  push.w  {r4-r11}
    //   8000108: b0a2       sub     sp, #136
    //   800010a: ec2d 0a00  vlstm   sp
    //   800010e: 47a4       blxns   r4
    //   8000110: ec3d 0a00  vlldm   sp
    //   8000114: b022       add     sp, #136
    //   8000116: e8bd 0ff0  pop.w   {r4-r11}
    //   800011a: 47b8       blx     r7
    //   800011c: bd90       pop     {r4, r7, pc}
    //   800011e: bf00       nop
    //   8000120: 30000101   .word   0x30000101
    let code = halfwords(&[
        0xb590, 0x4f07, 0xe92d, 0x0ff0, 0xb0a2, 0xec2d, 0x0a00, 0x47a4, 0xec3d, 0x0a00, 0xb022,
        0xe8bd, 0x0ff0, 0x47b8, 0xbd90, 0xbf00, 0x0101, 0x3000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_010e, IndirectCall),
            (0x0800_011a, Call(0x3000_0100)),
        ]
    );
}

#[test]
fn an_address_passed_on_the_stack_is_used() {
    // The fifth argument to the bl goes on the stack:
    //   8000100: b510       push  {r4, lr}
    //   8000102: b082       sub   sp, #8
    //   8000104: 4c05       ldr   r4, [pc, #20]
    //   8000106: 47a0       blx   r4
    //   8000108: 9400       str   r4, [sp, #0]
    //   800010a: 2304       movs  r3, #4
    //   800010c: 2203       movs  r2, #3
    //   800010e: 2102       movs  r1, #2
    //   8000110: 2001       movs  r0, #1
    //   8000112: f7ff fff5  bl    8000100
    //   8000116: b002       add   sp, #8
    //   8000118: bd10       pop   {r4, pc}
    //   800011a: 0000       .short 0x0000
    //   800011c: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb510, 0xb082, 0x4c05, 0x47a0, 0x9400, 0x2304, 0x2203, 0x2102, 0x2001, 0xf7ff, 0xfff5,
        0xb002, 0xbd10, 0x0000, 0x0101, 0x3000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0104, Address(0x3000_0101)),
            (0x0800_0106, Call(0x3000_0100)),
            (0x0800_0112, Call(0x0800_0100)),
        ]
    );
}

#[test]
fn an_address_stored_on_the_stack_is_passed_on_from_there() {
    // GCC -O2 with -fipa-ra keeps r3 across the call, then passes the
    // address to the next function through a volatile variable:
    //   8000100: b510       push  {r4, lr}
    //   8000102: b082       sub   sp, #8
    //   8000104: 4b04       ldr   r3, [pc, #16]
    //   8000106: 4798       blx   r3
    //   8000108: 9301       str   r3, [sp, #4]
    //   800010a: 9801       ldr   r0, [sp, #4]
    //   800010c: b002       add   sp, #8
    //   800010e: e8bd 4010  ldmia.w sp!, {r4, lr}
    //   8000112: f000 b875  b.w   8000200
    //   8000116: 0000       .short 0x0000
    //   8000118: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb510, 0xb082, 0x4b04, 0x4798, 0x9301, 0x9801, 0xb002, 0xe8bd, 0x4010, 0xf000, 0xb875,
        0x0000, 0x0101, 0x3000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0104, Address(0x3000_0101)),
            (0x0800_0106, Call(0x3000_0100)),
            (0x0800_0112, TailJump(0x0800_0200)),
        ]
    );
    // A tail call that passes the address in the word where its own
    // caller passed an argument, which it read before:
    //   8000100: e92d 4110  stmdb sp!, {r4, r8, lr}
    //   8000104: f8df 8010  ldr.w r8, [pc, #16]
    //   8000108: 9a03       ldr   r2, [sp, #12]
    //   800010a: 47c0       blx   r8
    //   800010c: f8cd 800c  str.w r8, [sp, #12]
    //   8000110: e8bd 4110  ldmia.w sp!, {r4, r8, lr}
    //   8000114: f000 b874  b.w   8000200
    //   8000118: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xe92d, 0x4110, 0xf8df, 0x8010, 0x9a03, 0x47c0, 0xf8cd, 0x800c, 0xe8bd, 0x4110, 0xf000,
        0xb874, 0x0101, 0x3000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0104, Address(0x3000_0101)),
            (0x0800_010a, Call(0x3000_0100)),
            (0x0800_0114, TailJump(0x0800_0200)),
        ]
    );
}

#[test]
fn a_float_load_from_the_stack_reads_one_word() {
    // GCC -O0 keeps the address and a float on the stack; the vldr
    // reads only the float:
    //   8000100: b580       push  {r7, lr}
    //   8000102: b084       sub   sp, #16
    //   8000104: af00       add   r7, sp, #0
    //   8000106: 4b04       ldr   r3, [pc, #16]
    //   8000108: 60fb       str   r3, [r7, #12]
    //   800010a: edd7 7a01  vldr  s15, [r7, #4]
    //   800010e: 68fb       ldr   r3, [r7, #12]
    //   8000110: 4798       blx   r3
    //   8000112: b004       add   sp, #16
    //   8000114: bd80       pop   {r7, pc}
    //   8000116: bf00       nop
    //   8000118: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb580, 0xb084, 0xaf00, 0x4b04, 0x60fb, 0xedd7, 0x7a01, 0x68fb, 0x4798, 0xb004, 0xbd80,
        0xbf00, 0x0101, 0x3000,
    ]);
    assert_eq!(kinds(&code), [(0x0800_0110, Call(0x3000_0100))]);
}

#[test]
fn a_load_through_an_index_into_the_stack_reads_it() {
    // GCC for Cortex-M0, for `return table[i]` on a table on the
    // stack, with sp in the second register:
    //   8000100: b510       push  {r4, lr}
    //   8000102: b082       sub   sp, #8
    //   8000104: 4b04       ldr   r3, [pc, #16]
    //   8000106: 9300       str   r3, [sp, #0]
    //   8000108: 9b00       ldr   r3, [sp, #0]
    //   800010a: 4798       blx   r3
    //   800010c: 466b       mov   r3, sp
    //   800010e: 00a4       lsls  r4, r4, #2
    //   8000110: 58e0       ldr   r0, [r4, r3]
    //   8000112: b002       add   sp, #8
    //   8000114: bd10       pop   {r4, pc}
    //   8000116: 0000       .short 0x0000
    //   8000118: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb510, 0xb082, 0x4b04, 0x9300, 0x9b00, 0x4798, 0x466b, 0x00a4, 0x58e0, 0xb002, 0xbd10,
        0x0000, 0x0101, 0x3000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0104, Address(0x3000_0101)),
            (0x0800_010a, Call(0x3000_0100)),
        ]
    );
}

#[test]
fn a_store_through_an_index_into_the_stack_forgets_it() {
    // GCC for Cortex-M0, for table[i] = h on a table on the stack,
    // with sp in the second register:
    //   8000100: b510       push  {r4, lr}
    //   8000102: b084       sub   sp, #16
    //   8000104: 4b04       ldr   r3, [pc, #16]
    //   8000106: 9300       str   r3, [sp, #0]
    //   8000108: 466b       mov   r3, sp
    //   800010a: 0080       lsls  r0, r0, #2
    //   800010c: 50c1       str   r1, [r0, r3]
    //   800010e: 9b00       ldr   r3, [sp, #0]
    //   8000110: 4798       blx   r3
    //   8000112: b004       add   sp, #16
    //   8000114: bd10       pop   {r4, pc}
    //   8000116: 0000       .short 0x0000
    //   8000118: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb510, 0xb084, 0x4b04, 0x9300, 0x466b, 0x0080, 0x50c1, 0x9b00, 0x4798, 0xb004, 0xbd10,
        0x0000, 0x0101, 0x3000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0104, Address(0x3000_0101)),
            (0x0800_0110, IndirectCall),
        ]
    );
}

#[test]
fn some_branches_go_to_even_addresses() {
    // blxns calls Non-secure code at an even address:
    //   8000100: b510       push  {r4, lr}
    //   8000102: f44f 1400  mov.w r4, #0x200000
    //   8000106: 47a4       blxns r4
    //   8000108: bd10       pop   {r4, pc}
    let code = halfwords(&[0xb510, 0xf44f, 0x1400, 0x47a4, 0xbd10]);
    assert_eq!(kinds(&code), [(0x0800_0106, Call(0x0020_0000))]);
    // mov pc ignores the low bit:
    //   8000100: 4801       ldr   r0, [pc, #4]
    //   8000102: 4687       mov   pc, r0
    //   8000104: 0000       .short 0x0000
    //   8000108: 30000028   .word 0x30000028
    let code = halfwords(&[0x4801, 0x4687, 0x0000, 0x0000, 0x0028, 0x3000]);
    assert_eq!(kinds(&code), [(0x0800_0102, TailJump(0x3000_0028))]);
}

#[test]
fn addresses_are_followed_around_loops_and_past_returns() {
    //   8000100: b570       push  {r4, r5, r6, lr}
    //   8000102: 4c03       ldr   r4, [pc, #12]
    //   8000104: 260a       movs  r6, #10
    //   8000106: 47a0       blx   r4
    //   8000108: 3e01       subs  r6, #1
    //   800010a: d1fc       bne.n 8000106
    //   800010c: bd70       pop   {r4, r5, r6, pc}
    //   800010e: 0000       .short 0x0000
    //   8000110: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb570, 0x4c03, 0x260a, 0x47a0, 0x3e01, 0xd1fc, 0xbd70, 0x0000, 0x0101, 0x3000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0106, Call(0x3000_0100)),
            (0x0800_010a, Jump(0x0800_0106)),
        ]
    );
    // The pop at 0x8000108 returns, but only on the path that skips the
    // blx, which still gets r7:
    //   8000100: b5f0       push  {r4, r5, r6, r7, lr}
    //   8000102: 4f03       ldr   r7, [pc, #12]
    //   8000104: 2800       cmp   r0, #0
    //   8000106: d100       bne.n 800010a
    //   8000108: bdf0       pop   {r4, r5, r6, r7, pc}
    //   800010a: 47b8       blx   r7
    //   800010c: bdf0       pop   {r4, r5, r6, r7, pc}
    //   800010e: 0000       .short 0x0000
    //   8000110: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb5f0, 0x4f03, 0x2800, 0xd100, 0xbdf0, 0x47b8, 0xbdf0, 0x0000, 0x0101, 0x3000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0106, Jump(0x0800_010a)),
            (0x0800_010a, Call(0x3000_0100)),
        ]
    );
}

#[test]
fn addresses_are_followed_through_the_stack() {
    // Stored and loaded back through r7, which points into the stack,
    // as without optimization:
    //   8000100: b580       push  {r7, lr}
    //   8000102: b082       sub   sp, #8
    //   8000104: af00       add   r7, sp, #0
    //   8000106: 4b04       ldr   r3, [pc, #16]
    //   8000108: 607b       str   r3, [r7, #4]
    //   800010a: f7ff fff9  bl    8000100
    //   800010e: 687b       ldr   r3, [r7, #4]
    //   8000110: 4798       blx   r3
    //   8000112: 3708       adds  r7, #8
    //   8000114: 46bd       mov   sp, r7
    //   8000116: bd80       pop   {r7, pc}
    //   8000118: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb580, 0xb082, 0xaf00, 0x4b04, 0x607b, 0xf7ff, 0xfff9, 0x687b, 0x4798, 0x3708, 0x46bd,
        0xbd80, 0x0101, 0x3000,
    ]);
    // r3 still holds the address at the bl, which may take it as an
    // argument.
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0106, Address(0x3000_0101)),
            (0x0800_010a, Call(0x0800_0100)),
            (0x0800_0110, Call(0x3000_0100)),
        ]
    );
    // The stub that LLD adds for Cortex-M0 stores the address over the
    // r1 it pushed, and pops it into pc:
    //   8000100: b403       push  {r0, r1}
    //   8000102: 4801       ldr   r0, [pc, #4]
    //   8000104: 9001       str   r0, [sp, #4]
    //   8000106: bd01       pop   {r0, pc}
    //   8000108: 30000101   .word 0x30000101
    let code = halfwords(&[0xb403, 0x4801, 0x9001, 0xbd01, 0x0101, 0x3000]);
    assert_eq!(kinds(&code), [(0x0800_0106, TailJump(0x3000_0100))]);
}

#[test]
fn a_function_that_gets_the_stack_s_address_may_change_it() {
    // r0 points at the word that holds the address, so the bl may
    // write another there:
    //   8000100: b510       push  {r4, lr}
    //   8000102: b082       sub   sp, #8
    //   8000104: 4b04       ldr   r3, [pc, #16]
    //   8000106: 9301       str   r3, [sp, #4]
    //   8000108: a801       add   r0, sp, #4
    //   800010a: f7ff fff9  bl    8000100
    //   800010e: 9b01       ldr   r3, [sp, #4]
    //   8000110: 4798       blx   r3
    //   8000112: b002       add   sp, #8
    //   8000114: bd10       pop   {r4, pc}
    //   8000116: 0000       .short 0x0000
    //   8000118: 30000101   .word 0x30000101
    let code = halfwords(&[
        0xb510, 0xb082, 0x4b04, 0x9301, 0xa801, 0xf7ff, 0xfff9, 0x9b01, 0x4798, 0xb002, 0xbd10,
        0x0000, 0x0101, 0x3000,
    ]);
    assert_eq!(
        kinds(&code),
        [
            (0x0800_0104, Address(0x3000_0101)),
            (0x0800_010a, Call(0x0800_0100)),
            (0x0800_0110, IndirectCall),
        ]
    );
}

#[test]
fn a_veneer_that_adds_pc_jumps_to_its_target() {
    // GNU ld's --pic-veneer. Reading pc at 0x8000104 gives 0x8000108:
    //   8000100: b401       push  {r0}
    //   8000102: 4802       ldr   r0, [pc, #8]
    //   8000104: 46fc       mov   ip, pc
    //   8000106: 4484       add   ip, r0
    //   8000108: bc01       pop   {r0}
    //   800010a: 4760       bx    ip
    //   800010c: 27fffff9   .word 0x27fffff9
    let code = halfwords(&[
        0xb401, 0x4802, 0x46fc, 0x4484, 0xbc01, 0x4760, 0xfff9, 0x27ff,
    ]);
    assert_eq!(kinds(&code), [(0x0800_010a, TailJump(0x3000_0100))]);
}

#[test]
fn code_that_never_runs_makes_no_calls() {
    // A call to a weak function that isn't there, whose address is 0,
    // only when it isn't 0, as in crtstuff's register_tm_clones:
    //   8000100: f04f 0300  mov.w r3, #0
    //   8000104: b113       cbz   r3, 800010c
    //   8000106: b510       push  {r4, lr}
    //   8000108: 4798       blx   r3
    //   800010a: bd10       pop   {r4, pc}
    //   800010c: 4770       bx    lr
    let code = halfwords(&[0xf04f, 0x0300, 0xb113, 0xb510, 0x4798, 0xbd10, 0x4770]);
    assert_eq!(kinds(&code), [(0x0800_0104, Jump(0x0800_010c))]);
}
