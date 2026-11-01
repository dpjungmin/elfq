//! Tests of the walk: small programs in Thumb code, written as the 16-bit
//! units that a disassembler shows, walked from their first instruction.

use object::Endianness::Little;

use super::each_iteration::same_words;
use super::execute::{add, holds, subtract};
use super::walk_state::Flags;
use super::{branches_past, passes_result, walk};
use crate::arch::startup::{DEPTH, Event, How, Program, Start, Walk};

/// Where the code of each test is, as in flash.
const FLASH: u64 = 0x0800_0000;

/// Code in flash from 0x8000000, made of 16-bit units as a disassembler
/// shows them: a 32-bit instruction as its first 16 bits, then the
/// second, and a word as its low half, then the high. Flash holds the
/// same at reset.
struct Flash {
    code: Vec<u8>,
    /// Where each function starts and ends.
    functions: Vec<(u64, u64)>,
}

impl Program for Flash {
    fn code(&self, address: u64) -> Option<&[u8]> {
        let offset = usize::try_from(address.checked_sub(FLASH)?).ok()?;
        self.code.get(offset..).filter(|rest| !rest.is_empty())
    }

    fn word_at_reset(&self, address: u64) -> Option<u32> {
        let bytes = self.code(address)?.get(..4)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }

    fn function_at(&self, address: u64) -> Option<(u64, u64)> {
        (self.functions.iter().copied()).find(|&(start, end)| (start..end).contains(&address))
    }
}

/// What the walk from 0x8000000 finds in `units`, with the functions
/// that start at `functions` and run to the next one, and to the end
/// of the code for the last one: a line for each event, with its depth
/// first, and one for how the walk ends.
fn walk_lines(units: &[u16], functions: &[u64], main: Option<u64>) -> Vec<String> {
    walk_lines_following(units, functions, main, DEPTH)
}

/// The walk from 0x8000000 through `units`, as `walk_lines` says, that
/// follows calls `follow` deep.
fn walk_units(units: &[u16], functions: &[u64], main: Option<u64>, follow: u32) -> Walk {
    let code: Vec<u8> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
    let end = FLASH + code.len() as u64;
    let ends = functions.iter().skip(1).copied().chain([end]);
    let flash = Flash {
        functions: functions.iter().copied().zip(ends).collect(),
        code,
    };
    let start = Start {
        address: FLASH | 1,
        stack_pointer: Some(0x2000_1000),
        main,
        follow,
    };
    walk(Little, &flash, &start)
}

/// What the stores of each loop that the walk through `units` counts
/// do, as `walk_lines` says: a line for each, as "0x8000010: 4 bytes
/// at 0x20000000 +4n, 4 times, of ?, from 0x8000100 +4n, loaded".
fn loop_stores(units: &[u16], functions: &[u64], main: Option<u64>) -> Vec<String> {
    let walk = walk_units(units, functions, main, DEPTH);
    let number = |number: Option<u32>| number.map_or("?".to_string(), |n| format!("{n:#x}"));
    (walk.events.iter())
        .filter_map(|event| match event {
            Event::Loop(l) => l.stores(),
            _ => None,
        })
        .flatten()
        .map(|store| {
            let source = store.source.map_or(String::new(), |(address, step)| {
                format!(", from {address:#x} {step:+}n")
            });
            let loaded = if store.loaded { ", loaded" } else { "" };
            format!(
                "{:#x}: {} bytes at {:#x} {:+}n, {} times, of {}{source}{loaded}",
                store.site,
                store.size,
                store.address,
                store.step,
                store.times,
                number(store.value)
            )
        })
        .collect()
}

/// As `walk_lines`, for a walk that follows calls `follow` deep.
fn walk_lines_following(
    units: &[u16],
    functions: &[u64],
    main: Option<u64>,
    follow: u32,
) -> Vec<String> {
    let walk = walk_units(units, functions, main, follow);
    let number = |number: Option<u32>| number.map_or("?".to_string(), |n| format!("{n:#x}"));
    let mut lines: Vec<String> = (walk.events.iter().enumerate())
        .map(|(k, event)| match event {
            Event::Call(call) => {
                let how = match call.how {
                    How::Direct => "call".to_string(),
                    How::Register(register) => format!("call through r{register}"),
                    How::Jump => "tail call".to_string(),
                    How::Supervisor => "svc".to_string(),
                };
                let arguments: Vec<String> = call.arguments.iter().map(|&n| number(n)).collect();
                let target = call.target.map_or("?".to_string(), |n| format!("{n:#x}"));
                let arguments = arguments.join(" ");
                let condition = match (call.conditional, call.followed) {
                    (true, _) => ", if",
                    (false, false) if call.target.is_some() => ", not followed",
                    _ => "",
                };
                let left: Vec<String> = (0..4)
                    .filter(|&n| call.left & 1 << n != 0 && call.arguments[n].is_some())
                    .map(|n| format!(" r{n}"))
                    .collect();
                let left = match left.is_empty() {
                    true => String::new(),
                    false => format!(", left by a call:{}", left.concat()),
                };
                format!(
                    "{} {:#x} {how} {target} ({arguments}){condition}{left}",
                    call.depth, call.site
                )
            }
            Event::Store(store) => {
                let place = match (store.address, store.base) {
                    (Some(address), _) => format!("{address:#x}"),
                    (None, Some(base)) => format!("{base:#x} on"),
                    (None, None) => "?".to_string(),
                };
                let value = number(store.value);
                let size = store.size;
                let condition = if store.conditional { ", if" } else { "" };
                let source = store.source.map_or(String::new(), |source| {
                    format!(", loaded from {:#x} at {:#x}", source.address, source.site)
                });
                let (depth, site) = (store.depth, store.site);
                format!(
                    "{depth} {site:#x} store {value} at {place}, {size} bytes{condition}{source}"
                )
            }
            Event::Loop(l) => {
                // The loop's events come just before it, and those of the
                // iterations that the walk went through one by one are
                // theirs, one after another.
                assert_eq!(l.events.end, k, "the events of the loop at {k}");
                let iterations = l.walked_iterations();
                if let (Some(first), Some(last)) = (iterations.first(), iterations.last()) {
                    assert_eq!(first.events.start..last.events.end, l.events);
                    for pair in iterations.windows(2) {
                        assert_eq!(pair[0].events.end, pair[1].events.start);
                    }
                }
                let end = l
                    .stopped()
                    .map_or(String::new(), |end| format!(", stops {end:x?}"));
                let runs = l
                    .iterations()
                    .map_or(String::new(), |n| format!(", runs {n:#x} times"));
                // Where each iteration that the walk went through starts,
                // and the row of a table it reads.
                let starts: Vec<String> = (iterations.iter())
                    .map(|iteration| match iteration.row {
                        Some(row) => format!(" {}@{row:#x}", iteration.events.start),
                        None => format!(" {}", iteration.events.start),
                    })
                    .collect();
                let starts = match starts.is_empty() {
                    true => String::new(),
                    false => format!(", times from events{}", starts.concat()),
                };
                format!(
                    "{} {:#x} loop back at {:#x}, from event {}{runs}{starts}{end}",
                    l.depth, l.head, l.back, l.events.start
                )
            }
            Event::Backtrack {
                site,
                target,
                because,
            } => format!("0 {site:#x} back to {target:#x}, since {because:x?}"),
            Event::Stop { site, depth, end } => format!("{depth} {site:#x} stop: {end:x?}"),
        })
        .collect();
    lines.push(format!("end {:x?}", walk.end));
    lines
}

#[test]
fn a_copy_loop_and_a_call_to_zero_memory() {
    // The loop is entered at its test, at the end of its code, and runs
    // 4 times, which leaves 0x10 in r3; this memset leaves the end in
    // r0:
    //   8000000: f04f 5000  mov.w r0, #0x20000000
    //   8000004: 4908       ldr   r1, [pc, #32]  ; 0x20000010
    //   8000006: 4a09       ldr   r2, [pc, #36]  ; 0x8000100
    //   8000008: 2300       movs  r3, #0
    //   800000a: e002       b.n   8000012
    //   800000c: 58d4       ldr   r4, [r2, r3]
    //   800000e: 50c4       str   r4, [r0, r3]
    //   8000010: 3304       adds  r3, #4
    //   8000012: 18c4       adds  r4, r0, r3
    //   8000014: 428c       cmp   r4, r1
    //   8000016: d3f9       bcc.n 800000c
    //   8000018: 4803       ldr   r0, [pc, #12]  ; 0x20000010
    //   800001a: 2100       movs  r1, #0
    //   800001c: 2220       movs  r2, #32
    //   800001e: f000 f807  bl    8000030 <memset>
    //   8000022: f000 f80c  bl    800003e <main>
    //   8000026: 0000
    //   8000028: 20000010   .word
    //   800002c: 08000100   .word
    // memset:
    //   8000030: 1882       adds  r2, r0, r2
    //   8000032: 4290       cmp   r0, r2
    //   8000034: d002       beq.n 800003c
    //   8000036: f800 1b01  strb.w r1, [r0], #1
    //   800003a: e7fa       b.n   8000032
    //   800003c: 4770       bx    lr
    // main:
    //   800003e: e7fe       b.n   800003e
    let code = [
        0xf04f, 0x5000, 0x4908, 0x4a09, 0x2300, 0xe002, 0x58d4, 0x50c4, 0x3304, 0x18c4, 0x428c,
        0xd3f9, 0x4803, 0x2100, 0x2220, 0xf000, 0xf807, 0xf000, 0xf80c, 0x0000, 0x0010, 0x2000,
        0x0100, 0x0800, 0x1882, 0x4290, 0xd002, 0xf800, 0x1b01, 0xe7fa, 0x4770, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0030, 0x0800_003e];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_003e)),
        [
            "0 0x800000e store ? at 0x20000000, 4 bytes, loaded from 0x8000100 at 0x800000c",
            "0 0x800000c loop back at 0x8000016, from event 0, runs 0x4 times",
            "0 0x800001e call 0x8000030 (0x20000010 0x0 0x20 0x10)",
            "1 0x8000036 store ? at 0x20000010, 1 bytes",
            "1 0x8000032 loop back at 0x800003a, from event 3, runs 0x21 times",
            "0 0x8000022 call 0x800003e (0x20000030 0x0 0x20000030 0x10), left by a call: r0 r2",
            "end Main(8000022)",
        ]
    );
    assert_eq!(
        loop_stores(&code, &functions, Some(0x0800_003e)),
        [
            "0x800000e: 4 bytes at 0x20000000 +4n, 4 times, of ?, from 0x8000100 +4n, loaded",
            "0x8000036: 1 bytes at 0x20000010 +1n, 32 times, of ?"
        ]
    );
}

#[test]
fn it_blocks_run_as_the_flags_say() {
    // Where the walk can tell the flags, an instruction in an `it`
    // block runs or not; where it can't, what it writes is unknown, and
    // a store is conditional:
    //   8000000: 2210       movs  r2, #16
    //   8000002: 2a20       cmp   r2, #32
    //   8000004: bf88       it    hi
    //   8000006: 2200       movhi r2, #0
    //   8000008: bf94       ite   ls
    //   800000a: 2101       movls r1, #1
    //   800000c: 2102       movhi r1, #2
    //   800000e: f04f 4080  mov.w r0, #0x40000000
    //   8000012: 6803       ldr   r3, [r0, #0]
    //   8000014: 2b00       cmp   r3, #0
    //   8000016: bf04       itt   eq
    //   8000018: 2230       moveq r2, #48
    //   800001a: 6041       streq r1, [r0, #4]
    //   800001c: f000 f802  bl    8000024 <func>
    //   8000020: f000 f801  bl    8000026 <main>
    // func:
    //   8000024: 4770       bx    lr
    // main:
    //   8000026: e7fe       b.n   8000026
    let code = [
        0x2210, 0x2a20, 0xbf88, 0x2200, 0xbf94, 0x2101, 0x2102, 0xf04f, 0x4080, 0x6803, 0x2b00,
        0xbf04, 0x2230, 0x6041, 0xf000, 0xf802, 0xf000, 0xf801, 0x4770, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0024, 0x0800_0026];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0026)),
        [
            "0 0x800001a store 0x1 at 0x40000004, 4 bytes, if",
            "0 0x800001c call 0x8000024 (0x40000000 0x1 ? ?)",
            "0 0x8000020 call 0x8000026 (0x40000000 0x1 ? ?)",
            "end Main(8000020)",
        ]
    );
}

#[test]
fn a_compare_that_skips_a_loop_for_an_empty_section() {
    // The first loop is skipped, since r0 and r1 hold the same address;
    // the second runs twice, up to r1:
    //   8000000: f04f 5000  mov.w r0, #0x20000000
    //   8000004: f04f 5100  mov.w r1, #0x20000000
    //   8000008: 2200       movs  r2, #0
    //   800000a: 4288       cmp   r0, r1
    //   800000c: d203       bcs.n 8000016
    //   800000e: f840 2b04  str.w r2, [r0], #4
    //   8000012: 4288       cmp   r0, r1
    //   8000014: d3fb       bcc.n 800000e
    //   8000016: f04f 5000  mov.w r0, #0x20000000
    //   800001a: 4904       ldr   r1, [pc, #16]  ; 0x20000008
    //   800001c: 4288       cmp   r0, r1
    //   800001e: d203       bcs.n 8000028
    //   8000020: f840 2b04  str.w r2, [r0], #4
    //   8000024: 4288       cmp   r0, r1
    //   8000026: d3fb       bcc.n 8000020
    //   8000028: f000 f802  bl    8000030 <main>
    //   800002c: 20000008   .word
    // main:
    //   8000030: e7fe       b.n   8000030
    let code = [
        0xf04f, 0x5000, 0xf04f, 0x5100, 0x2200, 0x4288, 0xd203, 0xf840, 0x2b04, 0x4288, 0xd3fb,
        0xf04f, 0x5000, 0x4904, 0x4288, 0xd203, 0xf840, 0x2b04, 0x4288, 0xd3fb, 0xf000, 0xf802,
        0x0008, 0x2000, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0030];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0030)),
        [
            "0 0x8000020 store 0x0 at 0x20000000, 4 bytes",
            "0 0x8000020 loop back at 0x8000026, from event 0, runs 0x2 times",
            "0 0x8000028 call 0x8000030 (0x20000008 0x20000008 0x0 ?)",
            "end Main(8000028)",
        ]
    );
}

#[test]
fn a_tail_call_goes_on_at_the_depth_of_the_function_that_makes_it() {
    // The reset handler goes on after a call with what the function
    // left in the registers:
    //   8000000: f000 f801  bl    8000006 <init>
    //   8000004: e005       b.n   8000012 <main>
    // init:
    //   8000006: f04f 5100  mov.w r1, #0x20000000
    //   800000a: 2001       movs  r0, #1
    //   800000c: e7ff       b.n   800000e <helper>
    // helper:
    //   800000e: 6008       str   r0, [r1, #0]
    //   8000010: 4770       bx    lr
    // main:
    //   8000012: e7fe       b.n   8000012
    let code = [
        0xf000, 0xf801, 0xe005, 0xf04f, 0x5100, 0x2001, 0xe7ff, 0x6008, 0x4770, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0006, 0x0800_000e, 0x0800_0012];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0012)),
        [
            "0 0x8000000 call 0x8000006 (? ? ? ?)",
            "1 0x800000c tail call 0x800000e (0x1 0x20000000 ? ?)",
            "1 0x800000e store 0x1 at 0x20000000, 4 bytes",
            "0 0x8000004 tail call 0x8000012 (0x1 0x20000000 ? ?), left by a call: r0 r1",
            "end Main(8000004)",
        ]
    );
}

#[test]
fn a_call_that_doesnt_return_runs_past_the_end_of_its_function() {
    //   8000000: 2000       movs  r0, #0
    //   8000002: f000 f800  bl    8000006 <fatal>
    // fatal:
    //   8000006: e7fe       b.n   8000006
    let code = [0x2000, 0xf000, 0xf800, 0xe7fe];
    let functions = [0x0800_0000, 0x0800_0006];
    assert_eq!(
        walk_lines(&code, &functions, None),
        [
            "0 0x8000002 call 0x8000006 (0x0 ? ? ?)",
            "end RunsOff(8000006)"
        ]
    );
}

#[test]
fn a_call_that_doesnt_return_is_followed_by_udf_or_data() {
    //   8000000: f000 f801  bl    8000006 <fatal>
    //   8000004: de00       udf   #0
    // fatal:
    //   8000006: e7fe       b.n   8000006
    let code = [0xf000, 0xf801, 0xde00, 0xe7fe];
    let functions = [0x0800_0000, 0x0800_0006];
    assert_eq!(
        walk_lines(&code, &functions, None),
        [
            "0 0x8000000 call 0x8000006 (? ? ? ?)",
            "end RunsOff(8000004)"
        ]
    );
    // So does a bkpt, as some compilers put after a call to abort, but
    // not bkpt 0xab, which asks a debugger to do semihosting, and after
    // which the walk goes on, here into fatal's code:
    //   8000004: be00       bkpt  0x0000
    let code = [0xf000, 0xf801, 0xbe00, 0xe7fe];
    assert_eq!(
        walk_lines(&code, &functions, None),
        [
            "0 0x8000000 call 0x8000006 (? ? ? ?)",
            "end RunsOff(8000004)"
        ]
    );
    //   8000004: beab       bkpt  0x00ab
    let code = [0xf000, 0xf801, 0xbeab, 0xe7fe];
    assert_eq!(
        walk_lines(&code, &functions, None),
        [
            "0 0x8000000 call 0x8000006 (? ? ? ?)",
            "end RunsOff(8000006)"
        ]
    );
    // The walk doesn't run the literal pool's first word, a store, as
    // code:
    //   8000000: 4901       ldr   r1, [pc, #4]  ; 0x12346000
    //   8000002: 4802       ldr   r0, [pc, #8]  ; fatal
    //   8000004: 4780       blx   r0
    //   8000006: bf00       nop
    //   8000008: 12346000   .word
    //   800000c: 08000011   .word
    // fatal:
    //   8000010: e7fe       b.n   8000010
    let code = [
        0x4901, 0x4802, 0x4780, 0xbf00, 0x6000, 0x1234, 0x0011, 0x0800, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0010];
    assert_eq!(
        walk_lines(&code, &functions, None),
        [
            "0 0x8000004 call through r0 0x8000010 (0x8000011 0x12346000 ? ?)",
            "end RunsOff(8000008)"
        ]
    );
}

#[test]
fn a_veneer_that_pops_the_address_it_goes_to_into_pc() {
    // LLD's veneer for Cortex-M0, which has no ip to spare: the walk
    // goes on to init, with r0 as the reset handler left it.
    //   8000000: 2005       movs  r0, #5
    //   8000002: f000 f802  bl    800000a <veneer>
    //   8000006: f000 f808  bl    800001a <main>
    // veneer:
    //   800000a: b403       push  {r0, r1}
    //   800000c: 4801       ldr   r0, [pc, #4]  ; init + 1
    //   800000e: 9001       str   r0, [sp, #4]
    //   8000010: bd01       pop   {r0, pc}
    //   8000012: bf00       nop
    //   8000014: 08000019   .word
    // init:
    //   8000018: 4770       bx    lr
    // main:
    //   800001a: e7fe       b.n   800001a
    let code = [
        0x2005, 0xf000, 0xf802, 0xf000, 0xf808, 0xb403, 0x4801, 0x9001, 0xbd01, 0xbf00, 0x0019,
        0x0800, 0x4770, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_000a, 0x0800_0018, 0x0800_001a];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_001a)),
        [
            "0 0x8000002 call 0x800000a (0x5 ? ? ?)",
            "1 0x8000010 tail call 0x8000018 (0x5 ? ? ?)",
            "0 0x8000006 call 0x800001a (0x5 ? ? ?), left by a call: r0",
            "end Main(8000006)"
        ]
    );
}

#[test]
fn a_loop_entered_at_its_test_that_runs_no_times() {
    // GCC -O0 jumps to a loop's test, which here finds no functions in
    // an empty table to call:
    //   8000000: 4c04       ldr   r4, [pc, #16]  ; 0x8000100
    //   8000002: 4d04       ldr   r5, [pc, #16]  ; 0x8000100
    //   8000004: e002       b.n   800000c
    //   8000006: 6820       ldr   r0, [r4, #0]
    //   8000008: 4780       blx   r0
    //   800000a: 3404       adds  r4, #4
    //   800000c: 42ac       cmp   r4, r5
    //   800000e: d1fa       bne.n 8000006
    //   8000010: f000 f802  bl    8000018 <main>
    //   8000014: 08000100   .word
    // main:
    //   8000018: e7fe       b.n   8000018
    let code = [
        0x4c04, 0x4d04, 0xe002, 0x6820, 0x4780, 0x3404, 0x42ac, 0xd1fa, 0xf000, 0xf802, 0x0100,
        0x0800, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0018];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0018)),
        ["0 0x8000010 call 0x8000018 (? ? ? ?)", "end Main(8000010)"]
    );
}

#[test]
fn a_length_rounded_to_words_with_bics() {
    // As Cortex-M0 code does, which has no bic with an immediate:
    //   8000000: f04f 5000  mov.w r0, #0x20000000
    //   8000004: 4a04       ldr   r2, [pc, #16]  ; 0x20000107
    //   8000006: 1a12       subs  r2, r2, r0
    //   8000008: 3203       adds  r2, #3
    //   800000a: 2103       movs  r1, #3
    //   800000c: 438a       bics  r2, r1
    //   800000e: 2100       movs  r1, #0
    //   8000010: f000 f804  bl    800001c <memset>
    //   8000014: f000 f803  bl    800001e <main>
    //   8000018: 20000107   .word
    // memset:
    //   800001c: 4770       bx    lr
    // main:
    //   800001e: e7fe       b.n   800001e
    let code = [
        0xf04f, 0x5000, 0x4a04, 0x1a12, 0x3203, 0x2103, 0x438a, 0x2100, 0xf000, 0xf804, 0xf000,
        0xf803, 0x0107, 0x2000, 0x4770, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_001c, 0x0800_001e];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_001e)),
        [
            "0 0x8000010 call 0x800001c (0x20000000 0x0 0x108 ?)",
            "0 0x8000014 call 0x800001e (0x20000000 0x0 0x108 ?)",
            "end Main(8000014)"
        ]
    );
}

#[test]
fn a_call_leaves_what_its_code_uses_in_r0_to_r3() {
    // init doesn't use r0, so it still holds what the reset handler
    // set for copy, as code built with GCC's -fipa-ra counts on; copy
    // reads r0 and writes r3, which main then finds there:
    //   8000000: 4803       ldr   r0, [pc, #12]  ; 0x20000000
    //   8000002: f000 f807  bl    8000014 <init>
    //   8000006: 2210       movs  r2, #16
    //   8000008: f000 f806  bl    8000018 <copy>
    //   800000c: f000 f806  bl    800001c <main>
    //   8000010: 20000000   .word
    // init:
    //   8000014: 2301       movs  r3, #1
    //   8000016: 4770       bx    lr
    // copy:
    //   8000018: 4603       mov   r3, r0
    //   800001a: 4770       bx    lr
    // main:
    //   800001c: e7fe       b.n   800001c
    let code = [
        0x4803, 0xf000, 0xf807, 0x2210, 0xf000, 0xf806, 0xf000, 0xf806, 0x0000, 0x2000, 0x2301,
        0x4770, 0x4603, 0x4770, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0014, 0x0800_0018, 0x0800_001c];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_001c)),
        [
            "0 0x8000002 call 0x8000014 (0x20000000 ? ? ?)",
            "0 0x8000008 call 0x8000018 (0x20000000 ? 0x10 0x1), left by a call: r3",
            "0 0x800000c call 0x800001c (0x20000000 ? 0x10 0x20000000), left by a call: r0 r3",
            "end Main(800000c)"
        ]
    );
}

#[test]
fn a_register_that_a_call_only_reads_isnt_left_by_it() {
    // check reads r1 and writes r2, so main finds the 5 the reset
    // handler put in r1, and 6 in r2 that check left:
    //   8000000: 2105       movs  r1, #5
    //   8000002: f000 f803  bl    800000c <check>
    //   8000006: f000 f803  bl    8000010 <main>
    //   800000a: bf00       nop
    // check:
    //   800000c: 1c4a       adds  r2, r1, #1
    //   800000e: 4770       bx    lr
    // main:
    //   8000010: e7fe       b.n   8000010
    let code = [
        0x2105, 0xf000, 0xf803, 0xf000, 0xf803, 0xbf00, 0x1c4a, 0x4770, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_000c, 0x0800_0010];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0010)),
        [
            "0 0x8000002 call 0x800000c (? 0x5 ? ?)",
            "0 0x8000006 call 0x8000010 (? 0x5 0x6 ?), left by a call: r2",
            "end Main(8000006)"
        ]
    );
}

#[test]
fn a_tail_call_to_a_dead_end_takes_the_other_way_without_following_calls() {
    // As in Pico SDK's crt0, core 1 tail-calls code that branches
    // through a register, and core 0 takes the beq. A walk that doesn't
    // follow calls still looks where the tail call goes, to go back
    // to the beq:
    //   8000000: 6800       ldr   r0, [r0, #0]
    //   8000002: 2800       cmp   r0, #0
    //   8000004: d001       beq.n 800000a
    //   8000006: 2000       movs  r0, #0
    //   8000008: e003       b.n   8000012 <enter>
    //   800000a: f000 f801  bl    8000010 <main>
    //   800000e: e7fe       b.n   800000e
    // main:
    //   8000010: e7fe       b.n   8000010
    // enter:
    //   8000012: 4710       bx    r2
    let code = [
        0x6800, 0x2800, 0xd001, 0x2000, 0xe003, 0xf000, 0xf801, 0xe7fe, 0xe7fe, 0x4710,
    ];
    let functions = [0x0800_0000, 0x0800_0010, 0x0800_0012];
    assert_eq!(
        walk_lines_following(&code, &functions, None, 0),
        [
            "0 0x8000004 back to 0x800000a, since Branches { site: 8000012, through: Register(2) }",
            "0 0x800000a call 0x8000010 (? ? ? ?), not followed",
            "end Spins(800000e)"
        ]
    );
}

#[test]
fn the_instructions_a_dead_end_goes_through_dont_count_for_the_walk() {
    // A walk that follows no calls makes 70 tail calls to a dead end of
    // 300 instructions, each after a branch it can't tell, which it goes
    // back to. Going through the dead end 70 times takes 21,000
    // instructions, more than the walk's 20,000, but they don't count
    // for it, so it gets to main:
    //   8000000: 2900       cmp   r1, #0
    //   8000002: d000       beq.n 8000006
    //   8000004: e0d3       b.n   80001ae <dead>
    //   ... 69 more times, 6 bytes on
    //   80001a4: f000 f801  bl    80001aa <main>
    //   80001a8: bf00       nop
    // main:
    //   80001aa: e7fe       b.n   80001aa
    //   80001ac: bf00       nop
    // dead:
    //   80001ae: bf00       nop
    //   ... 298 more
    //   8000404: 4710       bx    r2
    let blocks = 70u16;
    let mut code = Vec::new();
    for k in 0..blocks {
        // From 0x8000004 + 6k to 0x80001ae: (0x1ae - 8 - 6k) / 2 halves on.
        code.extend([0x2900, 0xd000, 0xe000 | (0xd3 - 3 * k)]);
    }
    code.extend([0xf000, 0xf801, 0xbf00, 0xe7fe, 0xbf00]);
    code.extend([0xbf00; 299]);
    code.push(0x4710);
    let functions = [0x0800_0000, 0x0800_01aa, 0x0800_01ae];
    let lines = walk_lines_following(&code, &functions, Some(0x0800_01aa), 0);
    assert_eq!(lines.len(), 72);
    assert_eq!(
        lines[69..],
        [
            "0 0x80001a0 back to 0x80001a4, since Branches { site: 8000404, through: Register(2) }",
            "0 0x80001a4 call 0x80001aa (? ? ? ?), not followed",
            "end Main(80001a4)",
        ]
    );
}

#[test]
fn the_walk_goes_through_so_many_instructions() {
    // 700 nops in the reset handler.
    let code = [0xbf00; 700];
    assert_eq!(walk_lines(&code, &[FLASH], None), ["end Budget(80004b0)"]);
    // A call to a function of 403 instructions, which the walk leaves
    // after 300 for the code after the call, so it doesn't list the
    // second store:
    //   8000000: f000 f803  bl    800000a <slow>
    //   8000004: f000 f800  bl    8000008 <main>
    // main:
    //   8000008: e7fe       b.n   8000008
    // slow:
    //   800000a: bf00       nop
    //   ...
    //   800025e: 6000       str   r0, [r0, #0]
    //   8000260: bf00       nop
    //   ...
    //   800032c: 6000       str   r0, [r0, #0]
    //   800032e: 4770       bx    lr
    let mut code = vec![0xf000, 0xf803, 0xf000, 0xf800, 0xe7fe];
    code.extend([0xbf00; 298]);
    code.push(0x6000);
    code.extend([0xbf00; 102]);
    code.extend([0x6000, 0x4770]);
    let functions = [0x0800_0000, 0x0800_0008, 0x0800_000a];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0008)),
        [
            "0 0x8000000 call 0x800000a (? ? ? ?)",
            "1 0x800025e store ? at ?, 4 bytes",
            "1 0x8000000 stop: Budget(8000262)",
            "0 0x8000004 call 0x8000008 (? ? ? ?)",
            "end Main(8000004)",
        ]
    );
}

#[test]
fn a_loop_body_after_the_function_s_other_code() {
    // As GCC -Os puts it, when the walk can't tell whether the loop
    // runs:
    //   8000000: f04f 4380  mov.w r3, #0x40000000
    //   8000004: 6819       ldr   r1, [r3, #0]
    //   8000006: f04f 5000  mov.w r0, #0x20000000
    //   800000a: 2200       movs  r2, #0
    //   800000c: 4288       cmp   r0, r1
    //   800000e: d301       bcc.n 8000014
    //   8000010: f000 f803  bl    800001a <main>
    //   8000014: f840 2b04  str.w r2, [r0], #4
    //   8000018: e7f8       b.n   800000c
    // main:
    //   800001a: e7fe       b.n   800001a
    let code = [
        0xf04f, 0x4380, 0x6819, 0xf04f, 0x5000, 0x2200, 0x4288, 0xd301, 0xf000, 0xf803, 0xf840,
        0x2b04, 0xe7f8, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_001a];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_001a)),
        [
            "0 0x8000014 store 0x0 at 0x20000000, 4 bytes",
            "0 0x8000014 loop back at 0x8000018, from event 0",
            "0 0x8000010 call 0x800001a (? ? 0x0 0x40000000)",
            "end Main(8000010)",
        ]
    );
}

#[test]
fn a_pointer_on_the_stack_that_two_loops_share() {
    // As GCC -O0 builds `for (p = &_sdata; p < &_edata;) *p++ = 1;
    // for (; p < &_ebss;) *p++ = 0;`: the second loop starts where the
    // first ends, at 0x20000010, as the walk tells from the pointer on
    // the stack, which moves by 4 up to the bound it is compared with.
    // Stores through r7 to the stack aren't listed:
    //   8000000: b580       push  {r7, lr}
    //   8000002: b082       sub   sp, #8
    //   8000004: af00       add   r7, sp, #0
    //   8000006: f04f 5300  mov.w r3, #0x20000000
    //   800000a: 607b       str   r3, [r7, #4]
    //   800000c: e004       b.n   8000018
    //   800000e: 687b       ldr   r3, [r7, #4]
    //   8000010: 1d1a       adds  r2, r3, #4
    //   8000012: 607a       str   r2, [r7, #4]
    //   8000014: 2201       movs  r2, #1
    //   8000016: 601a       str   r2, [r3, #0]
    //   8000018: 687b       ldr   r3, [r7, #4]
    //   800001a: 4a07       ldr   r2, [pc, #28]  ; 0x20000010
    //   800001c: 4293       cmp   r3, r2
    //   800001e: d3f6       bcc.n 800000e
    //   8000020: e004       b.n   800002c
    //   8000022: 687b       ldr   r3, [r7, #4]
    //   8000024: 1d1a       adds  r2, r3, #4
    //   8000026: 607a       str   r2, [r7, #4]
    //   8000028: 2200       movs  r2, #0
    //   800002a: 601a       str   r2, [r3, #0]
    //   800002c: 687b       ldr   r3, [r7, #4]
    //   800002e: 4a03       ldr   r2, [pc, #12]  ; 0x20000020
    //   8000030: 4293       cmp   r3, r2
    //   8000032: d3f6       bcc.n 8000022
    //   8000034: f000 f804  bl    8000040 <main>
    //   8000038: 20000010   .word
    //   800003c: 20000020   .word
    // main:
    //   8000040: e7fe       b.n   8000040
    let code = [
        0xb580, 0xb082, 0xaf00, 0xf04f, 0x5300, 0x607b, 0xe004, 0x687b, 0x1d1a, 0x607a, 0x2201,
        0x601a, 0x687b, 0x4a07, 0x4293, 0xd3f6, 0xe004, 0x687b, 0x1d1a, 0x607a, 0x2200, 0x601a,
        0x687b, 0x4a03, 0x4293, 0xd3f6, 0xf000, 0xf804, 0x0010, 0x2000, 0x0020, 0x2000, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0040];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0040)),
        [
            "0 0x8000016 store 0x1 at 0x20000000, 4 bytes",
            "0 0x800000e loop back at 0x800001e, from event 0, runs 0x4 times",
            "0 0x800002a store 0x0 at 0x20000010, 4 bytes",
            "0 0x8000022 loop back at 0x8000032, from event 2, runs 0x4 times",
            "0 0x8000034 call 0x8000040 (? ? 0x20000020 0x20000020)",
            "end Main(8000034)",
        ]
    );
}

#[test]
fn a_branch_the_walk_cant_tell_is_taken_when_the_other_way_branches_through_a_register() {
    // A check of which core runs the code, whose fall-through goes
    // where the walk can't follow; what it found that way is dropped:
    //   8000000: 4808       ldr   r0, [pc, #32]  ; 0xe000ed00
    //   8000002: 6800       ldr   r0, [r0, #0]
    //   8000004: f04f 5100  mov.w r1, #0x20000000
    //   8000008: 0900       lsrs  r0, r0, #4
    //   800000a: 28c2       cmp   r0, #194
    //   800000c: d005       beq.n 800001a
    //   800000e: 2201       movs  r2, #1
    //   8000010: 600a       str   r2, [r1, #0]
    //   8000012: f04f 4280  mov.w r2, #0x40000000
    //   8000016: 6812       ldr   r2, [r2, #0]
    //   8000018: 4710       bx    r2
    //   800001a: 2202       movs  r2, #2
    //   800001c: 600a       str   r2, [r1, #0]
    //   800001e: f000 f803  bl    8000028 <main>
    //   8000022: 0000
    //   8000024: e000ed00   .word
    // main:
    //   8000028: e7fe       b.n   8000028
    let code = [
        0x4808, 0x6800, 0xf04f, 0x5100, 0x0900, 0x28c2, 0xd005, 0x2201, 0x600a, 0xf04f, 0x4280,
        0x6812, 0x4710, 0x2202, 0x600a, 0xf000, 0xf803, 0x0000, 0xed00, 0xe000, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0028];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0028)),
        [
            "0 0x800000c back to 0x800001a, since Branches { site: 8000018, through: Register(2) }",
            "0 0x800001c store 0x2 at 0x20000000, 4 bytes",
            "0 0x800001e call 0x8000028 (? 0x20000000 0x2 ?)",
            "end Main(800001e)",
        ]
    );
}

#[test]
fn a_region_table_loop_that_calls_through_a_register() {
    // As Arm Compiler 6's __scatterload goes through its table of
    // regions, calling a function for each to copy or zero it: the
    // walk goes through the loop's code the one time it runs, which
    // reads the first row, with the function, and leaves at the table's
    // end, where the registers hold the row, since the function changes
    // none of them:
    //   8000000: a404       adr   r4, 8000014 <table>
    //   8000002: a508       adr   r5, 8000024 <table_end>
    //   8000004: e001       b.n   800000a
    //   8000006: cc0f       ldmia r4!, {r0, r1, r2, r3}
    //   8000008: 4798       blx   r3
    //   800000a: 42ac       cmp   r4, r5
    //   800000c: d3fb       bcc.n 8000006
    //   800000e: f000 f809  bl    8000024 <main>
    //   8000012: bf00       nop
    //   8000014: 08000100   .word
    //   8000018: 20000000   .word
    //   800001c: 00000010   .word
    //   8000020: 08000027   .word
    // main:
    //   8000024: e7fe       b.n   8000024
    // copy:
    //   8000026: 4770       bx    lr
    let code = [
        0xa404, 0xa508, 0xe001, 0xcc0f, 0x4798, 0x42ac, 0xd3fb, 0xf000, 0xf809, 0xbf00, 0x0100,
        0x0800, 0x0000, 0x2000, 0x0010, 0x0000, 0x0027, 0x0800, 0xe7fe, 0x4770,
    ];
    let functions = [0x0800_0000, 0x0800_0024, 0x0800_0026];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0024)),
        [
            "0 0x8000008 call through r3 0x8000026 (0x8000100 0x20000000 0x10 0x8000027)",
            "0 0x8000006 loop back at 0x800000c, from event 0, runs 0x1 times, times from events 0@0x8000014",
            "0 0x800000e call 0x8000024 (0x8000100 0x20000000 0x10 0x8000027)",
            "end Main(800000e)",
        ]
    );
}

#[test]
fn a_region_table_loop_whose_function_the_walk_cant_tell_is_walked_once() {
    // As armlib's __scatterload sets bit 0 of each row's function with
    // `orr`, which the walk doesn't follow: it can't tell that the
    // rows name the same function, so it goes through the loop once,
    // as through one over a table of functions:
    // reset:
    //   8000000: 4c0a       ldr   r4, [pc, #40]  ; 0x800002c
    //   8000002: 4d0b       ldr   r5, [pc, #44]  ; 0x8000030
    // loop:
    //   8000004: 68e0       ldr   r0, [r4, #12]
    //   8000006: f040 0301  orr.w r3, r0, #1
    //   800000a: e894 0007  ldmia.w r4, {r0, r1, r2}
    //   800000e: 4798       blx   r3
    //   8000010: 3410       adds  r4, #16
    //   8000012: 42ac       cmp   r4, r5
    //   8000014: d3f6       bcc.n 8000004 <loop>
    //   8000016: f000 f808  bl    800002a <main>
    // copy:
    //   800001a: b12a       cbz   r2, 8000028
    //   800001c: f850 3b04  ldr.w r3, [r0], #4
    //   8000020: f841 3b04  str.w r3, [r1], #4
    //   8000024: 3a04       subs  r2, #4
    //   8000026: d1f9       bne.n 800001c
    //   8000028: 4770       bx    lr
    // main:
    //   800002a: e7fe       b.n   800002a <main>
    //   800002c: 08000034   .word  ; table
    //   8000030: 08000054   .word  ; its end
    // table: two rows of (load image, start, size, copy)
    //   8000034: 08000100 20000000 00000010 0800001a
    //   8000044: 08000110 20000010 00000010 0800001a
    let code = [
        0x4c0a, 0x4d0b, 0x68e0, 0xf040, 0x0301, 0xe894, 0x0007, 0x4798, 0x3410, 0x42ac, 0xd3f6,
        0xf000, 0xf808, 0xb12a, 0xf850, 0x3b04, 0xf841, 0x3b04, 0x3a04, 0xd1f9, 0x4770, 0xe7fe,
        0x0034, 0x0800, 0x0054, 0x0800, 0x0100, 0x0800, 0x0000, 0x2000, 0x0010, 0x0000, 0x001a,
        0x0800, 0x0110, 0x0800, 0x0010, 0x2000, 0x0010, 0x0000, 0x001a, 0x0800,
    ];
    let functions = [0x0800_0000, 0x0800_001a, 0x0800_002a];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_002a)),
        [
            "0 0x800000e call through r3 ? (0x8000100 0x20000000 0x10 ?)",
            "0 0x8000004 loop back at 0x8000014, from event 0, runs 0x2 times",
            "0 0x8000016 call 0x800002a (? ? ? ?)",
            "end Main(8000016)",
        ]
    );
}

#[test]
fn a_table_loop_that_loads_the_next_row_at_its_end() {
    // The code loads the first row before the loop, and the next at
    // the end of each iteration, which the test then reads. Each
    // iteration's call reads a row that registers carry over from the
    // iteration before, which no register points at, so elfq names no
    // rows:
    // reset:
    //   8000000: 4c08       ldr   r4, [pc, #32]  ; 0x8000024
    //   8000002: cc07       ldmia r4!, {r0, r1, r2}
    // loop:
    //   8000004: f000 f805  bl    8000012 <copy>
    //   8000008: cc07       ldmia r4!, {r0, r1, r2}
    //   800000a: 2a00       cmp   r2, #0
    //   800000c: d1fa       bne.n 8000004 <loop>
    //   800000e: f000 f807  bl    8000020 <main>
    // copy:
    //   8000012: f850 3b04  ldr.w r3, [r0], #4
    //   8000016: f841 3b04  str.w r3, [r1], #4
    //   800001a: 3a04       subs  r2, #4
    //   800001c: d1f9       bne.n 8000012 <copy>
    //   800001e: 4770       bx    lr
    // main:
    //   8000020: e7fe       b.n   8000020 <main>
    //   8000024: 08000028   .word  ; table
    // table: two rows of (load image, start, size), then zeros
    //   8000028: 08000100 20000000 00000010
    //   8000034: 08000110 20000010 00000010
    //   8000040: 00000000 00000000 00000000
    let code = [
        0x4c08, 0xcc07, 0xf000, 0xf805, 0xcc07, 0x2a00, 0xd1fa, 0xf000, 0xf807, 0xf850, 0x3b04,
        0xf841, 0x3b04, 0x3a04, 0xd1f9, 0x4770, 0xe7fe, 0x0000, 0x0028, 0x0800, 0x0100, 0x0800,
        0x0000, 0x2000, 0x0010, 0x0000, 0x0110, 0x0800, 0x0010, 0x2000, 0x0010, 0x0000, 0x0000,
        0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
    ];
    let functions = [0x0800_0000, 0x0800_0012, 0x0800_0020];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0020)),
        [
            "0 0x8000004 call 0x8000012 (0x8000100 0x20000000 0x10 ?)",
            "1 0x8000016 store ? at 0x20000000, 4 bytes, loaded from 0x8000100 at 0x8000012",
            "1 0x8000012 loop back at 0x800001c, from event 1, runs 0x4 times",
            "0 0x8000004 call 0x8000012 (0x8000110 0x20000010 0x10 ?)",
            "1 0x8000016 store ? at 0x20000010, 4 bytes, loaded from 0x8000110 at 0x8000012",
            "1 0x8000012 loop back at 0x800001c, from event 4, runs 0x4 times",
            "0 0x8000004 loop back at 0x800000c, from event 0, runs 0x2 times, times from events 0 3",
            "0 0x800000e call 0x8000020 (0x0 0x0 0x0 ?)",
            "end Main(800000e)",
        ]
    );
}

#[test]
fn loops_that_cost_more_than_the_walk_each_budget_leave_the_walk_its_own() {
    // Three loops of 16 iterations, one inside the other through calls,
    // cost more to go through each iteration than WALK_EACH_BUDGET lets,
    // so the walk goes through the outer one once, as it would through a
    // loop that it doesn't go through each iteration, and still has its
    // own budget for the copy after it:
    // reset:
    //   8000000: 2600       movs  r6, #0
    //   8000002: f000 f80f  bl    8000024 <f>
    //   8000006: 3601       adds  r6, #1
    //   8000008: 2e10       cmp   r6, #16
    //   800000a: d1fa       bne.n 8000002
    //   800000c: 4811       ldr   r0, [pc, #68]  ; 0x8000054
    //   800000e: f04f 5100  mov.w r1, #0x20000000
    //   8000012: 2210       movs  r2, #16
    //   8000014: f850 3b04  ldr.w r3, [r0], #4
    //   8000018: f841 3b04  str.w r3, [r1], #4
    //   800001c: 3a04       subs  r2, #4
    //   800001e: d1f9       bne.n 8000014
    //   8000020: f000 f817  bl    8000052 <main>
    // f:
    //   8000024: b510       push  {r4, lr}
    //   8000026: 2400       movs  r4, #0
    //   8000028: f000 f804  bl    8000034 <g>
    //   800002c: 3401       adds  r4, #1
    //   800002e: 2c10       cmp   r4, #16
    //   8000030: d1fa       bne.n 8000028
    //   8000032: bd10       pop   {r4, pc}
    // g: as f, calling h
    //   8000034: b520 2500 f000 f804 3501 2d10 d1fa bd20
    // h:
    //   8000044: f04f 4180  mov.w r1, #0x40000000
    //   8000048: 6008       str   r0, [r1, #0]
    //   800004a: 6048       str   r0, [r1, #4]
    //   800004c: 6088       str   r0, [r1, #8]
    //   800004e: 60c8       str   r0, [r1, #12]
    //   8000050: 4770       bx    lr
    // main:
    //   8000052: e7fe       b.n   8000052 <main>
    //   8000054: 08000100   .word
    let code = [
        0x2600, 0xf000, 0xf80f, 0x3601, 0x2e10, 0xd1fa, 0x4811, 0xf04f, 0x5100, 0x2210, 0xf850,
        0x3b04, 0xf841, 0x3b04, 0x3a04, 0xd1f9, 0xf000, 0xf817, 0xb510, 0x2400, 0xf000, 0xf804,
        0x3401, 0x2c10, 0xd1fa, 0xbd10, 0xb520, 0x2500, 0xf000, 0xf804, 0x3501, 0x2d10, 0xd1fa,
        0xbd20, 0xf04f, 0x4180, 0x6008, 0x6048, 0x6088, 0x60c8, 0x4770, 0xe7fe, 0x0100, 0x0800,
    ];
    let functions = [
        0x0800_0000,
        0x0800_0024,
        0x0800_0034,
        0x0800_0044,
        0x0800_0052,
    ];
    let lines = walk_lines(&code, &functions, Some(0x0800_0052));
    assert_eq!(
        lines[lines.len() - 5..],
        [
            "0 0x8000002 loop back at 0x800000a, from event 0, runs 0x10 times",
            "0 0x8000018 store ? at 0x20000000, 4 bytes, loaded from 0x8000100 at 0x8000014",
            "0 0x8000014 loop back at 0x800001e, from event 1315, runs 0x4 times",
            "0 0x8000020 call 0x8000052 (0x8000110 0x20000010 0x0 ?)",
            "end Main(8000020)",
        ]
    );
}

#[test]
fn each_iteration_reads_the_same_words_of_its_row() {
    // Three words of each row, and then only the first, as of the zero
    // word that ends the table; iterations that load nothing don't
    // count.
    let rows = [
        vec![0x100, 0x104, 0x108],
        vec![0x10c, 0x110, 0x114],
        vec![0x118],
    ];
    assert!(same_words(&rows));
    assert!(same_words(&[
        vec![0x100, 0x104],
        vec![],
        vec![0x10c, 0x110]
    ]));
    // A first iteration that also loads the start of the next row at
    // its end, which the next iteration doesn't load again, or an
    // iteration that reads another word of its row.
    let rows = [
        vec![0x100, 0x104, 0x108, 0x10c],
        vec![0x110, 0x114, 0x118],
        vec![0x11c],
    ];
    assert!(!same_words(&rows));
    assert!(!same_words(&[
        vec![0x100, 0x104],
        vec![0x10c, 0x114],
        vec![0x118]
    ]));
    assert!(!same_words(&[vec![0x100], vec![0x10c], vec![0x118, 0x11c]]));
}

#[test]
fn a_table_of_functions_read_with_a_load_that_moves_its_base() {
    // As newlib's __libc_init_array calls the functions in .init_array.
    // main's r0 is what init left there:
    //   8000000: 4d03       ldr   r5, [pc, #12]  ; 0x8000010
    //   8000002: f855 3b04  ldr.w r3, [r5], #4
    //   8000006: 4798       blx   r3
    //   8000008: 4629       mov   r1, r5
    //   800000a: f000 f807  bl    800001c <main>
    //   800000e: bf00       nop
    //   8000010: 08000014   .word  ; the table
    //   8000014: 08000019   .word  ; init + 1
    // init:
    //   8000018: 2001       movs  r0, #1
    //   800001a: 4770       bx    lr
    // main:
    //   800001c: e7fe       b.n   800001c
    let code = [
        0x4d03, 0xf855, 0x3b04, 0x4798, 0x4629, 0xf000, 0xf807, 0xbf00, 0x0014, 0x0800, 0x0019,
        0x0800, 0x2001, 0x4770, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0018, 0x0800_001c];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_001c)),
        [
            "0 0x8000006 call through r3 0x8000018 (? ? ? 0x8000019)",
            "0 0x800000a call 0x800001c (0x1 0x8000018 ? 0x8000019), left by a call: r0",
            "end Main(800000a)",
        ]
    );
}

#[test]
fn calls_are_followed_three_deep() {
    // d, four calls deep, isn't followed, so its store isn't listed. A
    // function the walk can't follow to its end stops there:
    //   8000000: f000 f804  bl    800000c <a>
    //   8000004: f000 f812  bl    800002c <broken>
    //   8000008: f000 f811  bl    800002e <main>
    // a:
    //   800000c: b510       push  {r4, lr}
    //   800000e: f000 f801  bl    8000014 <b>
    //   8000012: bd10       pop   {r4, pc}
    // b:
    //   8000014: b510       push  {r4, lr}
    //   8000016: f000 f801  bl    800001c <c>
    //   800001a: bd10       pop   {r4, pc}
    // c:
    //   800001c: b510       push  {r4, lr}
    //   800001e: f000 f801  bl    8000024 <d>
    //   8000022: bd10       pop   {r4, pc}
    // d:
    //   8000024: f04f 5000  mov.w r0, #0x20000000
    //   8000028: 6000       str   r0, [r0, #0]
    //   800002a: 4770       bx    lr
    // broken:
    //   800002c: de00       udf   #0
    // main:
    //   800002e: e7fe       b.n   800002e
    let code = [
        0xf000, 0xf804, 0xf000, 0xf812, 0xf000, 0xf811, 0xb510, 0xf000, 0xf801, 0xbd10, 0xb510,
        0xf000, 0xf801, 0xbd10, 0xb510, 0xf000, 0xf801, 0xbd10, 0xf04f, 0x5000, 0x6000, 0x4770,
        0xde00, 0xe7fe,
    ];
    let functions = [
        0x0800_0000,
        0x0800_000c,
        0x0800_0014,
        0x0800_001c,
        0x0800_0024,
        0x0800_002c,
        0x0800_002e,
    ];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_002e)),
        [
            "0 0x8000000 call 0x800000c (? ? ? ?)",
            "1 0x800000e call 0x8000014 (? ? ? ?)",
            "2 0x8000016 call 0x800001c (? ? ? ?)",
            "3 0x800001e call 0x8000024 (? ? ? ?), not followed",
            "0 0x8000004 call 0x800002c (? ? ? ?)",
            "1 0x8000004 stop: Undecoded(800002c)",
            "0 0x8000008 call 0x800002e (? ? ? ?)",
            "end Main(8000008)",
        ]
    );
}

#[test]
fn loads_from_flash_from_memory_the_code_wrote_and_from_ram() {
    // Flash holds its words at reset, and RAM holds nothing the walk
    // can tell until code stores there:
    //   8000000: 4805       ldr   r0, [pc, #20]  ; 0x8000020 <table>
    //   8000002: 6801       ldr   r1, [r0, #0]
    //   8000004: 6842       ldr   r2, [r0, #4]
    //   8000006: 4b05       ldr   r3, [pc, #20]  ; 0x20000004
    //   8000008: 601a       str   r2, [r3, #0]
    //   800000a: 681b       ldr   r3, [r3, #0]
    //   800000c: f04f 5400  mov.w r4, #0x20000000
    //   8000010: 6824       ldr   r4, [r4, #0]
    //   8000012: 608c       str   r4, [r1, #8]
    //   8000014: f000 f808  bl    8000028 <main>
    //   8000018: 08000020   .word
    //   800001c: 20000004   .word
    // table:
    //   8000020: 20000000   .word
    //   8000024: 00000100   .word
    // main:
    //   8000028: e7fe       b.n   8000028
    let code = [
        0x4805, 0x6801, 0x6842, 0x4b05, 0x601a, 0x681b, 0xf04f, 0x5400, 0x6824, 0x608c, 0xf000,
        0xf808, 0x0020, 0x0800, 0x0004, 0x2000, 0x0000, 0x2000, 0x0100, 0x0000, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0028];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0028)),
        [
            "0 0x8000008 store 0x100 at 0x20000004, 4 bytes, loaded from 0x8000024 at 0x8000004",
            "0 0x8000012 store ? at 0x20000008, 4 bytes, loaded from 0x20000000 at 0x8000010",
            "0 0x8000014 call 0x8000028 (0x8000020 0x20000000 0x100 0x100)",
            "end Main(8000014)",
        ]
    );
}

#[test]
fn the_walk_ends_where_it_gets_to_main_s_code() {
    // As where a compiler inlined main into the reset handler:
    //   8000000: 2000       movs  r0, #0
    //   8000002: e7fe       b.n   8000002
    assert_eq!(
        walk_lines(&[0x2000, 0xe7fe], &[FLASH], Some(0x0800_0002)),
        ["end Main(8000002)"]
    );
}

#[test]
fn a_call_returns_what_the_function_left_in_registers_and_memory() {
    // A result in r0 and r1, a word stored through a pointer to the
    // caller's stack, and one stored in RAM:
    //   8000000: b082       sub   sp, #8
    //   8000002: 4668       mov   r0, sp
    //   8000004: f000 f806  bl    8000014 <divmod>
    //   8000008: 9b00       ldr   r3, [sp, #0]
    //   800000a: f04f 5200  mov.w r2, #0x20000000
    //   800000e: 6812       ldr   r2, [r2, #0]
    //   8000010: f000 f809  bl    8000026 <main>
    // divmod:
    //   8000014: 2109       movs  r1, #9
    //   8000016: 6001       str   r1, [r0, #0]
    //   8000018: f04f 5000  mov.w r0, #0x20000000
    //   800001c: 2107       movs  r1, #7
    //   800001e: 6001       str   r1, [r0, #0]
    //   8000020: 2002       movs  r0, #2
    //   8000022: 2103       movs  r1, #3
    //   8000024: 4770       bx    lr
    // main:
    //   8000026: e7fe       b.n   8000026
    let code = [
        0xb082, 0x4668, 0xf000, 0xf806, 0x9b00, 0xf04f, 0x5200, 0x6812, 0xf000, 0xf809, 0x2109,
        0x6001, 0xf04f, 0x5000, 0x2107, 0x6001, 0x2002, 0x2103, 0x4770, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0014, 0x0800_0026];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0026)),
        [
            "0 0x8000004 call 0x8000014 (0x20000ff8 ? ? ?)",
            "1 0x800001e store 0x7 at 0x20000000, 4 bytes",
            "0 0x8000010 call 0x8000026 (0x2 0x3 0x7 0x9), left by a call: r0 r1",
            "end Main(8000010)",
        ]
    );
}

#[test]
fn a_call_in_an_it_block_isnt_followed_and_changes_only_what_it_writes() {
    //   8000000: 2101       movs  r1, #1
    //   8000002: 2202       movs  r2, #2
    //   8000004: 2303       movs  r3, #3
    //   8000006: 2800       cmp   r0, #0
    //   8000008: bf08       it    eq
    //   800000a: f000 f802  bleq  8000012 <set_r0>
    //   800000e: f000 f802  bl    8000016 <main>
    // set_r0:
    //   8000012: 2005       movs  r0, #5
    //   8000014: 4770       bx    lr
    // main:
    //   8000016: e7fe       b.n   8000016
    let code = [
        0x2101, 0x2202, 0x2303, 0x2800, 0xbf08, 0xf000, 0xf802, 0xf000, 0xf802, 0x2005, 0x4770,
        0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0012, 0x0800_0016];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0016)),
        [
            "0 0x800000a call 0x8000012 (? 0x1 0x2 0x3), if",
            "0 0x800000e call 0x8000016 (? 0x1 0x2 0x3)",
            "end Main(800000e)",
        ]
    );
}

#[test]
fn a_branch_to_another_function_is_a_tail_call_when_taken() {
    // first branches to second where the walk can't tell, so what it
    // changes is unknown after the call; third always does:
    //   8000000: 2202       movs  r2, #2
    //   8000002: f000 f804  bl    800000e <first>
    //   8000006: f000 f80b  bl    8000020 <third>
    //   800000a: f000 f80c  bl    8000026 <main>
    // first:
    //   800000e: 2100       movs  r1, #0
    //   8000010: 2900       cmp   r1, #0
    //   8000012: d103       bne.n 800001c <second>
    //   8000014: 2800       cmp   r0, #0
    //   8000016: d001       beq.n 800001c <second>
    //   8000018: 2203       movs  r2, #3
    //   800001a: 4770       bx    lr
    // second:
    //   800001c: 2301       movs  r3, #1
    //   800001e: 4770       bx    lr
    // third:
    //   8000020: 2100       movs  r1, #0
    //   8000022: 2900       cmp   r1, #0
    //   8000024: d0fa       beq.n 800001c <second>
    // main:
    //   8000026: e7fe       b.n   8000026
    let code = [
        0x2202, 0xf000, 0xf804, 0xf000, 0xf80b, 0xf000, 0xf80c, 0x2100, 0x2900, 0xd103, 0x2800,
        0xd001, 0x2203, 0x4770, 0x2301, 0x4770, 0x2100, 0x2900, 0xd0fa, 0xe7fe,
    ];
    let functions = [
        0x0800_0000,
        0x0800_000e,
        0x0800_001c,
        0x0800_0020,
        0x0800_0026,
    ];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0026)),
        [
            "0 0x8000002 call 0x800000e (? ? 0x2 ?)",
            "1 0x8000016 tail call 0x800001c (? 0x0 0x2 ?), if",
            "0 0x8000006 call 0x8000020 (? ? ? ?)",
            "1 0x8000024 tail call 0x800001c (? 0x0 ? ?)",
            "0 0x800000a call 0x8000026 (? 0x0 ? 0x1), left by a call: r1 r3",
            "end Main(800000a)",
        ]
    );
}

#[test]
fn a_loop_whose_code_is_too_long_forgets_what_it_writes() {
    // 70 adds in the loop's code, more than the walk goes through:
    //   8000000: 2101       movs  r1, #1
    //   8000002: 2202       movs  r2, #2
    //   8000004: f04f 5000  mov.w r0, #0x20000000
    //   8000008: e046       b.n   8000098
    //   800000a: 3101       adds  r1, #1
    //   ...
    //   8000094: 3101       adds  r1, #1
    //   8000096: 6001       str   r1, [r0, #0]
    //   8000098: 6843       ldr   r3, [r0, #4]
    //   800009a: 2b00       cmp   r3, #0
    //   800009c: d1b5       bne.n 800000a
    //   800009e: f000 f800  bl    80000a2 <main>
    // main:
    //   80000a2: e7fe       b.n   80000a2
    let mut code = vec![0x2101, 0x2202, 0xf04f, 0x5000, 0xe046];
    code.extend([0x3101; 70]);
    code.extend([0x6001, 0x6843, 0x2b00, 0xd1b5, 0xf000, 0xf800, 0xe7fe]);
    let functions = [0x0800_0000, 0x0800_00a2];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_00a2)),
        [
            "0 0x800000a loop back at 0x800009c, from event 0, stops Budget(8000082)",
            "0 0x800009e call 0x80000a2 (0x20000000 ? 0x2 ?)",
            "end Main(800009e)",
        ]
    );
}

#[test]
fn loops_inside_loops_are_walked_8_deep() {
    // Nine loops, each entered at its test:
    //   8000000: e018       b.n   8000034
    //   8000002: e015       b.n   8000030
    //   ...
    //   8000010: e000       b.n   8000014
    //   8000012: bf00       nop
    //   8000014: 2800       cmp   r0, #0
    //   8000016: d1fc       bne.n 8000012
    //   8000018: 2800       cmp   r0, #0
    //   800001a: d1f9       bne.n 8000010
    //   ...
    //   8000034: 2800       cmp   r0, #0
    //   8000036: d1e4       bne.n 8000002
    //   8000038: f000 f800  bl    800003c <main>
    // main:
    //   800003c: e7fe       b.n   800003c
    let code = [
        0xe018, 0xe015, 0xe012, 0xe00f, 0xe00c, 0xe009, 0xe006, 0xe003, 0xe000, 0xbf00, 0x2800,
        0xd1fc, 0x2800, 0xd1f9, 0x2800, 0xd1f6, 0x2800, 0xd1f3, 0x2800, 0xd1f0, 0x2800, 0xd1ed,
        0x2800, 0xd1ea, 0x2800, 0xd1e7, 0x2800, 0xd1e4, 0xf000, 0xf800, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_003c];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_003c)),
        [
            "0 0x8000012 loop back at 0x8000016, from event 0, stops Budget(8000012)",
            "0 0x8000010 loop back at 0x800001a, from event 0",
            "0 0x800000e loop back at 0x800001e, from event 0",
            "0 0x800000c loop back at 0x8000022, from event 0",
            "0 0x800000a loop back at 0x8000026, from event 0",
            "0 0x8000008 loop back at 0x800002a, from event 0",
            "0 0x8000006 loop back at 0x800002e, from event 0",
            "0 0x8000004 loop back at 0x8000032, from event 0",
            "0 0x8000002 loop back at 0x8000036, from event 0",
            "0 0x8000038 call 0x800003c (? ? ? ?)",
            "end Main(8000038)",
        ]
    );
}

#[test]
fn a_loop_goes_on_through_its_own_way_out() {
    // Not through the branch before it, which goes further on:
    //   8000000: f04f 5000  mov.w r0, #0x20000000
    //   8000004: 6801       ldr   r1, [r0, #0]
    //   8000006: 2900       cmp   r1, #0
    //   8000008: d003       beq.n 8000012
    //   800000a: 6842       ldr   r2, [r0, #4]
    //   800000c: 2a00       cmp   r2, #0
    //   800000e: d002       beq.n 8000016
    //   8000010: e7fb       b.n   800000a
    //   8000012: 2301       movs  r3, #1
    //   8000014: e7fe       b.n   8000014
    //   8000016: f000 f800  bl    800001a <main>
    // main:
    //   800001a: e7fe       b.n   800001a
    let code = [
        0xf04f, 0x5000, 0x6801, 0x2900, 0xd003, 0x6842, 0x2a00, 0xd002, 0xe7fb, 0x2301, 0xe7fe,
        0xf000, 0xf800, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_001a];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_001a)),
        [
            "0 0x800000a loop back at 0x8000010, from event 0",
            "0 0x8000016 call 0x800001a (0x20000000 ? ? ?)",
            "end Main(8000016)",
        ]
    );
}

#[test]
fn code_after_the_function_s_other_code_that_goes_back_past_a_call_isnt_a_loop_s_code() {
    // The branch back goes to an outer loop, past a call, and not to
    // a test that leads to the branch to it:
    //   8000000: f04f 5000  mov.w r0, #0x20000000
    //   8000004: f000 f807  bl    8000016 <poll>
    //   8000008: 6801       ldr   r1, [r0, #0]
    //   800000a: 2900       cmp   r1, #0
    //   800000c: d001       beq.n 8000012
    //   800000e: f000 f803  bl    8000018 <main>
    //   8000012: 2201       movs  r2, #1
    //   8000014: e7f6       b.n   8000004
    // poll:
    //   8000016: 4770       bx    lr
    // main:
    //   8000018: e7fe       b.n   8000018
    let code = [
        0xf04f, 0x5000, 0xf000, 0xf807, 0x6801, 0x2900, 0xd001, 0xf000, 0xf803, 0x2201, 0xe7f6,
        0x4770, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0016, 0x0800_0018];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0018)),
        [
            "0 0x8000004 call 0x8000016 (0x20000000 ? ? ?)",
            "0 0x800000e call 0x8000018 (0x20000000 ? ? ?)",
            "end Main(800000e)",
        ]
    );
}

#[test]
fn the_walk_goes_on_into_the_next_function_where_no_call_went_before() {
    // As where a function finder split the reset handler in two:
    //   8000000: 2001       movs  r0, #1
    // next:
    //   8000002: 2102       movs  r1, #2
    //   8000004: f000 f800  bl    8000008 <main>
    // main:
    //   8000008: e7fe       b.n   8000008
    let code = [0x2001, 0x2102, 0xf000, 0xf800, 0xe7fe];
    let functions = [0x0800_0000, 0x0800_0002, 0x0800_0008];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0008)),
        [
            "0 0x8000004 call 0x8000008 (0x1 0x2 ? ?)",
            "end Main(8000004)",
        ]
    );
}

#[test]
fn a_branch_back_into_code_the_walk_went_through_is_a_loop() {
    // Though a function finder took the loop's test for another
    // function:
    //   8000000: f04f 5000  mov.w r0, #0x20000000
    //   8000004: 2100       movs  r1, #0
    //   8000006: 3101       adds  r1, #1
    //   8000008: 6001       str   r1, [r0, #0]
    // split:
    //   800000a: 6842       ldr   r2, [r0, #4]
    //   800000c: 2a00       cmp   r2, #0
    //   800000e: d1fa       bne.n 8000006
    //   8000010: f000 f800  bl    8000014 <main>
    // main:
    //   8000014: e7fe       b.n   8000014
    let code = [
        0xf04f, 0x5000, 0x2100, 0x3101, 0x6001, 0x6842, 0x2a00, 0xd1fa, 0xf000, 0xf800, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_000a, 0x0800_0014];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0014)),
        [
            "0 0x8000008 store 0x1 at 0x20000000, 4 bytes",
            "0 0x8000006 loop back at 0x800000e, from event 0",
            "0 0x8000010 call 0x8000014 (0x20000000 ? ? ?)",
            "end Main(8000010)",
        ]
    );
}

#[test]
fn the_walk_goes_through_20_000_instructions_in_all() {
    // 70 calls of a function of 299 instructions:
    //   8000000: 4c23       ldr   r4, [pc, #140]  ; 0x8000097
    //   8000002: 47a0       blx   r4
    //   ...
    //   800008c: 47a0       blx   r4
    //   800008e: e001       b.n   8000094 <main>
    //   8000090: 08000097   .word
    // main:
    //   8000094: e7fe       b.n   8000094
    // slow:
    //   8000096: bf00       nop
    //   ...
    //   80002e8: bf00       nop
    //   80002ea: 4770       bx    lr
    let mut code = vec![0x4c23];
    code.extend([0x47a0; 70]);
    code.extend([0xe001, 0x0097, 0x0800, 0xe7fe]);
    code.extend([0xbf00; 298]);
    code.push(0x4770);
    let functions = [0x0800_0000, 0x0800_0094, 0x0800_0096];
    let lines = walk_lines(&code, &functions, Some(0x0800_0094));
    // The ldr, 66 calls of 300 instructions with the blx, and the 67th
    // blx make 19,802; the walk goes 198 instructions into slow.
    assert_eq!(lines.len(), 69);
    assert_eq!(
        lines[66..],
        [
            "0 0x8000086 call through r4 0x8000096 (? ? ? ?)",
            "1 0x8000086 stop: Budget(8000222)",
            "end Budget(8000088)",
        ]
    );
}

#[test]
fn the_walk_of_a_function_it_calls_doesnt_take_branches_back() {
    // f branches through r2, which the walk can't tell, on one way;
    // the walk doesn't go back to take the other:
    //   8000000: f000 f802  bl    8000008 <f>
    //   8000004: f000 f807  bl    8000016 <main>
    // f:
    //   8000008: f04f 5000  mov.w r0, #0x20000000
    //   800000c: 6801       ldr   r1, [r0, #0]
    //   800000e: 2900       cmp   r1, #0
    //   8000010: d000       beq.n 8000014
    //   8000012: 4710       bx    r2
    //   8000014: 4770       bx    lr
    // main:
    //   8000016: e7fe       b.n   8000016
    let code = [
        0xf000, 0xf802, 0xf000, 0xf807, 0xf04f, 0x5000, 0x6801, 0x2900, 0xd000, 0x4710, 0x4770,
        0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0008, 0x0800_0016];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0016)),
        [
            "0 0x8000000 call 0x8000008 (? ? ? ?)",
            "1 0x8000000 stop: Branches { site: 8000012, through: Register(2) }",
            "0 0x8000004 call 0x8000016 (? ? ? ?)",
            "end Main(8000004)",
        ]
    );
}

#[test]
fn a_branch_is_taken_when_the_other_way_returns_or_spins_before_main() {
    //   8000000: f04f 5000  mov.w r0, #0x20000000
    //   8000004: 6801       ldr   r1, [r0, #0]
    //   8000006: 2900       cmp   r1, #0
    //   8000008: d000       beq.n 800000c
    //   800000a: 4770       bx    lr
    //   800000c: 2901       cmp   r1, #1
    //   800000e: d000       beq.n 8000012
    //   8000010: e7fe       b.n   8000010
    //   8000012: f000 f800  bl    8000016 <main>
    // main:
    //   8000016: e7fe       b.n   8000016
    let code = [
        0xf04f, 0x5000, 0x6801, 0x2900, 0xd000, 0x4770, 0x2901, 0xd000, 0xe7fe, 0xf000, 0xf800,
        0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0016];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0016)),
        [
            "0 0x8000008 back to 0x800000c, since Returns(800000a)",
            "0 0x800000e back to 0x8000012, since Spins(8000010)",
            "0 0x8000012 call 0x8000016 (0x20000000 ? ? ?)",
            "end Main(8000012)",
        ]
    );
    // Without main, the walk ends where the code spins.
    //   8000000: f04f 5000  mov.w r0, #0x20000000
    //   8000004: 6801       ldr   r1, [r0, #0]
    //   8000006: 2900       cmp   r1, #0
    //   8000008: d000       beq.n 800000c
    //   800000a: e7fe       b.n   800000a
    //   800000c: 4770       bx    lr
    let code = [0xf04f, 0x5000, 0x6801, 0x2900, 0xd000, 0xe7fe, 0x4770];
    assert_eq!(walk_lines(&code, &[FLASH], None), ["end Spins(800000a)"]);
}

#[test]
fn a_branch_taken_later_starts_from_what_the_walk_knew_there() {
    // The walk goes through the code at tail on the way on, so it
    // doesn't go back to take the branch there; it takes the one to
    // good, with r1 as it was then:
    //   8000000: 2101       movs  r1, #1
    //   8000002: f04f 5000  mov.w r0, #0x20000000
    //   8000006: 6803       ldr   r3, [r0, #0]
    //   8000008: 2b00       cmp   r3, #0
    //   800000a: d004       beq.n 8000016 <good>
    //   800000c: 2b01       cmp   r3, #1
    //   800000e: d000       beq.n 8000012 <tail>
    //   8000010: 2104       movs  r1, #4
    // tail:
    //   8000012: 2105       movs  r1, #5
    //   8000014: 4710       bx    r2
    // good:
    //   8000016: f000 f800  bl    800001a <main>
    // main:
    //   800001a: e7fe       b.n   800001a
    let code = [
        0x2101, 0xf04f, 0x5000, 0x6803, 0x2b00, 0xd004, 0x2b01, 0xd000, 0x2104, 0x2105, 0x4710,
        0xf000, 0xf800, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_001a];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_001a)),
        [
            "0 0x800000a back to 0x8000016, since Branches { site: 8000014, through: Register(2) }",
            "0 0x8000016 call 0x800001a (0x20000000 0x1 ? ?)",
            "end Main(8000016)",
        ]
    );
}

#[test]
fn the_stack_stays_as_low_as_a_way_the_walk_gave_up_took_it() {
    // The walk takes the way past the beq first, which moves sp down by
    // 64 bytes and spins, then goes back to the beq. The lowest the stack
    // pointer has been stays where that way took it, so the store at
    // 0x20000fe0, 32 bytes below the top, counts as one to the stack and
    // isn't listed:
    //   8000000: f04f 5000  mov.w r0, #0x20000000
    //   8000004: 6801       ldr   r1, [r0, #0]
    //   8000006: 2900       cmp   r1, #0
    //   8000008: d001       beq.n 800000e
    //   800000a: b090       sub   sp, #64
    //   800000c: e7fe       b.n   800000c
    //   800000e: 4a02       ldr   r2, [pc, #8]  ; 0x20000fe0
    //   8000010: 6010       str   r0, [r2, #0]
    //   8000012: f000 f803  bl    800001c <main>
    //   8000016: bf00       nop
    //   8000018: 20000fe0   .word
    // main:
    //   800001c: e7fe       b.n   800001c
    let code = |sub: u16| {
        vec![
            0xf04f, 0x5000, 0x6801, 0x2900, 0xd001, sub, 0xe7fe, 0x4a02, 0x6010, 0xf000, 0xf803,
            0xbf00, 0x0fe0, 0x2000, 0xe7fe,
        ]
    };
    let functions = [0x0800_0000, 0x0800_001c];
    assert_eq!(
        walk_lines(&code(0xb090), &functions, Some(0x0800_001c)),
        [
            "0 0x8000008 back to 0x800000e, since Spins(800000c)",
            "0 0x8000012 call 0x800001c (0x20000000 ? 0x20000fe0 ?)",
            "end Main(8000012)",
        ]
    );
    // With a nop in place of the sub, the store is listed.
    assert_eq!(
        walk_lines(&code(0xbf00), &functions, Some(0x0800_001c)),
        [
            "0 0x8000008 back to 0x800000e, since Spins(800000c)",
            "0 0x8000010 store 0x20000000 at 0x20000fe0, 4 bytes",
            "0 0x8000012 call 0x800001c (0x20000000 ? 0x20000fe0 ?)",
            "end Main(8000012)",
        ]
    );
}

#[test]
fn the_flags_that_compares_and_adds_set_and_the_conditions_they_meet() {
    // For each condition, in the order eq, ne, cs, cc, mi, pl, vs, vc,
    // hi, ls, ge, lt, gt, le and al: T where it holds, F where it
    // doesn't, ? where the walk can't tell.
    let conditions = |flags: Flags| -> String {
        (0..15)
            .map(|condition| match holds(condition, flags) {
                Some(true) => 'T',
                Some(false) => 'F',
                None => '?',
            })
            .collect()
    };
    let one = Some(1);
    assert_eq!(conditions(subtract(one, Some(2))), "FTFTTFFTFTFTFTT");
    assert_eq!(conditions(subtract(Some(2), Some(2))), "TFTFFTFTFTTFFTT");
    assert_eq!(conditions(subtract(Some(3), one)), "FTTFFTFTTFTFTFT");
    // The smallest signed number less 1 overflows.
    assert_eq!(conditions(subtract(Some(1 << 31), one)), "FTTFFTTFTFFTFTT");
    assert_eq!(conditions(add(Some(u32::MAX), one)), "TFTFFTFTFTTFFTT");
    assert_eq!(conditions(add(Some(0x7fff_ffff), one)), "FTFTTFTFFTTFTFT");
    assert_eq!(conditions(subtract(None, one)), "??????????????T");
    // A condition of two flags fails when either does, if the walk
    // can't tell the other.
    let flags = Flags {
        carry: Some(false),
        ..Flags::default()
    };
    assert_eq!(conditions(flags), "??FT????FT????T");
    let flags = Flags {
        zero: Some(true),
        ..Flags::default()
    };
    assert_eq!(conditions(flags), "TF??????FT??FTT");
}

#[test]
fn subs_sets_the_flags_that_a_branch_reads() {
    //   8000000: 2102       movs  r1, #2
    //   8000002: 3901       subs  r1, #1
    //   8000004: d100       bne.n 8000008
    //   8000006: e7fe       b.n   8000006
    //   8000008: f000 f800  bl    800000c <main>
    // main:
    //   800000c: e7fe       b.n   800000c
    let code = [0x2102, 0x3901, 0xd100, 0xe7fe, 0xf000, 0xf800, 0xe7fe];
    let functions = [0x0800_0000, 0x0800_000c];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_000c)),
        [
            "0 0x8000008 call 0x800000c (? 0x1 ? ?)",
            "end Main(8000008)"
        ]
    );
}

#[test]
fn numbers_from_movw_and_movt_and_shifts() {
    //   8000000: f241 2034  movw  r0, #0x1234
    //   8000004: f2c2 0000  movt  r0, #0x2000
    //   8000008: 2384       movs  r3, #0x84
    //   800000a: 085b       lsrs  r3, r3, #1
    //   800000c: 2100       movs  r1, #0
    //   800000e: 3908       subs  r1, #8
    //   8000010: 1089       asrs  r1, r1, #2
    //   8000012: 091a       lsrs  r2, r3, #4
    //   8000014: f000 f800  bl    8000018 <main>
    // main:
    //   8000018: e7fe       b.n   8000018
    let code = [
        0xf241, 0x2034, 0xf2c2, 0x0000, 0x2384, 0x085b, 0x2100, 0x3908, 0x1089, 0x091a, 0xf000,
        0xf800, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0018];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0018)),
        [
            "0 0x8000014 call 0x8000018 (0x20001234 0xfffffffe 0x4 0x42)",
            "end Main(8000014)",
        ]
    );
}

#[test]
fn calls_and_jumps_that_leave_the_file_s_code() {
    //   8000000: 4801       ldr   r0, [pc, #4]  ; 0x10000001
    //   8000002: 4780       blx   r0
    //   8000004: 4901       ldr   r1, [pc, #4]  ; 0x10000101
    //   8000006: 4708       bx    r1
    //   8000008: 10000001   .word
    //   800000c: 10000101   .word
    let code = [
        0x4801, 0x4780, 0x4901, 0x4708, 0x0001, 0x1000, 0x0101, 0x1000,
    ];
    assert_eq!(
        walk_lines(&code, &[FLASH], None),
        [
            "0 0x8000002 call through r0 0x10000000 (0x10000001 ? ? ?)",
            "1 0x8000002 stop: LeavesFile { site: 8000002, target: 10000000 }",
            "end LeavesFile { site: 8000006, target: 10000100 }",
        ]
    );
}

#[test]
fn a_loop_with_no_way_out_and_code_the_walk_gets_back_to() {
    //   8000000: f04f 5000  mov.w r0, #0x20000000
    //   8000004: 6801       ldr   r1, [r0, #0]
    //   8000006: 6041       str   r1, [r0, #4]
    //   8000008: e7fc       b.n   8000004
    let code = [0xf04f, 0x5000, 0x6801, 0x6041, 0xe7fc];
    assert_eq!(
        walk_lines(&code, &[FLASH], None),
        [
            "0 0x8000006 store ? at 0x20000004, 4 bytes, loaded from 0x20000000 at 0x8000004",
            "0 0x8000004 loop back at 0x8000008, from event 0",
            "end Loops(8000008)",
        ]
    );
    //   8000000: e000       b.n   8000004
    //   8000002: 2101       movs  r1, #1
    //   8000004: 2000       movs  r0, #0
    //   8000006: e7fc       b.n   8000002
    let code = [0xe000, 0x2101, 0x2000, 0xe7fc];
    assert_eq!(walk_lines(&code, &[FLASH], None), ["end Rejoins(8000004)"]);
}

#[test]
fn a_veneer_that_loads_the_address_it_goes_to_into_pc() {
    //   8000000: 2003       movs  r0, #3
    //   8000002: f000 f803  bl    800000c <veneer>
    //   8000006: f000 f807  bl    8000018 <main>
    //   800000a: bf00       nop
    // veneer:
    //   800000c: f8df f000  ldr.w pc, [pc, #0]  ; 0x8000015
    //   8000010: 08000015   .word
    // func:
    //   8000014: 2104       movs  r1, #4
    //   8000016: 4770       bx    lr
    // main:
    //   8000018: e7fe       b.n   8000018
    let code = [
        0x2003, 0xf000, 0xf803, 0xf000, 0xf807, 0xbf00, 0xf8df, 0xf000, 0x0015, 0x0800, 0x2104,
        0x4770, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_000c, 0x0800_0014, 0x0800_0018];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0018)),
        [
            "0 0x8000002 call 0x800000c (0x3 ? ? ?)",
            "1 0x800000c tail call 0x8000014 (0x3 ? ? ?)",
            "0 0x8000006 call 0x8000018 (0x3 0x4 ? ?), left by a call: r1",
            "end Main(8000006)",
        ]
    );
}

#[test]
fn an_and_in_an_it_block_doesnt_set_the_flags() {
    // So bne reads the flags of the cmp:
    //   8000000: 2001       movs  r0, #1
    //   8000002: 2102       movs  r1, #2
    //   8000004: 2200       movs  r2, #0
    //   8000006: 4288       cmp   r0, r1
    //   8000008: bf38       it    cc
    //   800000a: 400a       andcc r2, r1
    //   800000c: d100       bne.n 8000010
    //   800000e: e7fe       b.n   800000e
    //   8000010: f000 f800  bl    8000014 <main>
    // main:
    //   8000014: e7fe       b.n   8000014
    let code = [
        0x2001, 0x2102, 0x2200, 0x4288, 0xbf38, 0x400a, 0xd100, 0xe7fe, 0xf000, 0xf800, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0014];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0014)),
        [
            "0 0x8000010 call 0x8000014 (0x1 0x2 0x0 ?)",
            "end Main(8000010)"
        ]
    );
}

#[test]
fn a_loop_that_copies_two_words_at_a_time_with_ldm_and_stm() {
    // The loop runs twice, and leaves r0 and r2 past what it copied:
    //   8000000: f04f 5000  mov.w r0, #0x20000000
    //   8000004: 4903       ldr   r1, [pc, #12]  ; 0x20000010
    //   8000006: 4a04       ldr   r2, [pc, #16]  ; 0x8000100
    //   8000008: ca18       ldmia r2!, {r3, r4}
    //   800000a: c018       stmia r0!, {r3, r4}
    //   800000c: 4288       cmp   r0, r1
    //   800000e: d3fb       bcc.n 8000008
    //   8000010: f000 f804  bl    800001c <main>
    //   8000014: 20000010   .word
    //   8000018: 08000100   .word
    // main:
    //   800001c: e7fe       b.n   800001c
    let code = [
        0xf04f, 0x5000, 0x4903, 0x4a04, 0xca18, 0xc018, 0x4288, 0xd3fb, 0xf000, 0xf804, 0x0010,
        0x2000, 0x0100, 0x0800, 0xe7fe, 0xbf00,
    ];
    let functions = [0x0800_0000, 0x0800_001c];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_001c)),
        [
            "0 0x800000a store ? at 0x20000000, 8 bytes, loaded from 0x8000100 at 0x8000008",
            "0 0x8000008 loop back at 0x800000e, from event 0, runs 0x2 times",
            "0 0x8000010 call 0x800001c (0x20000010 0x20000010 0x8000110 ?)",
            "end Main(8000010)",
        ]
    );
    assert_eq!(
        loop_stores(&code, &functions, Some(0x0800_001c)),
        ["0x800000a: 8 bytes at 0x20000000 +8n, 2 times, of ?, from 0x8000100 +8n, loaded"]
    );
}

#[test]
fn a_loop_that_counts_down_to_zero() {
    // r3 counts the bytes left, from 16 down to 0, and the loop copies
    // the last word first:
    //   8000000: 4904       ldr   r1, [pc, #16]  ; 0x8000100
    //   8000002: f04f 5200  mov.w r2, #0x20000000
    //   8000006: 2310       movs  r3, #16
    //   8000008: 3b04       subs  r3, #4
    //   800000a: 58c8       ldr   r0, [r1, r3]
    //   800000c: 50d0       str   r0, [r2, r3]
    //   800000e: dcfb       bgt.n 8000008
    //   8000010: f000 f802  bl    8000018 <main>
    //   8000014: 08000100   .word
    // main:
    //   8000018: e7fe       b.n   8000018
    let code = [
        0x4904, 0xf04f, 0x5200, 0x2310, 0x3b04, 0x58c8, 0x50d0, 0xdcfb, 0xf000, 0xf802, 0x0100,
        0x0800, 0xe7fe, 0xbf00,
    ];
    let functions = [0x0800_0000, 0x0800_0018];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0018)),
        [
            "0 0x800000c store ? at 0x2000000c, 4 bytes, loaded from 0x800010c at 0x800000a",
            "0 0x8000008 loop back at 0x800000e, from event 0, runs 0x4 times",
            "0 0x8000010 call 0x8000018 (? 0x8000100 0x20000000 0x0)",
            "end Main(8000010)",
        ]
    );
    assert_eq!(
        loop_stores(&code, &functions, Some(0x0800_0018)),
        ["0x800000c: 4 bytes at 0x2000000c -4n, 4 times, of ?, from 0x800010c -4n, loaded"]
    );
}

#[test]
fn two_loops_that_share_the_register_they_copy_from() {
    // The second loop copies from where the first left r1, as startup
    // code does when one load image follows the other:
    //   8000000: 490a       ldr   r1, [pc, #40]  ; 0x8000100
    //   8000002: f04f 5200  mov.w r2, #0x20000000
    //   8000006: 4b0a       ldr   r3, [pc, #40]  ; 0x20000008
    //   8000008: 429a       cmp   r2, r3
    //   800000a: bfbe       ittt  lt
    //   800000c: f851 0b04  ldrlt.w r0, [r1], #4
    //   8000010: f842 0b04  strlt.w r0, [r2], #4
    //   8000014: e7f8       blt.n 8000008
    //   8000016: 4a07       ldr   r2, [pc, #28]  ; 0x20000100
    //   8000018: 4b07       ldr   r3, [pc, #28]  ; 0x2000010c
    //   800001a: 429a       cmp   r2, r3
    //   800001c: bfbe       ittt  lt
    //   800001e: f851 0b04  ldrlt.w r0, [r1], #4
    //   8000022: f842 0b04  strlt.w r0, [r2], #4
    //   8000026: e7f8       blt.n 800001a
    //   8000028: f000 f808  bl    800003c <main>
    //   800002c: 08000100 20000008 20000100 2000010c   .word
    // main:
    //   800003c: e7fe       b.n   800003c
    let code = [
        0x490a, 0xf04f, 0x5200, 0x4b0a, 0x429a, 0xbfbe, 0xf851, 0x0b04, 0xf842, 0x0b04, 0xe7f8,
        0x4a07, 0x4b07, 0x429a, 0xbfbe, 0xf851, 0x0b04, 0xf842, 0x0b04, 0xe7f8, 0xf000, 0xf808,
        0x0100, 0x0800, 0x0008, 0x2000, 0x0100, 0x2000, 0x010c, 0x2000, 0xe7fe, 0xbf00,
    ];
    let functions = [0x0800_0000, 0x0800_003c];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_003c)),
        [
            "0 0x8000010 store ? at 0x20000000, 4 bytes, loaded from 0x8000100 at 0x800000c",
            "0 0x8000008 loop back at 0x8000014, from event 0, runs 0x3 times",
            "0 0x8000022 store ? at 0x20000100, 4 bytes, loaded from 0x8000108 at 0x800001e",
            "0 0x800001a loop back at 0x8000026, from event 2, runs 0x4 times",
            "0 0x8000028 call 0x800003c (? 0x8000114 0x2000010c 0x2000010c)",
            "end Main(8000028)",
        ]
    );
}

#[test]
fn a_loop_that_runs_too_many_times_to_count() {
    // r0 counts up to 0xffffffff, more times than elfq counts, so the
    // walk can't tell what the loop leaves in it:
    //   8000000: f04f 5200  mov.w r2, #0x20000000
    //   8000004: 2000       movs  r0, #0
    //   8000006: f04f 31ff  mov.w r1, #0xffffffff
    //   800000a: 6010       str   r0, [r2, #0]
    //   800000c: 3001       adds  r0, #1
    //   800000e: 4288       cmp   r0, r1
    //   8000010: d1fb       bne.n 800000a
    //   8000012: f000 f800  bl    8000016 <main>
    // main:
    //   8000016: e7fe       b.n   8000016
    let code = [
        0xf04f, 0x5200, 0x2000, 0xf04f, 0x31ff, 0x6010, 0x3001, 0x4288, 0xd1fb, 0xf000, 0xf800,
        0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0016];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0016)),
        [
            "0 0x800000a store 0x0 at 0x20000000, 4 bytes",
            "0 0x800000a loop back at 0x8000010, from event 0",
            "0 0x8000012 call 0x8000016 (? 0xffffffff 0x20000000 ?)",
            "end Main(8000012)",
        ]
    );
}

#[test]
fn a_table_loop_whose_test_is_past_its_branch_back() {
    // As CMSIS 5.9's __cmsis_start goes through its copy table, built
    // with GCC -Os, which puts the test of the loop over the table's
    // rows after the branch back: the table has one row, which copies
    // 4 words, and the loop leaves in the second iteration. The walk
    // goes through the loop's code each iteration, and the first reads
    // the row from its start, though the code reads the row's last word
    // first:
    // reset:
    //   8000000: 4b0a       ldr	r3, [pc, #40]	@ (800002c <body+0x6>)
    //   8000002: 490b       ldr	r1, [pc, #44]	@ (8000030 <body+0xa>)
    //   8000004: b510       push	{r4, lr}
    // outer:
    //   8000006: 428b       cmp	r3, r1
    //   8000008: d30d       bcc.n	8000026 <body>
    //   800000a: f000 f819  bl	8000040 <main>
    // inner:
    //   800000e: 6818       ldr	r0, [r3, #0]
    //   8000010: f850 4022  ldr.w	r4, [r0, r2, lsl #2]
    //   8000014: 6858       ldr	r0, [r3, #4]
    //   8000016: f840 4022  str.w	r4, [r0, r2, lsl #2]
    //   800001a: 3201       adds	r2, #1
    // test:
    //   800001c: 6898       ldr	r0, [r3, #8]
    //   800001e: 4290       cmp	r0, r2
    //   8000020: d8f5       bhi.n	800000e <inner>
    //   8000022: 330c       adds	r3, #12
    //   8000024: e7ef       b.n	8000006 <outer>
    // body:
    //   8000026: 2200       movs	r2, #0
    //   8000028: e7f8       b.n	800001c <test>
    //   800002c: 08000034   .word	0x08000034
    //   8000030: 08000040   .word	0x08000040
    // table:
    //   8000034: 08000100   .word	0x08000100
    //   8000038: 20000000   .word	0x20000000
    //   800003c: 00000004   .word	0x00000004
    // main:
    //   8000040: e7fe       b.n	8000040 <main>
    let code = [
        0x4b0a, 0x490b, 0xb510, 0x428b, 0xd30d, 0xf000, 0xf819, 0x6818, 0xf850, 0x4022, 0x6858,
        0xf840, 0x4022, 0x3201, 0x6898, 0x4290, 0xd8f5, 0x330c, 0xe7ef, 0x2200, 0xe7f8, 0xbf00,
        0x0034, 0x0800, 0x0040, 0x0800, 0x0100, 0x0800, 0x0000, 0x2000, 0x0004, 0x0000, 0xe7fe,
        0xbf00,
    ];
    let functions = [0x0800_0000, 0x0800_0040];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0040)),
        [
            "0 0x8000016 store ? at 0x20000000, 4 bytes, loaded from 0x8000100 at 0x8000010",
            "0 0x800000e loop back at 0x8000020, from event 0, runs 0x4 times",
            "0 0x8000006 loop back at 0x8000024, from event 0, runs 0x2 times, times from events 0@0x8000034 2",
            "0 0x800000a call 0x8000040 (0x4 0x8000040 0x4 0x8000040)",
            "end Main(800000a)",
        ]
    );
    assert_eq!(
        loop_stores(&code, &functions, Some(0x0800_0040)),
        ["0x8000016: 4 bytes at 0x20000000 +4n, 4 times, of ?, from 0x8000100 +4n, loaded"]
    );
}

#[test]
fn a_loop_over_a_table_of_functions_is_walked_once() {
    // As __libc_init_array calls the constructors in .init_array: the
    // loop calls another function through r3 each iteration, so the
    // walk keeps what it found in the first, as for a loop it doesn't
    // go through each iteration, and doesn't follow the second
    // function:
    // reset:
    //   8000000: 4c08       ldr	r4, [pc, #32]	@ (8000024 <second+0x8>)
    //   8000002: 4d09       ldr	r5, [pc, #36]	@ (8000028 <second+0xc>)
    // loop:
    //   8000004: 42ac       cmp	r4, r5
    //   8000006: d202       bcs.n	800000e <done>
    //   8000008: cc08       ldmia	r4!, {r3}
    //   800000a: 4798       blx	r3
    //   800000c: e7fa       b.n	8000004 <loop>
    // done:
    //   800000e: f000 f813  bl	8000038 <main>
    // first:
    //   8000012: f04f 5000  mov.w	r0, #536870912	@ 0x20000000
    //   8000016: 2101       movs	r1, #1
    //   8000018: 6001       str	r1, [r0, #0]
    //   800001a: 4770       bx	lr
    // second:
    //   800001c: 4803       ldr	r0, [pc, #12]	@ (800002c <second+0x10>)
    //   800001e: 2102       movs	r1, #2
    //   8000020: 6001       str	r1, [r0, #0]
    //   8000022: 4770       bx	lr
    //   8000024: 08000030   .word	0x08000030
    //   8000028: 08000038   .word	0x08000038
    //   800002c: 20000004   .word	0x20000004
    // table:
    //   8000030: 08000013   .word	0x08000013
    //   8000034: 0800001d   .word	0x0800001d
    // main:
    //   8000038: e7fe       b.n	8000038 <main>
    let code = [
        0x4c08, 0x4d09, 0x42ac, 0xd202, 0xcc08, 0x4798, 0xe7fa, 0xf000, 0xf813, 0xf04f, 0x5000,
        0x2101, 0x6001, 0x4770, 0x4803, 0x2102, 0x6001, 0x4770, 0x0030, 0x0800, 0x0038, 0x0800,
        0x0004, 0x2000, 0x0013, 0x0800, 0x001d, 0x0800, 0xe7fe, 0xbf00,
    ];
    let functions = [0x0800_0000, 0x0800_0012, 0x0800_001c, 0x0800_0038];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0038)),
        [
            "0 0x800000a call through r3 0x8000012 (? ? ? 0x8000013)",
            "1 0x8000018 store 0x1 at 0x20000000, 4 bytes",
            "0 0x8000004 loop back at 0x800000c, from event 0, runs 0x3 times",
            "0 0x800000e call 0x8000038 (? ? ? ?)",
            "end Main(800000e)",
        ]
    );
}

#[test]
fn a_loop_that_leaves_with_cbz_at_its_head() {
    // r3 counts the bytes left down to 0, which `cbz` tests before the
    // loop copies each word:
    // reset:
    //   8000000: 4906       ldr	r1, [pc, #24]	@ (800001c <done+0x6>)
    //   8000002: f04f 5200  mov.w	r2, #536870912	@ 0x20000000
    //   8000006: 2310       movs	r3, #16
    // loop:
    //   8000008: b12b       cbz	r3, 8000016 <done>
    //   800000a: f851 0b04  ldr.w	r0, [r1], #4
    //   800000e: f842 0b04  str.w	r0, [r2], #4
    //   8000012: 3b04       subs	r3, #4
    //   8000014: e7f8       b.n	8000008 <loop>
    // done:
    //   8000016: f000 f803  bl	8000020 <main>
    //   800001c: 08000100   .word	0x08000100
    // main:
    //   8000020: e7fe       b.n	8000020 <main>
    let code = [
        0x4906, 0xf04f, 0x5200, 0x2310, 0xb12b, 0xf851, 0x0b04, 0xf842, 0x0b04, 0x3b04, 0xe7f8,
        0xf000, 0xf803, 0xbf00, 0x0100, 0x0800, 0xe7fe, 0xbf00,
    ];
    let functions = [0x0800_0000, 0x0800_0020];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0020)),
        [
            "0 0x800000e store ? at 0x20000000, 4 bytes, loaded from 0x8000100 at 0x800000a",
            "0 0x8000008 loop back at 0x8000014, from event 0, runs 0x5 times",
            "0 0x8000016 call 0x8000020 (? 0x8000110 0x20000010 0x0)",
            "end Main(8000016)",
        ]
    );
    assert_eq!(
        loop_stores(&code, &functions, Some(0x0800_0020)),
        ["0x800000e: 4 bytes at 0x20000000 +4n, 4 times, of ?, from 0x8000100 +4n, loaded"]
    );
}

#[test]
fn a_loop_counts_without_the_words_it_loads() {
    // The words the loop copies run 1, 2, 3, 4, then 9: the walk
    // doesn't take the word in r0 to move on by 1 each iteration, as the
    // registers that count do:
    // reset:
    //   8000000: a106       add	r1, pc, #24	@ (adr r1, 800001c <data>)
    //   8000002: f04f 5200  mov.w	r2, #536870912	@ 0x20000000
    //   8000006: 4b04       ldr	r3, [pc, #16]	@ (8000018 <loop+0x10>)
    // loop:
    //   8000008: f851 0b04  ldr.w	r0, [r1], #4
    //   800000c: f842 0b04  str.w	r0, [r2], #4
    //   8000010: 429a       cmp	r2, r3
    //   8000012: d3f9       bcc.n	8000008 <loop>
    //   8000014: f000 f80e  bl	8000034 <main>
    //   8000018: 20000018   .word	0x20000018
    // data:
    //   800001c: 00000001   .word	0x00000001
    //   8000020: 00000002   .word	0x00000002
    //   8000024: 00000003   .word	0x00000003
    //   8000028: 00000004   .word	0x00000004
    //   800002c: 00000009   .word	0x00000009
    //   8000030: 00000006   .word	0x00000006
    // main:
    //   8000034: e7fe       b.n	8000034 <main>
    let code = [
        0xa106, 0xf04f, 0x5200, 0x4b04, 0xf851, 0x0b04, 0xf842, 0x0b04, 0x429a, 0xd3f9, 0xf000,
        0xf80e, 0x0018, 0x2000, 0x0001, 0x0000, 0x0002, 0x0000, 0x0003, 0x0000, 0x0004, 0x0000,
        0x0009, 0x0000, 0x0006, 0x0000, 0xe7fe, 0xbf00,
    ];
    let functions = [0x0800_0000, 0x0800_0034];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0034)),
        [
            "0 0x800000c store 0x1 at 0x20000000, 4 bytes, loaded from 0x800001c at 0x8000008",
            "0 0x8000008 loop back at 0x8000012, from event 0, runs 0x6 times",
            "0 0x8000014 call 0x8000034 (0x6 0x8000034 0x20000018 0x20000018)",
            "end Main(8000014)",
        ]
    );
    assert_eq!(
        loop_stores(&code, &functions, Some(0x0800_0034)),
        ["0x800000c: 4 bytes at 0x20000000 +4n, 6 times, of ?, from 0x800001c +4n, loaded"]
    );
}

#[test]
fn numbers_from_adds_of_shifted_registers_and_negs() {
    // reset:
    //   8000000: 2103       movs	r1, #3
    //   8000002: f04f 5200  mov.w	r2, #536870912	@ 0x20000000
    //   8000006: eb02 0081  add.w	r0, r2, r1, lsl #2
    //   800000a: 424b       negs	r3, r1
    //   800000c: 6003       str	r3, [r0, #0]
    //   800000e: eba0 0081  sub.w	r0, r0, r1, lsl #2
    //   8000012: 6001       str	r1, [r0, #0]
    //   8000014: f000 f800  bl	8000018 <main>
    // main:
    //   8000018: e7fe       b.n	8000018 <main>
    let code = [
        0x2103, 0xf04f, 0x5200, 0xeb02, 0x0081, 0x424b, 0x6003, 0xeba0, 0x0081, 0x6001, 0xf000,
        0xf800, 0xe7fe,
    ];
    let functions = [0x0800_0000, 0x0800_0018];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_0018)),
        [
            "0 0x800000c store 0xfffffffd at 0x2000000c, 4 bytes",
            "0 0x8000012 store 0x3 at 0x20000000, 4 bytes",
            "0 0x8000014 call 0x8000018 (0x20000000 0x3 0x20000000 0xfffffffd)",
            "end Main(8000014)",
        ]
    );
}

#[test]
fn a_store_of_several_words_copies_only_words_loaded_one_after_another() {
    //   8000000: 4905       ldr   r1, [pc, #20]  ; 0x8000100
    //   8000002: f04f 5000  mov.w r0, #0x20000000
    //   8000006: 680b       ldr   r3, [r1, #0]
    //   8000008: 688c       ldr   r4, [r1, #8]
    //   800000a: c018       stmia r0!, {r3, r4}
    //   800000c: 684b       ldr   r3, [r1, #4]
    //   800000e: 688c       ldr   r4, [r1, #8]
    //   8000010: c018       stmia r0!, {r3, r4}
    //   8000012: f000 f803  bl    800001c <main>
    //   8000018: 08000100   .word
    // main:
    //   800001c: e7fe       b.n   800001c
    let code = [
        0x4905, 0xf04f, 0x5000, 0x680b, 0x688c, 0xc018, 0x684b, 0x688c, 0xc018, 0xf000, 0xf803,
        0xbf00, 0x0100, 0x0800, 0xe7fe, 0xbf00,
    ];
    let functions = [0x0800_0000, 0x0800_001c];
    assert_eq!(
        walk_lines(&code, &functions, Some(0x0800_001c)),
        [
            "0 0x800000a store ? at 0x20000000, 8 bytes",
            "0 0x8000010 store ? at 0x20000008, 8 bytes, loaded from 0x8000104 at 0x800000c",
            "0 0x8000012 call 0x800001c (0x20000010 0x8000100 ? ?)",
            "end Main(8000012)",
        ]
    );
}

/// Flash holding `units`, as `walk_units` lays them out, as one function.
fn flash(units: &[u16]) -> Flash {
    let code: Vec<u8> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
    let end = FLASH + code.len() as u64;
    Flash {
        functions: vec![(FLASH, end)],
        code,
    }
}

#[test]
fn a_call_passes_its_result_on_through_copies_of_r0() {
    //   8000000: f000 f804  bl    800000c
    //   8000004: 4603       mov   r3, r0
    //   8000006: 4618       mov   r0, r3
    //   8000008: f000 f802  bl    8000010
    let code = flash(&[0xf000, 0xf804, 0x4603, 0x4618, 0xf000, 0xf802]);
    assert!(passes_result(&code, FLASH, FLASH + 8));
    // Nothing between the calls passes it on too.
    let code = flash(&[0xf000, 0xf804, 0xf000, 0xf802]);
    assert!(passes_result(&code, FLASH, FLASH + 4));
    // Loading a word from a literal pool into another register keeps it.
    //   8000004: 4900       ldr   r1, [pc, #0]
    let code = flash(&[0xf000, 0xf804, 0x4900, 0xf000, 0xf802]);
    assert!(passes_result(&code, FLASH, FLASH + 6));
}

#[test]
fn a_call_doesnt_pass_its_result_when_r0_changes() {
    //   8000004: 4620       mov   r0, r4
    let code = flash(&[0xf000, 0xf804, 0x4620, 0xf000, 0xf802]);
    assert!(!passes_result(&code, FLASH, FLASH + 6));
    //   8000004: 4800       ldr   r0, [pc, #0]
    let code = flash(&[0xf000, 0xf804, 0x4800, 0xf000, 0xf802]);
    assert!(!passes_result(&code, FLASH, FLASH + 6));
    // Code that does anything but copy registers and load literals.
    //   8000004: 3001       adds  r0, #1
    let code = flash(&[0xf000, 0xf804, 0x3001, 0xf000, 0xf802]);
    assert!(!passes_result(&code, FLASH, FLASH + 6));
    // Calls more than 16 bytes apart, or in the wrong order.
    let mut units = vec![0xf000, 0xf804];
    units.extend([0x4603; 9]);
    units.extend([0xf000, 0xf802]);
    assert!(!passes_result(&flash(&units), FLASH, FLASH + 22));
    let code = flash(&[0xf000, 0xf804, 0xf000, 0xf802]);
    assert!(!passes_result(&code, FLASH + 4, FLASH));
    // A second call that isn't at an instruction's start.
    let code = flash(&[0xf000, 0xf804, 0x4603, 0xf000, 0xf802]);
    assert!(!passes_result(&code, FLASH, FLASH + 5));
}

#[test]
fn a_branch_past_a_call_says_that_it_doesnt_return() {
    //   8000000: e001       b.n   8000006
    //   8000002: f000 f802  bl    800000a
    //   8000006: 4770       bx    lr
    let code = flash(&[0xe001, 0xf000, 0xf802, 0x4770]);
    assert!(branches_past(&code, FLASH, FLASH + 2));
    // A conditional branch counts too.
    //   8000000: d001       beq.n 8000006
    let code = flash(&[0xd001, 0xf000, 0xf802, 0x4770]);
    assert!(branches_past(&code, FLASH, FLASH + 2));
    // One to elsewhere doesn't, nor one before `start`.
    //   8000000: e002       b.n   8000008
    let code = flash(&[0xe002, 0xf000, 0xf802, 0x4770, 0x4770]);
    assert!(!branches_past(&code, FLASH, FLASH + 2));
    let code = flash(&[0xe001, 0xf000, 0xf802, 0x4770]);
    assert!(!branches_past(&code, FLASH + 2, FLASH + 2));
}
